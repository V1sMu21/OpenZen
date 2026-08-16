use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use oz_core_types::{ContentBlock, LlmError, Message, StreamEvent, TokenUsage, ToolDefinition};
use tokio::sync::mpsc::UnboundedSender;

use crate::session::Session;

pub struct MixinSession {
    sessions: Vec<Box<dyn Session>>,
    /// (active session index, when the switch happened). Interior-mutable
    /// because `raw_ask*` take `&self`. This is what makes failover
    /// "sticky": without recording the switch, every turn re-rammed the
    /// dead primary first and paid its full connect-timeout failure.
    failover: Mutex<(usize, Instant)>,
    spring_back_secs: f64,
    retries: u32,
    base_delay: f64,
}

impl MixinSession {
    pub fn new(
        sessions: Vec<Box<dyn Session>>,
        _llm_nos: Option<Vec<usize>>,
        max_retries: Option<u32>,
        base_delay: Option<f64>,
        spring_back: Option<u64>,
    ) -> Self {
        MixinSession {
            sessions,
            failover: Mutex::new((0, Instant::now())),
            spring_back_secs: spring_back.unwrap_or(300) as f64,
            retries: max_retries.unwrap_or(3),
            base_delay: base_delay.unwrap_or(1.5),
        }
    }

    fn pick(&self) -> usize {
        let (cur, switched_at) = *self
            .failover
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        if cur != 0 && switched_at.elapsed().as_secs_f64() > self.spring_back_secs {
            0 // spring back to primary
        } else {
            cur
        }
    }

    /// Record which session served the last successful call so `pick()`
    /// stays on it until the spring-back window elapses.
    fn record_success(&self, idx: usize) {
        let mut f = self.failover.lock().unwrap_or_else(|e| e.into_inner());
        if f.0 != idx {
            *f = (idx, Instant::now());
        }
    }
}

#[async_trait::async_trait]
impl Session for MixinSession {
    fn config(&self) -> &oz_config::SessionConfig {
        self.sessions[self.pick()].config()
    }
    fn history(&self) -> &Mutex<Vec<Message>> {
        self.sessions[0].history()
    }
    fn history_mut(&self) -> &Mutex<Vec<Message>> {
        self.sessions[0].history_mut()
    }
    fn set_system(&mut self, system: String) {
        for s in &mut self.sessions {
            s.set_system(system.clone());
        }
    }
    fn set_tools(&mut self, tools: Vec<ToolDefinition>) {
        for s in &mut self.sessions {
            s.set_tools(tools.clone());
        }
    }

    async fn raw_ask(
        &self,
        messages: &[Message],
    ) -> Result<(Vec<ContentBlock>, Option<TokenUsage>), LlmError> {
        let base = self.pick();
        let n = self.sessions.len();

        for attempt in 0..=self.retries {
            let idx = (base + attempt as usize) % n;
            tracing::info!(
                "[MixinSession] Using session ({})",
                self.sessions[idx].model()
            );

            match self.sessions[idx].raw_ask(messages).await {
                Ok(result) => {
                    if attempt > 0 {
                        tracing::info!("[MixinSession] Switched to session {}", idx);
                    }
                    self.record_success(idx);
                    return Ok(result);
                }
                Err(e) if attempt < self.retries => {
                    let delay = self.base_delay * (1.5f64).powi(attempt as i32);
                    let delay = delay.min(30.0);
                    tracing::warn!(
                        "[MixinSession] Session {} failed: {e}, retry {}/{}, delay {delay:.1}s",
                        idx,
                        attempt + 1,
                        self.retries
                    );
                    tokio::time::sleep(Duration::from_secs_f64(delay)).await;
                }
                Err(e) => return Err(e),
            }
        }
        Err(LlmError::AllSessionsFailed)
    }

