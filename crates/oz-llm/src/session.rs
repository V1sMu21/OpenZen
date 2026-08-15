use std::sync::Mutex;

use oz_config::SessionConfig;
use oz_core_types::{
    ContentBlock, LlmError, Message, MockToolCall, StreamEvent, TokenUsage, ToolDefinition,
};
use tokio::sync::mpsc::UnboundedSender;

/// Core Session trait — abstracts all LLM API backends.
#[async_trait::async_trait]
pub trait Session: Send + Sync {
    fn config(&self) -> &SessionConfig;
    fn model(&self) -> &str {
        &self.config().model
    }
    fn api_base(&self) -> &str {
        &self.config().apibase
    }
    fn context_window(&self) -> usize {
        self.config().context_win
    }

    fn history(&self) -> &Mutex<Vec<Message>>;
    fn history_mut(&self) -> &Mutex<Vec<Message>>;

    fn set_system(&mut self, system: String);
    fn set_tools(&mut self, tools: Vec<ToolDefinition>);

    /// Raw API call. Returns ContentBlock list and optional token usage.
    async fn raw_ask(
        &self,
        messages: &[Message],
    ) -> Result<(Vec<ContentBlock>, Option<TokenUsage>), LlmError>;

    /// Streaming raw API call. Emits typed start-delta-end protocol
    /// events (`TextStart`/`TextDelta`/`TextEnd`,
    /// `ReasoningStart`/`ReasoningDelta`/`ReasoningEnd`) directly to
    /// the event channel. The default impl calls `raw_ask` and replays
    /// the resulting blocks as protocol events.
    async fn raw_ask_streaming(
        &self,
        messages: &[Message],
        event_tx: UnboundedSender<StreamEvent>,
        _speculative_tx: Option<UnboundedSender<StreamEvent>>,
    ) -> Result<(Vec<ContentBlock>, Option<TokenUsage>), LlmError> {
        let (blocks, usage) = self.raw_ask(messages).await?;
        let mut text_id: Option<String> = None;
        let mut reasoning_id: Option<String> = None;
        for block in &blocks {
            match block {
                ContentBlock::Text { text, .. } if !text.is_empty() => {
                    if text_id.is_none() {
                        let id = format!("t_fallback_{}", uuid::Uuid::new_v4());
                        let _ = event_tx.send(StreamEvent::TextStart { id: id.clone() });
                        text_id = Some(id);
                    }
                    if let Some(ref id) = text_id {
                        let _ = event_tx.send(StreamEvent::TextDelta {
                            id: id.clone(),
                            text: text.clone(),
                        });
                    }
                }
                ContentBlock::Thinking { thinking, .. } if !thinking.is_empty() => {
                    if reasoning_id.is_none() {
                        let id = format!("r_fallback_{}", uuid::Uuid::new_v4());
                        let _ = event_tx.send(StreamEvent::ReasoningStart { id: id.clone() });
                        reasoning_id = Some(id);
                    }
                    if let Some(ref id) = reasoning_id {
                        let _ = event_tx.send(StreamEvent::ReasoningDelta {
                            id: id.clone(),
                            text: thinking.clone(),
                        });
                    }
                }
                _ => {}
            }
        }
        if let Some(id) = text_id {
            let _ = event_tx.send(StreamEvent::TextEnd { id });
        }
        if let Some(id) = reasoning_id {
            let _ = event_tx.send(StreamEvent::ReasoningEnd { id });
        }
        Ok((blocks, usage))
    }

    /// High-level ask with history management.
    async fn ask(&self, prompt: &str) -> Result<Vec<ContentBlock>, LlmError>;

    /// Convert history + system to API-specific message format.
    fn format_messages(&self, messages: &[Message]) -> Vec<serde_json::Value>;
}

