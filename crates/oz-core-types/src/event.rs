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

    /// Emitted while a post-answer delivery-quality gate runs — acceptance
    /// spec synthesis, assertion execution, independent review. These run
    /// after the final text has streamed and before `done`, so without a
    /// signal the UI looks frozen for the seconds they take.
    DataQualityGate {
        /// "spec" | "assertions" | "review"
        stage: String,
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

    /// Heartbeat emitted while a long-running tool executes: resets the
    /// frontend's processing watchdog (which otherwise fires after 30
    /// silent minutes and freezes the bubble while the backend is still
    /// working) and gives the UI something to render (elapsed time).
    ToolProgress {
        tool_name: String,
        elapsed_secs: u64,
    },

    /// Emitted right before the agent loop backs off and retries a failed LLM
    /// request (connection error, header timeout, 5xx from the gateway).
    ///
    /// Without it a dead gateway froze the bubble with no explanation at all:
    /// one attempt costs a 60s header timeout wrapped in an inner 5-try
    /// backoff, and the loop makes up to `llm_error_retries` (8) of them —
    /// 20-40 minutes of pure silence. This also resets the frontend
    /// watchdog, since it is a real "still working" signal.
    LlmRetry {
        /// 1-based error count of the attempt that just failed.
        attempt: u32,
        /// Total attempts the loop makes before giving up.
        max_attempts: u32,
        /// The failure, for the status line.
        reason: String,
        /// Seconds until the next attempt starts.
        retry_in_secs: u64,
    },
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

/// Append `event` to `buf`, merging per-token deltas (Text / Reasoning /
/// ToolInput) into the previous event when they belong to the same block.
///
/// Long streaming runs emit one event per token; keeping every delta in the
/// collected Vec (and then in the persisted `streamEvents`) makes memory and
/// sessions.json grow with token count. Consumers concatenate same-id deltas
/// anyway, so merging keeps the collected stream O(blocks) with identical
/// reconstruction semantics.
///
/// Returns `true` when the event was merged into its predecessor — callers
/// that keep per-event side data (e.g. arrival timestamps) must skip pushing
/// a new sample in that case to stay index-aligned.
pub fn append_coalesced(buf: &mut Vec<StreamEvent>, event: StreamEvent) -> bool {
    match (buf.last_mut(), &event) {
        (
            Some(StreamEvent::TextDelta {
                id: prev_id,
                text: prev,
            }),
            StreamEvent::TextDelta { id, text },
        ) if prev_id == id => {
            prev.push_str(text);
            true
        }
        (
            Some(StreamEvent::ReasoningDelta {
                id: prev_id,
                text: prev,
            }),
            StreamEvent::ReasoningDelta { id, text },
        ) if prev_id == id => {
            prev.push_str(text);
            true
        }
        (
            Some(StreamEvent::ToolInputDelta {
                tool_call_id: prev_id,
                delta: prev,
            }),
            StreamEvent::ToolInputDelta {
                tool_call_id,
                delta,
            },
        ) if prev_id == tool_call_id => {
            prev.push_str(delta);
            true
        }
        _ => {
            buf.push(event);
            false
        }
    }
}

#[cfg(test)]
mod coalesce_tests {
    use super::*;

    #[test]
    fn merges_consecutive_same_id_deltas() {
        let mut buf = vec![
            StreamEvent::TextStart { id: "t1".into() },
            StreamEvent::TextDelta {
                id: "t1".into(),
                text: "你".into(),
            },
        ];
        let merged = append_coalesced(
            &mut buf,
            StreamEvent::TextDelta {
                id: "t1".into(),
                text: "好".into(),
            },
        );
        assert!(merged);
        assert_eq!(buf.len(), 2);
        match &buf[1] {
            StreamEvent::TextDelta { text, .. } => assert_eq!(text, "你好"),
            _ => panic!("expected TextDelta"),
        }
    }

    #[test]
    fn different_id_or_type_starts_new_event() {
        let mut buf = vec![StreamEvent::TextDelta {
            id: "t1".into(),
            text: "a".into(),
        }];
        assert!(!append_coalesced(
            &mut buf,
            StreamEvent::TextDelta {
                id: "t2".into(),
                text: "b".into(),
            }
        ));
        assert!(!append_coalesced(
            &mut buf,
            StreamEvent::TextEnd { id: "t2".into() }
        ));
        assert_eq!(buf.len(), 3);
    }

    /// The retry notice is the only thing the UI shows during a gateway
    /// outage, so its wire shape is a contract: the frontend reads
    /// `attempt` / `max_attempts` / `reason` / `retry_in_secs` off the
    /// `protocol_v1` payload (`frontends/src/lib/stores/parts.ts`).
    #[test]
    fn llm_retry_serialises_with_the_fields_the_ui_reads() {
        let ev = StreamEvent::LlmRetry {
            attempt: 2,
            max_attempts: 8,
            reason: "Stream error: no response headers within 60s".into(),
            retry_in_secs: 3,
        };
        let v = serde_json::to_value(&ev).unwrap();
        assert_eq!(v["type"], "llm_retry");
        assert_eq!(v["attempt"], 2);
        assert_eq!(v["max_attempts"], 8);
        assert_eq!(v["retry_in_secs"], 3);
        assert_eq!(v["reason"], "Stream error: no response headers within 60s");

        let back: StreamEvent = serde_json::from_value(v).unwrap();
        match back {
            StreamEvent::LlmRetry {
                attempt: 2,
                max_attempts: 8,
                retry_in_secs: 3,
                ..
            } => {}
            other => panic!("round-trip changed the event: {other:?}"),
        }
    }
}
