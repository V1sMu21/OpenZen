use serde::{Deserialize, Serialize};

/// Content block type matching Claude Messages API format.
/// Also used internally for OpenAI-compatible conversions.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ContentBlock {
    Text {
        text: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        cache_control: Option<CacheControl>,
    },
    ToolUse {
        id: String,
        name: String,
        input: serde_json::Value,
    },
    ToolResult {
        tool_use_id: String,
        content: ContentContainer,
        #[serde(skip_serializing_if = "Option::is_none")]
        is_error: Option<bool>,
    },
    Thinking {
        thinking: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        signature: Option<String>,
    },
    ImageUrl {
        url: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        media_type: Option<String>,
    },
}

impl ContentBlock {
    pub fn text(text: impl Into<String>) -> Self {
        ContentBlock::Text {
            text: text.into(),
            cache_control: None,
        }
    }
    pub fn text_cached(text: impl Into<String>) -> Self {
        ContentBlock::Text {
            text: text.into(),
            cache_control: Some(CacheControl { type_: "ephemeral".into() }),
        }
    }
    pub fn tool_use(id: impl Into<String>, name: impl Into<String>, input: serde_json::Value) -> Self {
        ContentBlock::ToolUse { id: id.into(), name: name.into(), input }
    }
    pub fn tool_result(tool_use_id: impl Into<String>, content: impl Into<String>) -> Self {
        ContentBlock::ToolResult {
            tool_use_id: tool_use_id.into(),
            content: ContentContainer::Text(content.into()),
            is_error: None,
        }
    }

    /// Deserialize a ContentBlock from a JSON value.
    pub fn from_json(value: serde_json::Value) -> Self {
        serde_json::from_value(value).expect("ContentBlock::from_json: invalid JSON")
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CacheControl {
    #[serde(rename = "type")]
    pub type_: String,
}

/// Container for multi-format content (text or block list).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(untagged)]
pub enum ContentContainer {
    Text(String),
    Blocks(Vec<ContentBlock>),
}

impl ContentContainer {
    pub fn as_text(&self) -> Option<&str> {
        match self {
            ContentContainer::Text(t) => Some(t.as_str()),
            _ => None,
        }
    }
    pub fn as_blocks(&self) -> Option<&[ContentBlock]> {
        match self {
            ContentContainer::Blocks(b) => Some(b.as_slice()),
            _ => None,
        }
    }
}

impl From<String> for ContentContainer {
    fn from(s: String) -> Self { ContentContainer::Text(s) }
}

impl From<Vec<ContentBlock>> for ContentContainer {
    fn from(v: Vec<ContentBlock>) -> Self { ContentContainer::Blocks(v) }
}

/// Message role in the conversation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Role {
    System,
    User,
    Assistant,
    Tool,
}

impl Role {
    pub fn as_str(&self) -> &'static str {
        match self {
            Role::System => "system",
            Role::User => "user",
            Role::Assistant => "assistant",
            Role::Tool => "tool",
        }
    }
}

/// A single message in the conversation history.
/// Mirrors the Claude Content-Block format.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Message {
    pub role: Role,
    pub content: Vec<ContentBlock>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_results: Option<Vec<ToolResultItem>>,
}

impl Message {
    pub fn system(text: impl Into<String>) -> Self {
        Message {
            role: Role::System,
            content: vec![ContentBlock::text(text)],
            tool_results: None,
        }
    }
    pub fn user(text: impl Into<String>) -> Self {
        Message {
            role: Role::User,
            content: vec![ContentBlock::text(text)],
            tool_results: None,
        }
    }
    pub fn user_with_blocks(blocks: Vec<ContentBlock>) -> Self {
        Message { role: Role::User, content: blocks, tool_results: None }
    }
    pub fn assistant(text: impl Into<String>) -> Self {
        Message {
            role: Role::Assistant,
            content: vec![ContentBlock::text(text)],
            tool_results: None,
        }
    }
    pub fn assistant_with_blocks(blocks: Vec<ContentBlock>) -> Self {
        Message { role: Role::Assistant, content: blocks, tool_results: None }
    }
    pub fn tool(tool_use_id: impl Into<String>, content: impl Into<String>) -> Self {
        Message {
            role: Role::Tool,
            content: vec![ContentBlock::tool_result(tool_use_id, content)],
            tool_results: None,
        }
    }

    /// Extract all text from text-type content blocks.
    pub fn content_text(&self) -> String {
        self.content.iter().filter_map(|b| match b {
            ContentBlock::Text { text, .. } => Some(text.as_str()),
            _ => None,
        }).collect::<Vec<_>>().join("\n")
    }

