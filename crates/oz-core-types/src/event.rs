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

    /// Emitted when the user's reply reached the loop and the run is leaving
    /// the waiting state. Runners use it to flip the session status from
    /// `waiting` back to `running` (including IM replies, which never pass
    /// through the desktop `ask_user_response` command).
    AskUserAnswered {
        tool_use_id: String,
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

/// Hard ceiling on the number of events kept per assistant message.
const PERSIST_EVENT_CAP: usize = 6_000;

/// Compact a serialized stream-event log before it is persisted.
///
/// The raw log is a *display replay*: every consumer rebuilds the same
/// bubbles from it, and most of its bytes are redundant. Measured on real
/// data, storing it verbatim made `sessions.json` 93.4 MB for 987 messages —
/// 65.1 MB of that was this log, with each tool's arguments stored three
/// times (start + every input delta + the final `tool_input_available`) and
/// each tool output twice (round5 P0-1).
///
/// Rules — every one of them preserves exactly what the frontend can render
/// (`frontends/src/lib/stores/parts.ts` is the consumer):
///   * consecutive deltas of the same block are MERGED into one event
///     (`reasoning_delta` / `tool_input_delta`), never dropped: the reasoning
///     card's text is built by accumulating deltas, and `message.thinking` is
///     not rendered anywhere — dropping them made every historical thinking
///     block vanish on reload (review finding A/B/E-1);
///   * `tool_input_delta` is dropped only when the same `tool_call_id` also
///     has a `tool_input_available` (which overwrites the args); a stream cut
///     short by Stop has only the deltas, and those are kept;
///   * duplicate `tool_input_available` (speculative + regular execution) is
///     deduplicated; duplicate `tool_output_available` keeps the LAST one,
///     which is what the live UI showed;
///   * transient status readouts (`data_context_usage` / `data_todo_update`)
///     keep only their final value;
///   * the total event count is capped, keeping head and tail.
///
/// Output payloads are deliberately NOT truncated here: the collector already
/// bounds them (`runner.rs::truncate_stream_event`, 100 KB) and the frontend
/// only falls back to the message's `tool_results` when the part has no
/// result at all — a truncated log entry therefore hides the full text
/// forever (review finding A-F2).
pub fn compact_stream_events(events: &[serde_json::Value]) -> Vec<serde_json::Value> {
    use std::collections::{HashMap, HashSet};

    let id_of = |ev: &serde_json::Value| -> String {
        ev.get("tool_call_id")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string()
    };

    // Which tool calls ended up with a complete argument payload?
    let mut complete_args: HashSet<String> = HashSet::new();
    for ev in events {
        if ev.get("type").and_then(|v| v.as_str()) == Some("tool_input_available") {
            let id = id_of(ev);
            if !id.is_empty() {
                complete_args.insert(id);
            }
        }
    }

    let mut out: Vec<serde_json::Value> = Vec::with_capacity(events.len().min(512));
    let mut output_index: HashMap<String, usize> = HashMap::new();
    let mut seen_input: HashSet<String> = HashSet::new();
    let mut last_context_usage: Option<serde_json::Value> = None;
    let mut last_todo_update: Option<serde_json::Value> = None;

    for ev in events {
        let kind = ev.get("type").and_then(|v| v.as_str()).unwrap_or("");
        match kind {
            "reasoning_delta" | "text_delta" => {
                // Merge with the previous event when it is the same kind and
                // the same block id. Adjacency is what keeps two separate
                // blocks (or a block split by a tool call) from being fused.
                let id = ev.get("id").and_then(|v| v.as_str()).unwrap_or("");
                // Both delta kinds carry their text in `text`.
                let field = "text";
                let mergeable = out.last().is_some_and(|prev| {
                    prev.get("type").and_then(|v| v.as_str()) == Some(kind)
                        && prev.get("id").and_then(|v| v.as_str()).unwrap_or("") == id
                        && prev.get(field).and_then(|v| v.as_str()).is_some()
                });
                if mergeable {
                    let prev = out.last_mut().expect("checked above");
                    let mut joined = prev[field].as_str().unwrap_or("").to_string();
                    joined.push_str(ev.get(field).and_then(|v| v.as_str()).unwrap_or(""));
                    prev[field] = serde_json::Value::String(joined);
                } else {
                    out.push(ev.clone());
                }
            }
            "tool_input_delta" => {
                let id = id_of(ev);
                if complete_args.contains(&id) {
                    continue; // superseded by tool_input_available
                }
                let mergeable = out.last().is_some_and(|prev| {
                    prev.get("type").and_then(|v| v.as_str()) == Some("tool_input_delta")
                        && id_of(prev) == id
                });
                if mergeable {
                    let prev = out.last_mut().expect("checked above");
                    let mut joined = prev
                        .get("delta")
                        .and_then(|v| v.as_str())
                        .unwrap_or("")
                        .to_string();
                    joined.push_str(ev.get("delta").and_then(|v| v.as_str()).unwrap_or(""));
                    prev["delta"] = serde_json::Value::String(joined);
                } else {
                    out.push(ev.clone());
                }
            }
            "tool_input_available" => {
                let id = id_of(ev);
                if !id.is_empty() && !seen_input.insert(id) {
                    continue;
                }
                out.push(ev.clone());
            }
            "tool_output_available" => {
                let id = id_of(ev);
                if id.is_empty() {
                    out.push(ev.clone());
                    continue;
                }
                match output_index.get(&id) {
                    // Replace in place: the final result wins (that is what
                    // the live view showed), position stays stable.
                    Some(&idx) => out[idx] = ev.clone(),
                    None => {
                        output_index.insert(id, out.len());
                        out.push(ev.clone());
                    }
                }
            }
            "data_context_usage" => last_context_usage = Some(ev.clone()),
            "data_todo_update" => last_todo_update = Some(ev.clone()),
            _ => out.push(ev.clone()),
        }
    }
    if let Some(v) = last_context_usage {
        out.push(v);
    }
    if let Some(v) = last_todo_update {
        out.push(v);
    }
    if out.len() > PERSIST_EVENT_CAP {
        // Keep the head (tool starts / text) and the tail (final state): the
        // frontend renders the tail's state and the head's cards.
        let head = PERSIST_EVENT_CAP / 2;
        let tail = PERSIST_EVENT_CAP - head;
        let mut capped: Vec<serde_json::Value> = Vec::with_capacity(PERSIST_EVENT_CAP);
        capped.extend_from_slice(&out[..head]);
        capped.extend_from_slice(&out[out.len() - tail..]);
        out = capped;
    }
    out
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

#[cfg(test)]
mod persist_compaction_tests {
    use super::*;
    use serde_json::json;

    fn kinds(events: &[serde_json::Value]) -> Vec<String> {
        events
            .iter()
            .map(|e| e["type"].as_str().unwrap_or("").to_string())
            .collect()
    }

    /// round5 P0-1 regression: the persisted replay log stored each tool's
    /// args three times and each output twice. Deduplication must drop
    /// exactly the redundant copies and keep everything renderable.
    #[test]
    fn compaction_dedupes_without_losing_renderable_events() {
        let events = vec![
            json!({"type":"tool_input_start","tool_call_id":"c1","name":"read"}),
            json!({"type":"tool_input_delta","tool_call_id":"c1","delta":"{\"file_"}),
            json!({"type":"tool_input_delta","tool_call_id":"c1","delta":"path\":\"x\"}"}),
            json!({"type":"tool_input_available","tool_call_id":"c1","name":"read","args":"{\"file_path\":\"x\"}"}),
            // duplicate emission (speculative + regular execution)
            json!({"type":"tool_input_available","tool_call_id":"c1","name":"read","args":"{\"file_path\":\"x\"}"}),
            json!({"type":"tool_output_available","tool_call_id":"c1","name":"read","output":"hello"}),
            json!({"type":"reasoning_start","id":"r0"}),
            json!({"type":"reasoning_delta","id":"r0","text":"think"}),
            json!({"type":"reasoning_delta","id":"r0","text":"ing…"}),
            json!({"type":"reasoning_end","id":"r0"}),
            json!({"type":"text_delta","id":"t0","text":"answer"}),
            json!({"type":"data_context_usage","turn":1,"current_tokens":10}),
            json!({"type":"data_context_usage","turn":2,"current_tokens":20}),
        ];
        let out = compact_stream_events(&events);
        assert_eq!(
            kinds(&out),
            vec![
                "tool_input_start",
                "tool_input_available",
                "tool_output_available",
                "reasoning_start",
                // MERGED, not dropped — the reasoning card's text comes from
                // these deltas (review finding A/B/E-1: dropping them made
                // every historical thinking block disappear on reload).
                "reasoning_delta",
                "reasoning_end",
                "text_delta",
                "data_context_usage",
            ],
            "compaction kept the wrong set of events: {:?}",
            kinds(&out)
        );
        // The two input deltas are gone (superseded), the two reasoning
        // deltas are one merged event with the full text.
        assert_eq!(out[4]["text"], "thinking…");
        // Only the last transient status survives.
        assert_eq!(out.last().unwrap()["turn"], 2);
        assert_eq!(out[2]["output"], "hello");
    }

    /// Review finding A-F3: a tool call whose argument stream was cut short
    /// (Stop pressed mid-stream) has deltas but no `tool_input_available`.
    /// Those deltas are the only copy of its arguments.
    #[test]
    fn input_deltas_are_kept_when_no_complete_payload_arrived() {
        let events = vec![
            json!({"type":"tool_input_start","tool_call_id":"c9","name":"write"}),
            json!({"type":"tool_input_delta","tool_call_id":"c9","delta":"{\"file_path\":\"a\","}),
            json!({"type":"tool_input_delta","tool_call_id":"c9","delta":"\"content\":\"big…\"}"}),
        ];
        let out = compact_stream_events(&events);
        assert_eq!(
            kinds(&out),
            vec!["tool_input_start", "tool_input_delta"],
            "orphan deltas must survive: {:?}",
            kinds(&out)
        );
        assert_eq!(
            out[1]["delta"], "{\"file_path\":\"a\",\"content\":\"big…\"}",
            "orphan deltas must be merged, not lost"
        );
    }

    /// Review finding E-1/E-4: the live UI applies the LAST result for a
    /// tool call (later events overwrite earlier ones), so the persisted log
    /// must agree — keeping the first one showed a stale card after reload.
    #[test]
    fn duplicate_tool_output_keeps_the_last_result() {
        let events = vec![
            json!({"type":"tool_output_available","tool_call_id":"c1","name":"ask_user","output":"INTERRUPT"}),
            json!({"type":"tool_output_available","tool_call_id":"c1","name":"ask_user","output":"USER_REPLIED: go on"}),
        ];
        let out = compact_stream_events(&events);
        assert_eq!(out.len(), 1, "duplicates must collapse: {:?}", out);
        assert_eq!(
            out[0]["output"], "USER_REPLIED: go on",
            "the LAST result is the one the user saw"
        );
    }

    /// Review finding A-F2: truncating the stored output hid the full text
    /// forever (the frontend only falls back to `tool_results` when the part
    /// has no result at all).
    #[test]
    fn tool_output_is_not_truncated() {
        let big = "x".repeat(200_000);
        let events = vec![
            json!({"type":"tool_output_available","tool_call_id":"c1","name":"read","output":big}),
        ];
        let out = compact_stream_events(&events);
        assert_eq!(
            out[0]["output"].as_str().unwrap().len(),
            200_000,
            "output must be stored verbatim; the collector already caps it at 100 KB"
        );
    }

    #[test]
    fn event_count_is_capped_but_keeps_head_and_tail() {
        let mut events = Vec::new();
        for i in 0..(PERSIST_EVENT_CAP + 500) {
            // Distinct ids so the merge rule cannot collapse them.
            events.push(json!({"type":"reasoning_delta","id":format!("r{i}"),"text":"x"}));
        }
        let out = compact_stream_events(&events);
        assert_eq!(out.len(), PERSIST_EVENT_CAP);
        assert_eq!(out[0]["id"], "r0");
        assert_eq!(
            out.last().unwrap()["id"],
            format!("r{}", PERSIST_EVENT_CAP + 499)
        );
    }
}