/// Result from parsing a MockResponse out of ContentBlocks.
pub fn blocks_to_response(blocks: &[ContentBlock]) -> (String, String, Vec<MockToolCall>) {
    let mut thinking = String::new();
    let mut content = String::new();
    let mut tool_calls = Vec::new();
    for block in blocks {
        match block {
            ContentBlock::Text { text, .. } => content.push_str(text),
            ContentBlock::Thinking { thinking: t, .. } => thinking.push_str(t),
            ContentBlock::ToolUse { id, name, input } => {
                tool_calls.push(MockToolCall::with_id(
                    name.clone(),
                    input.clone(),
                    id.clone(),
                ));
            }
            _ => {}
        }
    }
    (thinking, content, tool_calls)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_blocks_to_response_empty() {
        let (thinking, content, tool_calls) = blocks_to_response(&[]);
        assert!(thinking.is_empty());
        assert!(content.is_empty());
        assert!(tool_calls.is_empty());
    }

    #[test]
    fn test_blocks_to_response_text() {
        let blocks = vec![ContentBlock::text("hello world")];
        let (thinking, content, tool_calls) = blocks_to_response(&blocks);
        assert_eq!(content, "hello world");
        assert!(thinking.is_empty());
        assert!(tool_calls.is_empty());
    }

    #[test]
    fn test_blocks_to_response_thinking() {
        let blocks = vec![ContentBlock::Thinking {
            thinking: "let me think...".into(),
            signature: None,
        }];
        let (thinking, content, tool_calls) = blocks_to_response(&blocks);
        assert_eq!(thinking, "let me think...");
        assert!(content.is_empty());
        assert!(tool_calls.is_empty());
    }

    #[test]
    fn test_blocks_to_response_tool_use() {
        let blocks = vec![ContentBlock::ToolUse {
            id: "tu_1".into(),
            name: "read_file".into(),
            input: serde_json::json!({"path": "/tmp/foo.txt"}),
        }];
        let (thinking, content, tool_calls) = blocks_to_response(&blocks);
        assert!(thinking.is_empty());
        assert!(content.is_empty());
        assert_eq!(tool_calls.len(), 1);
        assert_eq!(tool_calls[0].name, "read_file");
    }

    #[test]
    fn test_blocks_to_response_mixed() {
        let blocks = vec![
            ContentBlock::Thinking {
                thinking: "processing...".into(),
                signature: None,
            },
            ContentBlock::text("result here"),
            ContentBlock::ToolUse {
                id: "tu_2".into(),
                name: "write_file".into(),
                input: serde_json::json!({"data": "x"}),
            },
        ];
        let (thinking, content, tool_calls) = blocks_to_response(&blocks);
        assert_eq!(thinking, "processing...");
        assert_eq!(content, "result here");
        assert_eq!(tool_calls.len(), 1);
        assert_eq!(tool_calls[0].name, "write_file");
    }

    #[test]
    fn test_blocks_to_response_multiple_text() {
        let blocks = vec![ContentBlock::text("first"), ContentBlock::text(" second")];
        let (thinking, content, tool_calls) = blocks_to_response(&blocks);
        assert_eq!(content, "first second");
        assert!(thinking.is_empty());
        assert!(tool_calls.is_empty());
    }

    #[test]
    fn test_blocks_to_response_tool_use_with_id() {
        let blocks = vec![ContentBlock::ToolUse {
            id: "abc-123".into(),
            name: "search".into(),
            input: serde_json::json!({}),
        }];
        let (_, _, tool_calls) = blocks_to_response(&blocks);
        assert_eq!(tool_calls.len(), 1);
        assert_eq!(tool_calls[0].id, "abc-123");
    }

    #[test]
    fn test_blocks_to_response_multiple_tools() {
        let blocks = vec![
            ContentBlock::ToolUse {
                id: "tu_a".into(),
                name: "read_file".into(),
                input: serde_json::json!({}),
            },
            ContentBlock::ToolUse {
                id: "tu_b".into(),
                name: "write_file".into(),
                input: serde_json::json!({}),
            },
        ];
        let (_, _, tool_calls) = blocks_to_response(&blocks);
        assert_eq!(tool_calls.len(), 2);
    }
}