    /// Mark last user block with cache_control: ephemeral (Claude prompt caching).
    pub fn mark_cache_control(&mut self) {
        if let Some(last) = self.content.last_mut() {
            if let ContentBlock::Text { ref mut cache_control, .. } = last {
                let _ = cache_control.insert(CacheControl { type_: "ephemeral".into() });
            }
        }
    }
}

use crate::tool::ImageRef;

/// Thin wrapper for tool result passing in the agent loop.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolResultItem {
    pub tool_use_id: String,
    pub content: String,
    #[serde(default)]
    pub images: Vec<ImageRef>,
}

#[derive(Debug, Clone)]
pub struct MockToolCall {
    pub name: String,
    pub arguments: serde_json::Value,
    pub id: String,
}

impl MockToolCall {
    pub fn new(name: impl Into<String>, arguments: serde_json::Value) -> Self {
        MockToolCall {
            name: name.into(),
            arguments,
            id: String::new(),
        }
    }
    pub fn with_id(name: impl Into<String>, arguments: serde_json::Value, id: impl Into<String>) -> Self {
        MockToolCall {
            name: name.into(),
            arguments,
            id: id.into(),
        }
    }
}

/// Real token usage from the LLM provider, captured from the final
/// streaming chunk (when `stream_options.include_usage=true` on
/// OpenAI, or `message_delta.usage` on Claude). Distinguishes prompt
/// (input) vs completion (output) so the UI can show meaningful
/// per-turn token counts instead of chars/4 estimates.
#[derive(Debug, Clone, Default)]
pub struct TokenUsage {
    /// Tokens consumed by the prompt (system + tools + history + user message).
    pub input_tokens: u64,
    /// Tokens generated by the model in this turn (visible + reasoning).
    pub output_tokens: u64,
    /// Sum of input + output, if the provider reports it (OpenAI does,
    /// Claude does not). Optional for back-compat.
    pub total_tokens: Option<u64>,
}

impl TokenUsage {
    pub fn merge(&mut self, other: &TokenUsage) {
        self.input_tokens = self.input_tokens.saturating_add(other.input_tokens);
        self.output_tokens = self.output_tokens.saturating_add(other.output_tokens);
        match (self.total_tokens, other.total_tokens) {
            (Some(a), Some(b)) => self.total_tokens = Some(a.saturating_add(b)),
            (None, Some(b)) => self.total_tokens = Some(b),
            (Some(a), None) => self.total_tokens = Some(a),
            (None, None) => self.total_tokens = None,
        }
    }
}

/// Unified LLM response — matches Python's MockResponse.
#[derive(Debug, Clone)]
pub struct MockResponse {
    pub thinking: String,
    pub content: String,
    pub tool_calls: Vec<MockToolCall>,
    pub raw: String,
    pub stop_reason: String,
    /// Real token usage reported by the LLM provider. `None` when the
    /// provider didn't send a usage chunk (e.g. native sessions that
    /// wrap a different API). When `None`, callers fall back to the
    /// legacy chars/4 estimate.
    pub usage: Option<TokenUsage>,
}

