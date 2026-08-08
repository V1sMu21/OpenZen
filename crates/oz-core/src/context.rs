use oz_core_types::Message;

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
        m.content.iter().map(|b| {
            match b {
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
            }
        }).sum::<usize>()
    }).sum()
}

fn sanitize_leading_user_msg(msg: &mut Message) {
    let texts: Vec<String> = msg.content.iter().filter_map(|b| {
        match b {
            oz_core_types::ContentBlock::Text { text, .. } => Some(text.clone()),
            oz_core_types::ContentBlock::ToolResult { content, .. } => {
                match content {
                    oz_core_types::ContentContainer::Text(t) => Some(t.clone()),
                    oz_core_types::ContentContainer::Blocks(bs) => {
                        Some(bs.iter().filter_map(|b| {
                            match b {
                                oz_core_types::ContentBlock::Text { text, .. } => Some(text.clone()),
                                _ => None,
                            }
                        }).collect::<Vec<_>>().join("\n"))
                    }
                }
            }
            _ => None,
        }
    }).collect();
    msg.content = vec![oz_core_types::ContentBlock::text(texts.join("\n"))];
}

#[cfg(test)]
mod tests {
    use super::*;
    use oz_core_types::Message;

    #[test]
    fn trim_history_small_history_under_budget_does_nothing() {
        let mut history = vec![
            Message::user("hello"),
            Message::assistant("hi there"),
            Message::user("how are you"),
        ];
        trim_history(&mut history, 1000);
        assert_eq!(history.len(), 3);
    }

    #[test]
    fn trim_history_removes_old_messages_when_over_budget() {
        let mut history = vec![];
        for i in 0..20 {
            if i % 2 == 0 {
                history.push(Message::user(&format!("User message number {}", i)));
            } else {
                history.push(Message::assistant(&format!("Assistant response {}", i)));
            }
        }
        let original_len = history.len();
        trim_history(&mut history, 5);
        assert!(history.len() < original_len);
        assert!(!history.is_empty());
    }

    #[test]
    fn trim_history_preserves_minimum_message_count() {
        let mut history = vec![];
        for i in 0..30 {
            if i % 2 == 0 {
                history.push(Message::user(&format!("msg_{}", i)));
            } else {
                history.push(Message::assistant(&format!("response_{}", i)));
            }
        }
        trim_history(&mut history, 1);
        assert!(history.len() >= 3);
        assert!(!history.is_empty());
    }

    #[test]
    fn trim_history_with_empty_vector() {
        let mut history: Vec<Message> = vec![];
        trim_history(&mut history, 100);
        assert!(history.is_empty());
    }

    #[test]
    fn trim_history_leaves_user_message_at_start_after_removal() {
        let mut history = vec![
            Message::user("first user msg"),
            Message::assistant("first assistant msg"),
            Message::user("second user msg"),
            Message::assistant("second assistant msg"),
            Message::user("third user msg"),
            Message::assistant("third assistant msg"),
            Message::user("fourth user msg"),
            Message::assistant("fourth assistant msg"),
        ];
        trim_history(&mut history, 2);
        if !history.is_empty() {
            assert_eq!(history[0].role, oz_core_types::Role::User);
        }
    }
}
