//! Maps a neutral "exchange" representation onto OpenZen's persisted message
//! shape.
//!
//! OpenZen restores a session from `sessions.json`; the frontend's
//! `parseSessionMessages` (frontends/src/lib/stores/chat.ts) reads `role`,
//! `content`, `timestamp`, `streamEvents` and the token/timing fields, then
//! `convertStreamEventsToParts` (frontends/src/lib/stores/parts.ts) turns
//! `streamEvents` into the rendered text/reasoning/tool-invocation parts.
//!
//! So the importer emits the same protocol-v1 event sequence the agent loop
//! writes at finalize time:
//!
//! ```text
//! reasoning_start/delta/end          -> collapsible thinking block
//! text_start/delta/end               -> markdown content block
//! tool_input_start/available         -> tool card (args)
//! tool_output_available              -> tool card result
//! ```
//!
//! `content` is still populated with the concatenated assistant text: the
//! frontend only falls back to it when no text part exists
//! (ChatMessage.svelte:461), so it never double-renders, and it keeps the
//! session searchable.

use serde_json::{json, Value};

/// Neutral representation of one user prompt plus the assistant work it
/// triggered. A single exchange becomes exactly two OpenZen messages.
#[derive(Debug, Clone)]
pub struct Exchange {
    pub user_text: String,
    pub user_ts_ms: Option<i64>,
    pub assistant: Option<AssistantTurn>,
}

/// Every assistant step (text / reasoning / tool activity) that followed a
/// user prompt, flattened into one assistant message — matching OpenZen's
/// one-assistant-message-per-turn model.
#[derive(Debug, Clone, Default)]
pub struct AssistantTurn {
    pub ts_ms: Option<i64>,
    pub duration_ms: Option<i64>,
    pub model: Option<String>,
    pub provider: Option<String>,
    pub tokens_in: Option<i64>,
    pub tokens_out: Option<i64>,
    pub blocks: Vec<RawBlock>,
}

#[derive(Debug, Clone)]
pub enum RawBlock {
    Text(String),
    Reasoning(String),
    ToolCall {
        id: String,
        name: String,
        /// Already-serialized JSON arguments.
        args: String,
    },
    ToolResult {
        id: String,
        output: String,
    },
}

impl AssistantTurn {
    /// Concatenated visible text, used for the message `content` field.
    pub fn text(&self) -> String {
        let mut out = String::new();
        for b in &self.blocks {
            if let RawBlock::Text(t) = b {
                out.push_str(t);
            }
        }
        out
    }
}

/// Convert milliseconds since the Unix epoch to RFC 3339, falling back to
/// "now" for out-of-range values.
pub fn ms_to_rfc3339(ms: i64) -> String {
    chrono::DateTime::from_timestamp_millis(ms)
        .unwrap_or_else(chrono::Utc::now)
        .to_rfc3339()
}

fn opt_rfc3339(ms: Option<i64>) -> String {
    ms.map(ms_to_rfc3339)
        .unwrap_or_else(|| chrono::Utc::now().to_rfc3339())
}

/// Build the OpenZen message vector for a parsed session.
pub fn exchanges_to_messages(exchanges: &[Exchange]) -> Vec<Value> {
    let mut messages = Vec::new();
    for ex in exchanges {
        if ex.user_text.trim().is_empty() && ex.assistant.is_none() {
            continue;
        }
        if !ex.user_text.trim().is_empty() {
            messages.push(json!({
                "role": "user",
                "content": ex.user_text,
                "timestamp": opt_rfc3339(ex.user_ts_ms),
            }));
        }
        if let Some(turn) = &ex.assistant {
            // A turn whose only source parts were structural (step-start /
            // step-finish / timeline) has nothing to render. The frontend
            // drops such a message anyway, so don't emit an empty bubble.
            if let Some(msg) = assistant_message(turn) {
                messages.push(msg);
            }
        }
    }
    messages
}

fn assistant_message(turn: &AssistantTurn) -> Option<Value> {
    let mut events: Vec<Value> = Vec::new();
    let mut text_seq = 0usize;
    let mut reasoning_seq = 0usize;

    // Tool calls and their results arrive interleaved from the parser; emit
    // each result right after its call so the tool card closes in place.
    for block in &turn.blocks {
        match block {
            RawBlock::Text(text) => {
                if text.is_empty() {
                    continue;
                }
                let id = format!("t{text_seq}");
                text_seq += 1;
                events.push(json!({ "type": "text_start", "id": id }));
                events.push(json!({ "type": "text_delta", "id": id, "text": text }));
                events.push(json!({ "type": "text_end", "id": id }));
            }
            RawBlock::Reasoning(text) => {
                if text.is_empty() {
                    continue;
                }
                let id = format!("r{reasoning_seq}");
                reasoning_seq += 1;
                events.push(json!({ "type": "reasoning_start", "id": id }));
                events.push(json!({ "type": "reasoning_delta", "id": id, "text": text }));
                events.push(json!({ "type": "reasoning_end", "id": id }));
            }
            RawBlock::ToolCall { id, name, args } => {
                events.push(json!({
                    "type": "tool_input_start",
                    "tool_call_id": id,
                    "name": name,
                }));
                events.push(json!({
                    "type": "tool_input_available",
                    "tool_call_id": id,
                    "name": name,
                    "args": args,
                }));
            }
            RawBlock::ToolResult { id, output } => {
                events.push(json!({
                    "type": "tool_output_available",
                    "tool_call_id": id,
                    "output": output,
                }));
            }
        }
    }

    let mut msg = serde_json::Map::new();
    let text = turn.text();
    if events.is_empty() && text.is_empty() {
        return None;
    }
    msg.insert("role".into(), json!("assistant"));
    msg.insert("content".into(), json!(text));
    msg.insert("timestamp".into(), json!(opt_rfc3339(turn.ts_ms)));
    // `duration > 0 || exitReason` is what ChatMessage uses to decide a turn
    // is finished (hasFinished); without one of them a restored turn renders
    // with a live "Running" pill.
    msg.insert(
        "duration".into(),
        json!(turn.duration_ms.unwrap_or(0).max(0)),
    );
    msg.insert("exitReason".into(), json!("end_turn"));
    if let Some(model) = &turn.model {
        msg.insert(
            "modelInfo".into(),
            json!({
                "model": model,
                "provider": turn.provider.clone().unwrap_or_else(|| "imported".into()),
                "contextWindow": 0,
                "isLocal": false,
            }),
        );
    }
    if let Some(t) = turn.tokens_in {
        msg.insert("tokensIn".into(), json!(t));
    }
    if let Some(t) = turn.tokens_out {
        msg.insert("tokensOut".into(), json!(t));
    }
    if !events.is_empty() {
        msg.insert("streamEvents".into(), Value::Array(events));
    }
    Some(Value::Object(msg))
}