    /// Native streaming with failover. Rotation is only attempted while it
    /// is provably safe: pre-output failures (connection refused, HTTP
    /// error status) have emitted nothing, so the next session can take
    /// over transparently. Once a stream has started producing events, a
    /// failure is surfaced to the agent loop — switching would duplicate
    /// partial output on the consumer's channel.
    async fn raw_ask_streaming(
        &self,
        messages: &[Message],
        event_tx: UnboundedSender<StreamEvent>,
        speculative_tx: Option<UnboundedSender<StreamEvent>>,
    ) -> Result<(Vec<ContentBlock>, Option<TokenUsage>), LlmError> {
        let base = self.pick();
        let n = self.sessions.len();

        for attempt in 0..=self.retries {
            let idx = (base + attempt as usize) % n;
            tracing::info!(
                "[MixinSession] Streaming via session ({})",
                self.sessions[idx].model()
            );

            match self.sessions[idx]
                .raw_ask_streaming(messages, event_tx.clone(), speculative_tx.clone())
                .await
            {
                Ok(result) => {
                    if attempt > 0 {
                        tracing::info!("[MixinSession] Stream switched to session {}", idx);
                    }
                    self.record_success(idx);
                    return Ok(result);
                }
                Err(e) => {
                    let safe_to_failover = matches!(
                        e,
                        LlmError::RequestFailed(_) | LlmError::HttpError { .. }
                    );
                    if !safe_to_failover || attempt == self.retries {
                        return Err(e);
                    }
                    let delay = self.base_delay * (1.5f64).powi(attempt as i32);
                    let delay = delay.min(30.0);
                    tracing::warn!(
                        "[MixinSession] Session {} stream failed: {e}, failing over (delay {delay:.1}s)",
                        idx
                    );
                    tokio::time::sleep(Duration::from_secs_f64(delay)).await;
                }
            }
        }
        Err(LlmError::AllSessionsFailed)
    }

    async fn ask(&self, prompt: &str) -> Result<Vec<ContentBlock>, LlmError> {
        let raw_messages = {
            let mut history = self
                .history()
                .lock()
                .map_err(|e| LlmError::Custom(e.to_string()))?;
            history.push(Message::user(prompt));
            if history.len() > 5 {
                crate::retry::trim_history(&mut history, self.config().context_win);
            }
            history
                .iter()
                .map(|m| Message {
                    role: m.role,
                    content: m.content.clone(),
                    tool_results: None,
                })
                .collect::<Vec<_>>()
        };
        let (blocks, _usage) = self.raw_ask(&raw_messages).await?;
        if !blocks.is_empty() {
            let has_error = blocks
                .first()
                .map(|b| match b {
                    ContentBlock::Text { text, .. } => text.starts_with("!!!Error:"),
                    _ => false,
                })
                .unwrap_or(false);
            if !has_error {
                let mut history = self
                    .history()
                    .lock()
                    .map_err(|e| LlmError::Custom(e.to_string()))?;
                history.push(Message::assistant_with_blocks(blocks.clone()));
            }
        }
        Ok(blocks)
    }

    fn format_messages(&self, messages: &[Message]) -> Vec<serde_json::Value> {
        self.sessions[self.pick()].format_messages(messages)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct FakeSession {
        fail_times: usize,
        calls: std::sync::atomic::AtomicUsize,
        model_name: String,
    }

    impl FakeSession {
        fn named(name: &str, fail_times: usize) -> Box<dyn Session> {
            Box::new(FakeSession {
                fail_times,
                calls: std::sync::atomic::AtomicUsize::new(0),
                model_name: name.to_string(),
            })
        }
    }

    #[async_trait::async_trait]
    impl Session for FakeSession {
        fn config(&self) -> &oz_config::SessionConfig {
            unreachable!("not needed for these tests")
        }
        fn history(&self) -> &Mutex<Vec<Message>> {
            unreachable!("not needed for these tests")
        }
        fn history_mut(&self) -> &Mutex<Vec<Message>> {
            unreachable!("not needed for these tests")
        }
        fn set_system(&mut self, _system: String) {}
        fn set_tools(&mut self, _tools: Vec<ToolDefinition>) {}
        fn model(&self) -> String {
            self.model_name.clone()
        }
        async fn raw_ask(
            &self,
            _messages: &[Message],
        ) -> Result<(Vec<ContentBlock>, Option<TokenUsage>), LlmError> {
            let n = self.calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            if n < self.fail_times {
                Err(LlmError::RequestFailed("boom".into()))
            } else {
                Ok((vec![], None))
            }
        }
    }

    #[tokio::test]
    async fn failover_is_sticky() {
        // Primary always fails, secondary always works.
        let mixin = MixinSession::new(
            vec![FakeSession::named("primary", 100), FakeSession::named("backup", 0)],
            None,
            Some(3),
            Some(0.0),
            Some(300),
        );
        mixin.raw_ask(&[]).await.unwrap();
        assert_eq!(mixin.pick(), 1, "must stay on the backup after failover");
        // Second call must NOT hit the primary again (its call count stays).
        mixin.raw_ask(&[]).await.unwrap();
        assert_eq!(mixin.pick(), 1);
    }

    #[tokio::test]
    async fn spring_back_to_primary() {
        let mixin = MixinSession::new(
            vec![FakeSession::named("primary", 0), FakeSession::named("backup", 0)],
            None,
            Some(3),
            Some(0.0),
            Some(0), // spring back immediately
        );
        mixin.record_success(1);
        assert_eq!(mixin.pick(), 0, "must spring back to primary after window");
    }
}
