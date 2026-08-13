use serde::{Deserialize, Serialize};

use crate::event::StreamEvent;
use crate::error::LlmError;
use crate::message::{Message, MockResponse};
use tokio::sync::mpsc::UnboundedSender;

// ── StepOutcome ──

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StepOutcome {
    pub data: serde_json::Value,
    pub next_prompt: Option<String>,
    pub should_exit: bool,
    #[serde(default)]
    pub images: Vec<ImageRef>,
}

impl StepOutcome {
    pub fn success(data: serde_json::Value) -> Self {
        StepOutcome { data, next_prompt: Some("\n".into()), should_exit: false, images: vec![] }
    }
    pub fn success_with_prompt(data: serde_json::Value, prompt: impl Into<String>) -> Self {
        StepOutcome { data, next_prompt: Some(prompt.into()), should_exit: false, images: vec![] }
    }
    pub fn exit(data: serde_json::Value) -> Self {
        StepOutcome { data, next_prompt: None, should_exit: true, images: vec![] }
    }
    pub fn ask_user(question: impl Into<String>) -> Self {
        let question = question.into();
        StepOutcome {
            data: serde_json::json!({
                "status": "INTERRUPT",
                "intent": "HUMAN_INTERVENTION",
                "data": {
                    "question": question,
                    "candidates": [
                        { "label": "Continue", "value": "continue" },
                        { "label": "Provide feedback", "value": "feedback" },
                        { "label": "Abort", "value": "abort" },
                    ],
                },
            }),
            next_prompt: Some("".into()),
            should_exit: true,
            images: vec![],
        }
    }
    pub fn is_exit(&self) -> bool {
        self.should_exit
    }

    /// Create an outcome for an unknown/unregistered tool name.
    pub fn unknown_tool(name: &str) -> Self {
        StepOutcome {
            data: serde_json::Value::Null,
            next_prompt: Some(format!("未知工具: {name}")),
            should_exit: false,
            images: vec![],
        }
    }
}

// ── ExitReason ──

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum ExitReason {
    #[serde(rename = "current_task_done")]
    CurrentTaskDone,
    #[serde(rename = "exited")]
    Exited,
    #[serde(rename = "max_turns_exceeded")]
    MaxTurnsExceeded,
}

impl ExitReason {
    pub fn data(&self) -> serde_json::Value {
        match self {
            ExitReason::CurrentTaskDone => serde_json::json!({"exit_reason": "current_task_done"}),
            ExitReason::Exited => serde_json::json!({"exit_reason": "exited"}),
            ExitReason::MaxTurnsExceeded => serde_json::json!({"exit_reason": "max_turns_exceeded"}),
        }
    }
}