/// Best-effort arguments serialization: keep an already-JSON string as-is,
/// serialize objects/arrays, and fall back to an empty object.
pub fn args_to_string(value: &Value) -> String {
    match value {
        Value::String(s) if !s.is_empty() => s.clone(),
        Value::Null => "{}".to_string(),
        other => serde_json::to_string(other).unwrap_or_else(|_| "{}".to_string()),
    }
}

/// Flatten a `[{type:"text",text:...}, ...]` content array into plain text.
/// Used for user prompts, where reasoning/tool blocks are not meaningful.
pub fn content_parts_to_text(parts: &[Value]) -> String {
    let mut out = String::new();
    for p in parts {
        let kind = p.get("type").and_then(Value::as_str).unwrap_or("");
        if kind == "text" {
            if let Some(t) = p.get("text").and_then(Value::as_str) {
                if !out.is_empty() {
                    out.push('\n');
                }
                out.push_str(t);
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn emits_protocol_v1_events_in_order() {
        let turn = AssistantTurn {
            ts_ms: Some(1_700_000_000_000),
            duration_ms: Some(1234),
            model: Some("glm-5.2".into()),
            provider: Some("zcode".into()),
            tokens_in: Some(10),
            tokens_out: Some(20),
            blocks: vec![
                RawBlock::Reasoning("think".into()),
                RawBlock::Text("hello".into()),
                RawBlock::ToolCall {
                    id: "call_1".into(),
                    name: "bash".into(),
                    args: r#"{"command":"ls"}"#.into(),
                },
                RawBlock::ToolResult {
                    id: "call_1".into(),
                    output: "file.txt".into(),
                },
            ],
        };
        let ex = Exchange {
            user_text: "do it".into(),
            user_ts_ms: Some(1_699_999_999_000),
            assistant: Some(turn),
        };
        let msgs = exchanges_to_messages(&[ex]);
        assert_eq!(msgs.len(), 2);
        assert_eq!(msgs[0]["role"], "user");
        assert_eq!(msgs[0]["content"], "do it");

        let a = &msgs[1];
        assert_eq!(a["role"], "assistant");
        assert_eq!(a["content"], "hello");
        assert_eq!(a["duration"], 1234);
        assert_eq!(a["exitReason"], "end_turn");
        assert_eq!(a["tokensIn"], 10);

        let types: Vec<&str> = a["streamEvents"]
            .as_array()
            .unwrap()
            .iter()
            .map(|e| e["type"].as_str().unwrap())
            .collect();
        assert_eq!(
            types,
            vec![
                "reasoning_start",
                "reasoning_delta",
                "reasoning_end",
                "text_start",
                "text_delta",
                "text_end",
                "tool_input_start",
                "tool_input_available",
                "tool_output_available",
            ]
        );
        // Tool result must reference the same call id so the card closes.
        let events = a["streamEvents"].as_array().unwrap();
        let result = events.last().unwrap();
        assert_eq!(result["tool_call_id"], "call_1");
        assert_eq!(result["output"], "file.txt");
    }

    #[test]
    fn skips_empty_exchanges_and_empty_assistant() {
        let msgs = exchanges_to_messages(&[Exchange {
            user_text: "   ".into(),
            user_ts_ms: None,
            assistant: None,
        }]);
        assert!(msgs.is_empty());
    }

    #[test]
    fn structural_only_assistant_turn_is_not_emitted() {
        // A turn whose source parts were all structural has no text and no
        // events; emitting it would render an empty assistant bubble that the
        // frontend then discards.
        let msgs = exchanges_to_messages(&[Exchange {
            user_text: "hi".into(),
            user_ts_ms: None,
            assistant: Some(AssistantTurn::default()),
        }]);
        assert_eq!(msgs.len(), 1);
        assert_eq!(msgs[0]["role"], "user");
    }

    #[test]
    fn args_serialization_keeps_json_strings() {
        assert_eq!(args_to_string(&json!(r#"{"a":1}"#)), r#"{"a":1}"#);
        assert_eq!(args_to_string(&json!({"a": 1})), r#"{"a":1}"#);
        assert_eq!(args_to_string(&Value::Null), "{}");
    }
}
