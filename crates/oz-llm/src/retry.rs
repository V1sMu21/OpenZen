use std::future::Future;
use std::pin::Pin;
use std::time::Duration;

use oz_core_types::{LlmError, Message};
use oz_config::SessionConfig;

type AsyncFn<T> = Pin<Box<dyn Future<Output = Result<T, LlmError>> + Send>>;

/// Retry with exponential backoff — matches Python _stream_with_retry
pub async fn retry_with_backoff<T, F>(
    operation: F,
    config: &SessionConfig,
) -> Result<T, LlmError>
where
    F: FnMut() -> AsyncFn<T>,
    T: Send + 'static,
{
    let max_retries = config.max_retries.unwrap_or(4) as usize;
    let mut op = operation;

    for attempt in 0..=max_retries {
        let future = op();
        let result = future.await;

        match result {
            Ok(val) => return Ok(val),
            Err(e) if e.is_retryable() && attempt < max_retries => {
                let delay = compute_delay(attempt, config.timeout);
                tracing::warn!(
                    "[LLM Retry] {e}, retry in {delay:.1}s ({}/{})",
                    attempt + 1, max_retries + 1
                );
                tokio::time::sleep(Duration::from_secs_f64(delay)).await;
            }
            Err(e) => return Err(e),
        }
    }

    Err(LlmError::MaxRetriesExceeded("All retry attempts failed".into()))
}

/// Backoff delay for retry `attempt` (0-based): 1.5s × 2^attempt, capped at
/// 30s (or the configured request timeout, whichever is lower).
pub fn compute_delay(attempt: usize, timeout: Option<u64>) -> f64 {
    let delay = 1.5 * (2u64.pow(attempt as u32) as f64);
    // Cap backoff at the configured request timeout (default 30s) so a
    // retry never waits longer than a full request may take.
    let cap = timeout.unwrap_or(30).min(30) as f64;
    delay.min(cap)
}

pub fn trim_history(history: &mut Vec<Message>, context_win: usize) {
    let cost = estimate_chars(history);
    if cost <= context_win * 3 {
        return;
    }
    let target = (context_win as f64 * 3.0 * 0.6) as usize;
    while history.len() > 5 && estimate_chars(history) > target {
        history.remove(0);
        while !history.is_empty() && history[0].role != oz_core_types::Role::User {
            history.remove(0);
        }
        if let Some(first) = history.first_mut() {
            sanitize_leading_user_msg(first);
        }
    }
}

fn estimate_chars(messages: &[Message]) -> usize {
    messages.iter().map(|m| {
        m.content.iter().map(|b| match b {
            oz_core_types::ContentBlock::Text { text, .. } => text.len(),
            oz_core_types::ContentBlock::ToolUse { name, input, .. } => {
                name.len() + serde_json::to_string(input).unwrap_or_default().len()
            }
            oz_core_types::ContentBlock::ToolResult { content, .. } => {
                match content {
                    oz_core_types::ContentContainer::Text(t) => t.len(),
                    oz_core_types::ContentContainer::Blocks(bs) => {
                        bs.iter().map(|b| match b {
                            oz_core_types::ContentBlock::Text { text, .. } => text.len(),
                            _ => 0,
                        }).sum()
                    }
                }
            }
            _ => 0,
        }).sum::<usize>()
    }).sum()
}

fn sanitize_leading_user_msg(msg: &mut Message) {
    let texts: Vec<String> = msg.content.iter().filter_map(|b| match b {
        oz_core_types::ContentBlock::Text { text, .. } => Some(text.clone()),
        oz_core_types::ContentBlock::ToolResult { content, .. } => {
            match content {
                oz_core_types::ContentContainer::Text(t) => Some(t.clone()),
                oz_core_types::ContentContainer::Blocks(bs) => {
                    Some(bs.iter().filter_map(|b| match b {
                        oz_core_types::ContentBlock::Text { text, .. } => Some(text.clone()),
                        _ => None,
                    }).collect::<Vec<_>>().join("\n"))
                }
            }
        }
        _ => None,
    }).collect();
    msg.content = vec![oz_core_types::ContentBlock::text(texts.join("\n"))];
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_compute_delay_0() {
        let delay = compute_delay(0, None);
        assert!((delay - 1.5).abs() < f64::EPSILON);
    }

    #[test]
    fn test_compute_delay_1() {
        let delay = compute_delay(1, None);
        assert!((delay - 3.0).abs() < f64::EPSILON);
    }

    #[test]
    fn test_compute_delay_2() {
        let delay = compute_delay(2, None);
        assert!((delay - 6.0).abs() < f64::EPSILON);
    }

    #[test]
    fn test_compute_delay_capped() {
        let delay = compute_delay(10, None);
        assert!((delay - 30.0).abs() < f64::EPSILON);
    }

    #[test]
    fn test_compute_delay_respects_timeout_cap() {
        // A tight request timeout must shrink the backoff cap below 30s.
        let delay = compute_delay(10, Some(5));
        assert!((delay - 5.0).abs() < f64::EPSILON);
        // A generous timeout (e.g. default 120s) must not raise the 30s cap.
        let delay = compute_delay(10, Some(120));
        assert!((delay - 30.0).abs() < f64::EPSILON);
    }

    #[test]
    fn test_compute_delay_exponential_growth() {
        for attempt in 0..5 {
            let delay = compute_delay(attempt, None);
            assert_eq!(delay, 1.5 * (2u64.pow(attempt as u32) as f64));
        }
    }

    #[test]
    fn test_trim_history_empty() {
        let mut history: Vec<Message> = Vec::new();
        trim_history(&mut history, 100);
        assert!(history.is_empty());
    }

    #[test]
    fn test_trim_history_small_no_change() {
        let mut history = vec![Message::user("hello")];
        let original_len = history.len();
        trim_history(&mut history, 1000);
        assert_eq!(history.len(), original_len);
    }

    #[test]
    fn test_trim_history_exceeding_budget() {
        let long_text: String = "x".repeat(5000);
        let mut history = vec![
            Message::user(long_text.clone()),
            Message::assistant("response"),
            Message::user(long_text.clone()),
            Message::assistant("response 2"),
            Message::user(long_text.clone()),
            Message::assistant("response 3"),
            Message::user(long_text.clone()),
            Message::assistant("response 4"),
        ];
        let original_len = history.len();
        trim_history(&mut history, 100);
        assert!(history.len() < original_len);
    }

    #[test]
    fn test_trim_history_removes_from_oldest() {
        let long_text: String = "y".repeat(5000);
        let mut history = vec![
            Message::user("oldest user"),
            Message::assistant(long_text.clone()),
            Message::user(long_text.clone()),
            Message::assistant("middle response"),
            Message::user(long_text.clone()),
            Message::assistant(long_text.clone()),
            Message::user("newest user"),
        ];
        trim_history(&mut history, 100);
        if !history.is_empty() {
            assert_eq!(history[0].role, oz_core_types::Role::User);
        }
    }

    #[test]
    fn test_trim_history_preserves_minimum() {
        let long_text: String = "z".repeat(10000);
        let mut history = (0..50).map(|i| match i % 2 {
            0 => Message::user(long_text.clone()),
            _ => Message::assistant(format!("response {}", i)),
        }).collect::<Vec<_>>();
        let original_len = history.len();
        trim_history(&mut history, 50);
        assert!(history.len() < original_len);
    }
}