// ── ToolFunction / ToolDefinition ──

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolFunction {
    pub name: String,
    pub description: String,
    pub parameters: serde_json::Value,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolDefinition {
    #[serde(rename = "type")]
    pub type_: String,
    pub function: ToolFunction,
}

// ── ToolErrorData (serialisable error payload for tool results) ──

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolErrorData {
    pub message: String,
    pub code: Option<String>,
    pub details: Option<serde_json::Value>,
}

// ── ImageRef (carries image data from tools to the VLM) ──

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ImageRef {
    pub url: String,
    pub media_type: String,
}

// ── ToolOutput (tool execution result, mirrors StepOutcome) ──

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolOutput {
    pub data: serde_json::Value,
    pub next_prompt: Option<String>,
    pub should_exit: bool,
    #[serde(default)]
    pub images: Vec<ImageRef>,
}

impl From<ToolOutput> for StepOutcome {
    fn from(output: ToolOutput) -> Self {
        StepOutcome {
            data: output.data,
            next_prompt: output.next_prompt,
            should_exit: output.should_exit,
            images: output.images,
        }
    }
}

impl ToolOutput {
    pub fn success(data: serde_json::Value) -> Self {
        ToolOutput { data, next_prompt: Some("\n".into()), should_exit: false, images: vec![] }
    }

    pub fn success_with_prompt(data: serde_json::Value, prompt: impl Into<String>) -> Self {
        ToolOutput { data, next_prompt: Some(prompt.into()), should_exit: false, images: vec![] }
    }

    pub fn unknown_tool(name: &str) -> Self {
        ToolOutput {
            data: serde_json::Value::Null,
            next_prompt: Some(format!("未知工具: {name}")),
            should_exit: false,
            images: vec![],
        }
    }

    pub fn bad_json(msg: impl Into<String>) -> Self {
        let msg = msg.into();
        ToolOutput {
            data: serde_json::json!({"status": "error", "error": &msg}),
            next_prompt: Some(msg),
            should_exit: false,
            images: vec![],
        }
    }
}

// ── ToolContext ──

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ToolContext {
    pub working_dir: String,
    pub assets_dir: String,
    pub script_dir: String,
    pub lang: String,
    /// Path to the .skill_mcp/ directory (for skill/SOP search tools).
    /// If None, defaults to {working_dir}/.skill_mcp/.
    pub skill_mcp_dir: Option<String>,
    /// Session id of the agent run this context belongs to ("" when not in
    /// a session — e.g. tests, TUI, or bridge invocations). Tools that need
    /// per-session identity (like schedule_reminder) read this instead of a
    /// process-global, which breaks under concurrent sessions.
    pub session_id: String,
}

impl ToolContext {
    /// Create a minimal context for testing.
    pub fn test() -> Self {
        ToolContext {
            working_dir: "/tmp".into(),
            assets_dir: "/tmp".into(),
            script_dir: "/tmp".into(),
            lang: "en".into(),
            skill_mcp_dir: None,
            session_id: String::new(),
        }
    }

    /// Resolve a path relative to working_dir.
    /// If the path is absolute or contains ~, return as-is.
    pub fn resolve_path(&self, path: &str) -> String {
        let path = path.trim();
        if path.starts_with('/') || path.starts_with('~') {
            return path.to_string();
        }
        let wd = self.working_dir.trim_end_matches('/');
        format!("{}/{}", wd, path)
    }
}

// ── LlmClient trait ──

/// LLM client trait — allows the agent loop to call different backends.
#[async_trait::async_trait]
pub trait LlmClient: Send {
    async fn chat(&mut self, messages: &[Message], tools: &[ToolDefinition]) -> Result<MockResponse, LlmError>;

    /// Stream a chat, sending events through `event_tx` as they arrive.
    /// When `speculative_tx` is provided, `ToolCallReady` events may be sent
    /// for speculative pre-execution of tool calls from partial stream output.
    /// Includes a default implementation that falls back to the non-streaming `chat`.
    async fn stream_chat(
        &mut self,
        messages: &[Message],
        tools: &[ToolDefinition],
        _event_tx: UnboundedSender<StreamEvent>,
        _speculative_tx: Option<UnboundedSender<StreamEvent>>,
    ) -> Result<MockResponse, LlmError> {
        // Default: fallback to non-streaming
        let result = self.chat(messages, tools).await?;
        tokio::time::sleep(tokio::time::Duration::from_millis(50)).await;
        Ok(result)
    }
}

// ── Tests ──

#[cfg(test)]
mod tests {
    use super::*;

    // ── StepOutcome constructors ──

    #[test]
    fn step_outcome_success() {
        let outcome = StepOutcome::success(serde_json::json!({ "key": "val" }));
        assert_eq!(outcome.data, serde_json::json!({ "key": "val" }));
        assert!(outcome.next_prompt.is_some());
        assert_eq!(outcome.next_prompt.as_ref().unwrap(), "\n");
        assert!(!outcome.should_exit);
    }

    #[test]
    fn step_outcome_success_with_empty_json() {
        let outcome = StepOutcome::success(serde_json::json!({}));
        assert_eq!(outcome.data, serde_json::json!({}));
        assert!(!outcome.should_exit);
    }

    #[test]
    fn step_outcome_success_with_null() {
        let outcome = StepOutcome::success(serde_json::json!(null));
        assert_eq!(outcome.data, serde_json::json!(null));
        assert!(!outcome.should_exit);
    }

    #[test]
    fn step_outcome_success_with_prompt() {
        let outcome = StepOutcome::success_with_prompt(serde_json::json!({ "ok": true }), "continue here");
        assert_eq!(outcome.next_prompt.as_ref().unwrap(), "continue here");
        assert!(!outcome.should_exit);
    }

    #[test]
    fn step_outcome_success_with_prompt_empty() {
        let outcome = StepOutcome::success_with_prompt(serde_json::json!({}), "");
        assert_eq!(outcome.next_prompt.as_ref().unwrap(), "");
        assert!(!outcome.should_exit);
    }

    #[test]
    fn step_outcome_exit() {
        let outcome = StepOutcome::exit(serde_json::json!({ "final": true }));
        assert_eq!(outcome.data, serde_json::json!({ "final": true }));
        assert!(outcome.next_prompt.is_none());
        assert!(outcome.should_exit);
    }

    #[test]
    fn step_outcome_exit_with_null() {
        let outcome = StepOutcome::exit(serde_json::Value::Null);
        assert!(outcome.should_exit);
        assert!(outcome.next_prompt.is_none());
    }

    #[test]
    fn step_outcome_ask_user() {
        let outcome = StepOutcome::ask_user("Do you want to proceed?");
        assert!(outcome.should_exit);
        assert!(outcome.next_prompt.is_some());
        assert_eq!(outcome.next_prompt.as_ref().unwrap(), "");

        let data = &outcome.data;
        assert_eq!(data["status"], "INTERRUPT");
        assert_eq!(data["intent"], "HUMAN_INTERVENTION");
        assert_eq!(data["data"]["question"], "Do you want to proceed?");
        assert!(data["data"]["candidates"].is_array());
    }

    #[test]
    fn step_outcome_ask_user_empty_question() {
        let outcome = StepOutcome::ask_user("");
        assert_eq!(outcome.data["data"]["question"], "");
    }

    #[test]
    fn step_outcome_bad_json() {
        // `success` with non-object is fine — just wraps it.
        let outcome = StepOutcome::success(serde_json::json!("raw string"));
        assert_eq!(outcome.data, "raw string");
    }

    #[test]
    fn step_outcome_bad_json_empty() {
        let outcome = StepOutcome::success(serde_json::json!([]));
        assert_eq!(outcome.data, serde_json::json!([]));
    }

    #[test]
    fn step_outcome_unknown_tool() {
        // Ensure it doesn't panic on unexpected patterns.
        let outcome = StepOutcome::exit(serde_json::json!({ "unknown": true }));
        assert!(outcome.should_exit);
    }

    #[test]
    fn step_outcome_breaker_skip() {
        // `exit` should not force `next_prompt`.
        let outcome = StepOutcome::exit(serde_json::json!({}));
        assert!(outcome.next_prompt.is_none());
    }

    // ── ExitReason::data() ──

    #[test]
    fn exit_reason_current_task_done_data() {
        let reason = ExitReason::CurrentTaskDone;
        let data = reason.data();
        assert_eq!(data["exit_reason"], "current_task_done");
    }

    #[test]
    fn exit_reason_exited_data() {
        let reason = ExitReason::Exited;
        let data = reason.data();
        assert_eq!(data["exit_reason"], "exited");
    }

    #[test]
    fn exit_reason_max_turns_exceeded_data() {
        let reason = ExitReason::MaxTurnsExceeded;
        let data = reason.data();
        assert_eq!(data["exit_reason"], "max_turns_exceeded");
    }

    // ── ToolContext::resolve_path() ──

    #[test]
    fn resolve_path_absolute() {
        let ctx = ToolContext {
            working_dir: "/home/user/project".into(),
            assets_dir: "/assets".into(),
            script_dir: "/scripts".into(),
            lang: "en".into(),
            skill_mcp_dir: None,
            session_id: String::new(),
        };
        assert_eq!(ctx.resolve_path("/etc/passwd"), "/etc/passwd");
    }

    #[test]
    fn resolve_path_tilde() {
        let ctx = ToolContext {
            working_dir: "/home/user/project".into(),
            assets_dir: "/assets".into(),
            script_dir: "/scripts".into(),
            lang: "en".into(),
            skill_mcp_dir: None,
            session_id: String::new(),
        };
        assert_eq!(ctx.resolve_path("~/config"), "~/config");
    }

    #[test]
    fn resolve_path_relative() {
        let ctx = ToolContext {
            working_dir: "/home/user/project".into(),
            assets_dir: "/assets".into(),
            script_dir: "/scripts".into(),
            lang: "en".into(),
            skill_mcp_dir: None,
            session_id: String::new(),
        };
        assert_eq!(ctx.resolve_path("src/main.rs"), "/home/user/project/src/main.rs");
    }

    #[test]
    fn resolve_path_relative_nested() {
        let ctx = ToolContext {
            working_dir: "/workspace".into(),
            assets_dir: "/assets".into(),
            script_dir: "/scripts".into(),
            lang: "en".into(),
            skill_mcp_dir: None,
            session_id: String::new(),
        };
        assert_eq!(ctx.resolve_path("a/b/c.txt"), "/workspace/a/b/c.txt");
    }

    #[test]
    fn resolve_path_working_dir_trailing_slash_removed() {
        let ctx = ToolContext {
            working_dir: "/home/user/project/".into(),
            assets_dir: "/assets".into(),
            script_dir: "/scripts".into(),
            lang: "en".into(),
            skill_mcp_dir: None,
            session_id: String::new(),
        };
        assert_eq!(ctx.resolve_path("foo.txt"), "/home/user/project/foo.txt");
    }

    #[test]
    fn resolve_path_working_dir_multiple_trailing_slashes() {
        let ctx = ToolContext {
            working_dir: "/home/user/project//".into(),
            assets_dir: "/assets".into(),
            script_dir: "/scripts".into(),
            lang: "en".into(),
            skill_mcp_dir: None,
            session_id: String::new(),
        };
        assert!(!ctx.resolve_path("foo.txt").contains("//"));
    }

    #[test]
    fn resolve_path_empty_relative_path() {
        let ctx = ToolContext {
            working_dir: "/home/user/project".into(),
            assets_dir: "/assets".into(),
            script_dir: "/scripts".into(),
            lang: "en".into(),
            skill_mcp_dir: None,
            session_id: String::new(),
        };
        assert_eq!(ctx.resolve_path(""), "/home/user/project/");
    }

    // ── ToolDefinition/ToolFunction serde ──

    #[test]
    fn tool_definition_serialization() {
        let tool = ToolDefinition {
            type_: "function".into(),
            function: ToolFunction {
                name: "test".into(),
                description: "A test tool".into(),
                parameters: serde_json::json!({ "type": "object" }),
            },
        };
        let json = serde_json::to_value(&tool).unwrap();
        assert_eq!(json["type"], "function");
        assert_eq!(json["function"]["name"], "test");
    }

    #[test]
    fn tool_definition_deserialization() {
        let json = serde_json::json!({
            "type": "function",
            "function": {
                "name": "read",
                "description": "Read a file",
                "parameters": { "type": "object" }
            }
        });
        let tool: ToolDefinition = serde_json::from_value(json).unwrap();
        assert_eq!(tool.type_, "function");
        assert_eq!(tool.function.name, "read");
    }

    #[test]
    fn tool_definition_round_trip() {
        let tool = ToolDefinition {
            type_: "function".into(),
            function: ToolFunction {
                name: "write".into(),
                description: "Write a file".into(),
                parameters: serde_json::json!({ "type": "object", "properties": {} }),
            },
        };
        let json = serde_json::to_value(&tool).unwrap();
        let back: ToolDefinition = serde_json::from_value(json).unwrap();
        assert_eq!(back.type_, tool.type_);
        assert_eq!(back.function.name, tool.function.name);
    }

    // ── Clone derives ──

    #[test]
    fn step_outcome_clone() {
        let outcome = StepOutcome::success(serde_json::json!({ "key": "val" }));
        let cloned = outcome.clone();
        assert_eq!(cloned.data, outcome.data);
        assert_eq!(cloned.next_prompt, outcome.next_prompt);
        assert_eq!(cloned.should_exit, outcome.should_exit);
    }

    #[test]
    fn exit_reason_clone() {
        let reason = ExitReason::CurrentTaskDone;
        let cloned = reason.clone();
        assert!(matches!(cloned, ExitReason::CurrentTaskDone));
    }

    #[test]
    fn tool_context_clone() {
        let ctx = ToolContext {
            working_dir: "/test".into(),
            assets_dir: "/assets".into(),
            script_dir: "/scripts".into(),
            lang: "en".into(),
            skill_mcp_dir: None,
            session_id: String::new(),
        };
        let cloned = ctx.clone();
        assert_eq!(cloned.working_dir, ctx.working_dir);
    }
}
