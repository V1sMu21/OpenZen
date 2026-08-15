//! Unified streaming event type.
//!
//! Replaces the previous pattern of 4 independent channels
//! (token_tx, thinking_tx, tool_call_tx, tool_result_tx) with a single
//! event stream of typed start-delta-end protocol events. Inspired by
//! opencode's "message as event source" pattern and the Vercel AI SDK
//! v5 `UIMessageStream` protocol.

use serde::{Deserialize, Serialize};

/// A single todo item tracked by the agent.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TodoItem {
    pub id: String,
    pub content: String,
    pub status: String,
    pub priority: String,
    pub order: usize,
    /// Turn number when this todo entered "in_progress" status (None if never).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub in_progress_since_turn: Option<u32>,
}

/// A single streaming event produced by the agent loop.
///
/// Every event carries enough context for the consumer (SSE, TUI, Tauri)
/// to render or forward the update without needing separate channels.
///
/// The protocol is a typed start-delta-end model:
/// - `TextStart` / `TextDelta` / `TextEnd` — visible assistant text
/// - `ReasoningStart` / `ReasoningDelta` / `ReasoningEnd` — model reasoning
/// - `ToolInputStart` / `ToolInputDelta` / `ToolInputAvailable` — args
/// - `ToolOutputAvailable` — execution result
/// - `StartStep` / `FinishStep` / `FinishMessage` — multi-step markers
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum StreamEvent {
    ReasoningStart {
        id: String,
    },
    ReasoningDelta {
        id: String,
        text: String,
    },
    ReasoningEnd {
        id: String,
    },

    TextStart {
        id: String,
    },
    TextDelta {
        id: String,
        text: String,
    },
    TextEnd {
        id: String,
    },

    ToolInputStart {
        tool_call_id: String,
        name: String,
    },
    ToolInputDelta {
        tool_call_id: String,
        delta: String,
    },
    ToolInputAvailable {
        tool_call_id: String,
        name: String,
        args: String,
    },
    ToolOutputAvailable {
        tool_call_id: String,
        name: String,
        output: String,
    },

    /// Internal — emitted by the OpenAI parser when accumulated JSON
    /// args parse cleanly, so the agent loop can speculatively dispatch.
    /// UIs ignore it.
    ToolCallReady {
        id: String,
        name: String,
        args: String,
    },

    /// Emitted when the ask_user tool fires. The agent loop is now
    /// blocked waiting for the user's reply on `ask_user_rx` and the
    /// same run will resume once it arrives — the reply is NOT a new
    /// user message. `data` is a JSON string of
    /// `{tool_use_id, tool_name, payload}` so the frontend can show
    /// the AskUserDialog without spawning a new assistant turn.
    AskUserPending {
        data: String,
    },

    /// Emitted when context compression runs. Carries before/after token
    /// estimates so the frontend can show a transient notification.
    DataCompressingContext {
        before_tokens: usize,
        after_tokens: usize,
        saved_tokens: usize,
    },

    /// Emitted when the agent calls todowrite or todoupdate tools.
    /// Carries the full todo list snapshot so the frontend can render
    /// the todo progress card inline in the chat message.
    DataTodoUpdate {
        items: Vec<TodoItem>,
        current: usize,
        total: usize,
    },

    StartStep {},
    FinishStep {},
    FinishMessage {
        stop_reason: String,
    },

    Error {
        message: String,
    },

    /// Agent called open_side_panel — frontend should open the artifact
    /// in the right sidebar.
    OpenArtifact {
        artifact_type: String,
        artifact_path: String,
        artifact_label: String,
    },

    /// Emitted after each LLM call to report current context usage.
    /// Lets the frontend show real-time context-window pressure
    /// (the progress bar) instead of waiting until the task finishes.
    DataContextUsage {
        current_tokens: u64,
        output_tokens: u64,
        context_window: usize,
        turn: u32,
        message_count: usize,
        total_input_tokens: u64,
        total_output_tokens: u64,
    },

    /// Emitted when a user intervention was applied mid-task.
    /// The frontend should render an inline card showing the user's message
    /// inside the current assistant bubble with distinct styling.
    UserIntervention {
        content: String,
    },
}