impl MockResponse {
    pub fn new(content: impl Into<String>) -> Self {
        MockResponse {
            thinking: String::new(),
            content: content.into(),
            tool_calls: Vec::new(),
            raw: String::new(),
            stop_reason: "end_turn".into(),
            usage: None,
        }
    }
    pub fn with_tools(content: impl Into<String>, tool_calls: Vec<MockToolCall>) -> Self {
        let stop = if tool_calls.is_empty() { "end_turn".into() } else { "tool_use".into() };
        MockResponse {
            thinking: String::new(),
            content: content.into(),
            tool_calls,
            raw: String::new(),
            stop_reason: stop,
            usage: None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── ContentBlock constructors ──

    #[test]
    fn content_block_text_basic() {
        let block = ContentBlock::text("hello");
        match &block {
            ContentBlock::Text { text, cache_control } => {
                assert_eq!(text, "hello");
                assert!(cache_control.is_none());
            }
            _ => panic!("expected Text variant"),
        }
    }

    #[test]
    fn content_block_text_empty_string() {
        let block = ContentBlock::text("");
        match &block {
            ContentBlock::Text { text, cache_control } => {
                assert_eq!(text, "");
                assert!(cache_control.is_none());
            }
            _ => panic!("expected Text variant"),
        }
    }

    #[test]
    fn content_block_text_cached() {
        let block = ContentBlock::text_cached("cached text");
        match &block {
            ContentBlock::Text { text, cache_control } => {
                assert_eq!(text, "cached text");
                assert!(cache_control.is_some());
                assert_eq!(cache_control.as_ref().unwrap().type_, "ephemeral");
            }
            _ => panic!("expected Text variant"),
        }
    }

    #[test]
    fn content_block_text_cached_empty_string() {
        let block = ContentBlock::text_cached("");
        match &block {
            ContentBlock::Text { text, cache_control } => {
                assert_eq!(text, "");
                assert!(cache_control.is_some());
            }
            _ => panic!("expected Text variant"),
        }
    }

    #[test]
    fn content_block_tool_use() {
        let input = serde_json::json!({ "query": "test" });
        let block = ContentBlock::tool_use("tu_1", "search", input.clone());
        match &block {
            ContentBlock::ToolUse { id, name, input: i } => {
                assert_eq!(id, "tu_1");
                assert_eq!(name, "search");
                assert_eq!(i, &input);
            }
            _ => panic!("expected ToolUse variant"),
        }
    }

    #[test]
    fn content_block_tool_result() {
        let block = ContentBlock::tool_result("tu_1", "result content");
        match &block {
            ContentBlock::ToolResult { tool_use_id, content, is_error } => {
                assert_eq!(tool_use_id, "tu_1");
                assert!(matches!(content.as_text().unwrap(), s if s == "result content"));
                assert!(is_error.is_none());
            }
            _ => panic!("expected ToolResult variant"),
        }
    }

    #[test]
    fn content_block_thinking() {
        let block = ContentBlock::Thinking {
            thinking: "thoughts".into(),
            signature: None,
        };
        assert!(matches!(&block, ContentBlock::Thinking { .. }));
    }

    #[test]
    fn content_block_image_url() {
        let block = ContentBlock::ImageUrl {
            url: "https://example.com/img.png".into(),
            media_type: Some("image/png".into()),
        };
        assert!(matches!(&block, ContentBlock::ImageUrl { .. }));
    }

    // ── ContentContainer ──

    #[test]
    fn content_container_from_string() {
        let container: ContentContainer = "plain text".to_string().into();
        assert_eq!(container.as_text(), Some("plain text"));
        assert!(container.as_blocks().is_none());
    }

    #[test]
    fn content_container_from_string_empty() {
        let container: ContentContainer = String::new().into();
        assert_eq!(container.as_text(), Some(""));
    }

    #[test]
    fn content_container_from_vec_blocks() {
        let blocks = vec![ContentBlock::text("a"), ContentBlock::text("b")];
        let container: ContentContainer = blocks.into();
        assert!(container.as_text().is_none());
        let retrieved = container.as_blocks().unwrap();
        assert_eq!(retrieved.len(), 2);
    }

    #[test]
    fn content_container_from_empty_vec() {
        let blocks: Vec<ContentBlock> = vec![];
        let container: ContentContainer = blocks.into();
        let retrieved = container.as_blocks().unwrap();
        assert!(retrieved.is_empty());
    }

    #[test]
    fn content_container_as_text_on_blocks_returns_none() {
        let blocks = vec![ContentBlock::text("x")];
        let container: ContentContainer = ContentContainer::Blocks(blocks);
        assert!(container.as_text().is_none());
    }

    #[test]
    fn content_container_as_blocks_on_text_returns_none() {
        let container: ContentContainer = ContentContainer::Text("hi".into());
        assert!(container.as_blocks().is_none());
    }

    // ── Role ──

    #[test]
    fn role_as_str() {
        assert_eq!(Role::System.as_str(), "system");
        assert_eq!(Role::User.as_str(), "user");
        assert_eq!(Role::Assistant.as_str(), "assistant");
        assert_eq!(Role::Tool.as_str(), "tool");
    }

    // ── Message constructors ──

    #[test]
    fn message_system() {
        let msg = Message::system("system prompt");
        assert_eq!(msg.role, Role::System);
        assert_eq!(msg.content.len(), 1);
        assert!(msg.tool_results.is_none());
    }

    #[test]
    fn message_user() {
        let msg = Message::user("user query");
        assert_eq!(msg.role, Role::User);
        assert_eq!(msg.content.len(), 1);

        match &msg.content[0] {
            ContentBlock::Text { text, cache_control } => {
                assert_eq!(text, "user query");
                assert!(cache_control.is_none());
            }
            _ => panic!("expected Text block"),
        }
    }

    #[test]
    fn message_user_with_blocks() {
        let blocks = vec![ContentBlock::text("a"), ContentBlock::text("b")];
        let msg = Message::user_with_blocks(blocks);
        assert_eq!(msg.role, Role::User);
        assert_eq!(msg.content.len(), 2);
    }

    #[test]
    fn message_assistant() {
        let msg = Message::assistant("agent reply");
        assert_eq!(msg.role, Role::Assistant);
        assert_eq!(msg.content.len(), 1);
    }

    #[test]
    fn message_assistant_with_blocks() {
        let blocks = vec![ContentBlock::tool_use("tu_1", "read_file", serde_json::json!({}))];
        let msg = Message::assistant_with_blocks(blocks);
        assert_eq!(msg.role, Role::Assistant);
        assert_eq!(msg.content.len(), 1);
    }

    #[test]
    fn message_tool() {
        let msg = Message::tool("tu_1", "tool output");
        assert_eq!(msg.role, Role::Tool);
        assert_eq!(msg.content.len(), 1);

        match &msg.content[0] {
            ContentBlock::ToolResult { tool_use_id, content, .. } => {
                assert_eq!(tool_use_id, "tu_1");
                match content.as_text() {
                    Some(t) => assert_eq!(t, "tool output"),
                    None => panic!("expected text content"),
                }
            }
            _ => panic!("expected ToolResult block"),
        }
    }

    #[test]
    fn message_constructors_with_empty_string() {
        let sys = Message::system("");
        assert_eq!(sys.role, Role::System);

        let usr = Message::user("");
        assert_eq!(usr.role, Role::User);

        let asst = Message::assistant("");
        assert_eq!(asst.role, Role::Assistant);

        let tool = Message::tool("", "");
        assert_eq!(tool.role, Role::Tool);
    }

    // ── content_text (extracts text from Text blocks) ──

    #[test]
    fn content_text_single_block() {
        let msg = Message::user("hello world");
        assert_eq!(msg.content_text(), "hello world");
    }

    #[test]
    fn content_text_multiple_text_blocks() {
        let blocks = vec![ContentBlock::text("first"), ContentBlock::text("second")];
        let msg = Message::user_with_blocks(blocks);
        assert_eq!(msg.content_text(), "first\nsecond");
    }

    #[test]
    fn content_text_skips_non_text_blocks() {
        let blocks = vec![
            ContentBlock::text("a"),
            ContentBlock::tool_use("tu_1", "read", serde_json::json!({})),
            ContentBlock::text("b"),
        ];
        let msg = Message::assistant_with_blocks(blocks);
        assert_eq!(msg.content_text(), "a\nb");
    }

    #[test]
    fn content_text_all_non_text() {
        let blocks = vec![ContentBlock::tool_use("tu_1", "read", serde_json::json!({}))];
        let msg = Message::assistant_with_blocks(blocks);
        assert_eq!(msg.content_text(), "");
    }

    #[test]
    fn content_text_empty_message() {
        let blocks: Vec<ContentBlock> = vec![];
        let msg = Message::user_with_blocks(blocks);
        assert_eq!(msg.content_text(), "");
    }

    // ── mark_cache_control ──

    #[test]
    fn mark_cache_control_on_text_block() {
        let mut msg = Message::user("important text");
        msg.mark_cache_control();

        match &msg.content[0] {
            ContentBlock::Text { cache_control, .. } => {
                assert!(cache_control.is_some());
                assert_eq!(cache_control.as_ref().unwrap().type_, "ephemeral");
            }
            _ => panic!("expected Text block"),
        }
    }

    #[test]
    fn mark_cache_control_on_non_text_last_block() {
        let blocks = vec![ContentBlock::tool_use("tu_1", "read", serde_json::json!({}))];
        let mut msg = Message::assistant_with_blocks(blocks);
        msg.mark_cache_control();

        match &msg.content[0] {
            ContentBlock::ToolUse { .. } => {}
            _ => panic!("expected ToolUse block to remain unchanged"),
        }
    }

    #[test]
    fn mark_cache_control_on_empty_content() {
        let blocks: Vec<ContentBlock> = vec![];
        let mut msg = Message::user_with_blocks(blocks);
        msg.mark_cache_control();
    }

    #[test]
    fn mark_cache_control_mixed_blocks_marks_last_only() {
        let blocks = vec![
            ContentBlock::text("first"),
            ContentBlock::text("last"),
        ];
        let mut msg = Message::user_with_blocks(blocks);
        msg.mark_cache_control();

        match &msg.content[0] {
            ContentBlock::Text { cache_control, .. } => assert!(cache_control.is_none()),
            _ => panic!("expected Text block"),
        }

        match &msg.content[1] {
            ContentBlock::Text { cache_control, .. } => assert!(cache_control.is_some()),
            _ => panic!("expected Text block"),
        }
    }

    // ── MockToolCall ──

    #[test]
    fn mock_tool_call_new() {
        let call = MockToolCall::new("read_file", serde_json::json!({ "path": "/foo" }));
        assert_eq!(call.name, "read_file");
        assert_eq!(call.id, "");
    }

    #[test]
    fn mock_tool_call_with_id() {
        let call = MockToolCall::with_id("write_file", serde_json::json!({}), "tu_custom_1");
        assert_eq!(call.name, "write_file");
        assert_eq!(call.id, "tu_custom_1");
    }

    #[test]
    fn mock_tool_call_with_id_vs_new() {
        let new_call = MockToolCall::new("tool", serde_json::json!({}));
        let id_call = MockToolCall::with_id("tool", serde_json::json!({}), "my-id");
        assert_eq!(new_call.id, "");
        assert_eq!(id_call.id, "my-id");
        assert_ne!(new_call.id, id_call.id);
    }

    // ── MockResponse ──

    #[test]
    fn mock_response_new() {
        let resp = MockResponse::new("some content");
        assert_eq!(resp.content, "some content");
        assert_eq!(resp.thinking, "");
        assert!(resp.tool_calls.is_empty());
        assert_eq!(resp.raw, "");
        assert_eq!(resp.stop_reason, "end_turn");
    }

    #[test]
    fn mock_response_new_empty_string() {
        let resp = MockResponse::new("");
        assert_eq!(resp.content, "");
        assert_eq!(resp.stop_reason, "end_turn");
    }

    #[test]
    fn mock_response_with_tools_non_empty() {
        let calls = vec![MockToolCall::new("search", serde_json::json!({}))];
        let resp = MockResponse::with_tools("doing search", calls);
        assert!(!resp.tool_calls.is_empty());
        assert_eq!(resp.stop_reason, "tool_use");
    }

    #[test]
    fn mock_response_with_tools_empty() {
        let resp = MockResponse::with_tools("just text", vec![]);
        assert!(resp.tool_calls.is_empty());
        assert_eq!(resp.stop_reason, "end_turn");
    }

    // ── Serde serialization / deserialization ──

    #[test]
    fn content_block_text_serialization() {
        let block = ContentBlock::text("hello");
        let json = serde_json::to_string(&block).unwrap();
        assert!(json.contains("text"));
        assert!(json.contains("hello"));
        assert!(!json.contains("cache_control"));
    }

    #[test]
    fn content_block_text_cached_serialization() {
        let block = ContentBlock::text_cached("x");
        let json = serde_json::to_string(&block).unwrap();
        assert!(json.contains("ephemeral"));
    }

    #[test]
    fn content_block_tool_use_serialization() {
        let block = ContentBlock::tool_use("tu_1", "read_file", serde_json::json!({ "path": "/" }));
        let json = serde_json::to_string(&block).unwrap();
        assert!(json.contains("tool_use"));
        assert!(json.contains("tu_1"));
        assert!(json.contains("read_file"));
    }

    #[test]
    fn message_serialization_round_trip() {
        let msg = Message::system("test prompt");
        let json = serde_json::to_string(&msg).unwrap();
        let parsed: Message = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed.role, Role::System);
        assert_eq!(parsed.content.len(), 1);
    }

    #[test]
    fn role_serialization() {
        let json = serde_json::to_string(&Role::User).unwrap();
        assert_eq!(json, "\"user\"");

        let role: Role = serde_json::from_str("\"assistant\"").unwrap();
        assert_eq!(role, Role::Assistant);
    }

    #[test]
    fn content_container_text_serialization() {
        let container: ContentContainer = "simple text".to_string().into();
        let json = serde_json::to_string(&container).unwrap();
        assert_eq!(json, "\"simple text\"");
    }

    #[test]
    fn message_with_tool_results_serialization() {
        let msg = Message {
            role: Role::Tool,
            content: vec![ContentBlock::tool_result("tu_1", "ok")],
            tool_results: Some(vec![ToolResultItem { tool_use_id: "tu_1".into(), content: "ok".into(), images: vec![] }]),
        };
        let json = serde_json::to_string(&msg).unwrap();
        assert!(json.contains("tool_results"));
    }

    #[test]
    fn message_without_tool_results_skips_in_json() {
        let msg = Message::user("hi");
        let json = serde_json::to_string(&msg).unwrap();
        assert!(!json.contains("tool_results"));
    }
}
