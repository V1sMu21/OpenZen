//! Reader for the z.ai ZCode CLI history database.
//!
//! ZCode keeps its authoritative index in `~/.zcode/cli/db/db.sqlite`:
//!
//! - `session(id, slug, directory, title, time_created, time_updated, …)`
//! - `message(id, session_id, time_created, data, sequence)` where `data` is
//!   `{"role":"user"|"assistant","time":{"created":ms,"completed":ms},…}`
//! - `part(id, message_id, session_id, time_created, data, sequence)` where
//!   `data` carries one typed block:
//!   `text` / `reasoning` / `tool` (`state.{input,output,error}`) / `file`,
//!   plus structural blocks (`step-start`, `step-finish`, `timeline`,
//!   `compaction`) that carry no conversation content.
//!
//! The database may be open (and being written) by a live ZCode process, so
//! this reader opens it read-only and never mutates it.

use std::path::Path;
use std::time::Duration;

use rusqlite::{Connection, OpenFlags};
use serde_json::Value;

use crate::convert::{
    args_to_string, content_parts_to_text, Exchange, RawBlock, AssistantTurn,
};
use crate::model::{ImportError, ImportedSession, SessionSummary, SourcePaths};

fn open_db(path: &Path) -> Result<Connection, ImportError> {
    if !path.exists() {
        return Err(ImportError::SourceUnavailable {
            source_id: "zcode",
            reason: format!("database not found at {}", path.display()),
        });
    }
    let conn = Connection::open_with_flags(
        path,
        OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_URI,
    )?;
    conn.busy_timeout(Duration::from_secs(10))?;
    Ok(conn)
}

fn parse_json_column(raw: &str) -> Option<Value> {
    serde_json::from_str(raw).ok()
}

/// List every session in the ZCode store, newest activity first.
pub fn list_sessions(paths: &SourcePaths) -> Result<Vec<SessionSummary>, ImportError> {
    let conn = open_db(&paths.zcode_db)?;
    let mut stmt = conn.prepare(
        "SELECT s.id, s.title, s.directory, s.time_created,
                (SELECT COUNT(*) FROM message m WHERE m.session_id = s.id) AS msg_count
         FROM session s
         ORDER BY s.time_updated DESC",
    )?;
    let rows = stmt.query_map([], |row| {
        let id: String = row.get(0)?;
        let title: Option<String> = row.get(1)?;
        let directory: Option<String> = row.get(2)?;
        let created: Option<i64> = row.get(3)?;
        let count: i64 = row.get(4).unwrap_or(0);
        Ok(SessionSummary {
            source_id: id,
            title: title.unwrap_or_default(),
            directory,
            created_at: created.map(crate::convert::ms_to_rfc3339),
            message_count: count.max(0) as usize,
            already_imported: false,
        })
    })?;
    let mut out = Vec::new();
    for row in rows {
        out.push(row?);
    }
    Ok(out)
}

/// Parse one ZCode session into OpenZen messages.
pub fn read_session(
    source_id: &str,
    paths: &SourcePaths,
) -> Result<ImportedSession, ImportError> {
    let conn = open_db(&paths.zcode_db)?;

    let (title, directory, created): (Option<String>, Option<String>, Option<i64>) = conn
        .query_row(
            "SELECT title, directory, time_created FROM session WHERE id = ?1",
            [source_id],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .map_err(|e| match e {
            rusqlite::Error::QueryReturnedNoRows => {
                ImportError::SessionNotFound(source_id.to_string())
            }
            other => ImportError::from(other),
        })?;

    // (message_id, role, data)
    let mut msg_stmt = conn.prepare(
        "SELECT id, data FROM message
         WHERE session_id = ?1
         ORDER BY COALESCE(sequence, 0) ASC, time_created ASC, id ASC",
    )?;
    let msg_rows: Vec<(String, Value)> = msg_stmt
        .query_map([source_id], |row| {
            let id: String = row.get(0)?;
            let data: String = row.get(1)?;
            Ok((id, data))
        })?
        .filter_map(Result::ok)
        .filter_map(|(id, raw)| parse_json_column(&raw).map(|v| (id, v)))
        .collect();

    let mut part_stmt = conn.prepare(
        "SELECT data FROM part
         WHERE message_id = ?1
         ORDER BY COALESCE(sequence, 0) ASC, time_created ASC, id ASC",
    )?;

    let mut exchanges: Vec<Exchange> = Vec::new();
    // The assistant text/reasoning/tool blocks accumulated for the current
    // user prompt. ZCode writes one assistant row per step; OpenZen renders
    // one assistant bubble per turn, so they are merged.
    let mut current: Option<Exchange> = None;
    let mut first_user_text: Option<String> = None;

    for (message_id, data) in &msg_rows {
        let role = data.get("role").and_then(Value::as_str).unwrap_or("");
        let created_ms = data
            .get("time")
            .and_then(|t| t.get("created"))
            .and_then(Value::as_i64);
        let parts: Vec<Value> = part_stmt
            .query_map([message_id], |row| row.get::<_, String>(0))?
            .filter_map(Result::ok)
            .filter_map(|raw| parse_json_column(&raw))
            .collect();

        match role {
            "user" => {
                let text = content_parts_to_text(&parts);
                let text = text.trim().to_string();
                // Injected system reminders are scaffolding, not conversation.
                if text.is_empty() || text.contains("<system-reminder>") {
                    continue;
                }
                if first_user_text.is_none() {
                    first_user_text = Some(first_user_text_from(&text));
                }
                if let Some(prev) = current.take() {
                    exchanges.push(prev);
                }
                current = Some(Exchange {
                    user_text: text,
                    user_ts_ms: created_ms,
                    assistant: None,
                });
            }
            "assistant" => {
                let Some(ex) = current.as_mut() else { continue };
                let turn = ex.assistant.get_or_insert_with(|| AssistantTurn {
                    ts_ms: created_ms,
                    ..Default::default()
                });
                if turn.ts_ms.is_none() {
                    turn.ts_ms = created_ms;
                }
                if let Some(model) = message_model(data) {
                    if turn.model.is_none() {
                        turn.model = Some(model);
                    }
                }
                if turn.provider.is_none() {
                    if let Some(p) = data.get("providerID").and_then(Value::as_str) {
                        turn.provider = Some(p.to_string());
                    }
                }
                if let Some(tokens) = data.get("tokens") {
                    if turn.tokens_in.is_none() {
                        turn.tokens_in = tokens.get("input").and_then(Value::as_i64);
                    }
                    if turn.tokens_out.is_none() {
                        turn.tokens_out = tokens.get("output").and_then(Value::as_i64);
                    }
                }
                // Duration = finish time of this step minus the turn's start.
                if let Some(completed) = data
                    .get("time")
                    .and_then(|t| t.get("completed"))
                    .and_then(Value::as_i64)
                {
                    if let Some(start) = turn.ts_ms {
                        let d = completed.saturating_sub(start).max(0);
                        turn.duration_ms = Some(turn.duration_ms.unwrap_or(0).max(d));
                    }
                }
                for part in &parts {
                    turn.blocks.extend(zcode_part_to_blocks(part));
                }
            }
            _ => {}
        }
    }
    if let Some(prev) = current.take() {
        exchanges.push(prev);
    }

    let title = title
        .map(|t| t.trim().to_string())
        .filter(|t| !t.is_empty())
        .or(first_user_text)
        .unwrap_or_else(|| source_id.to_string());

    Ok(ImportedSession {
        source_id: source_id.to_string(),
        title,
        directory,
        created_at: created.and_then(chrono::DateTime::from_timestamp_millis),
        messages: crate::convert::exchanges_to_messages(&exchanges),
    })
}

fn message_model(data: &Value) -> Option<String> {
    if let Some(m) = data.get("modelID").and_then(Value::as_str) {
        if !m.is_empty() {
            return Some(m.to_string());
        }
    }
    data.get("model")
        .and_then(Value::as_str)
        .filter(|m| !m.is_empty())
        .map(str::to_string)
}

/// First line of the prompt, trimmed to a sane session title length.
fn first_user_text_from(text: &str) -> String {
    let first = text.lines().find(|l| !l.trim().is_empty()).unwrap_or(text);
    let mut s: String = first.trim().chars().take(80).collect();
    if first.trim().chars().count() > 80 {
        s.push('…');
    }
    s
}

/// A single ZCode `part` row maps to zero, one, or two neutral blocks: a
/// `tool` part carries both the call and its (already settled) result, and
/// both must be emitted so the frontend can pair them by `tool_call_id`.
fn zcode_part_to_blocks(part: &Value) -> Vec<RawBlock> {
    let Some(kind) = part.get("type").and_then(Value::as_str) else {
        return Vec::new();
    };
    match kind {
        "text" => {
            let text = part.get("text").and_then(Value::as_str).unwrap_or("");
            if text.is_empty() {
                Vec::new()
            } else {
                vec![RawBlock::Text(text.to_string())]
            }
        }
        "reasoning" => {
            let text = part.get("text").and_then(Value::as_str).unwrap_or("");
            if text.is_empty() {
                Vec::new()
            } else {
                vec![RawBlock::Reasoning(text.to_string())]
            }
        }
        "tool" => {
            let id = part
                .get("callID")
                .and_then(Value::as_str)
                .unwrap_or("tool")
                .to_string();
            let name = part
                .get("tool")
                .and_then(Value::as_str)
                .unwrap_or("unknown")
                .to_string();
            let state = part.get("state").cloned().unwrap_or(Value::Null);
            let args = args_to_string(state.get("input").unwrap_or(&Value::Null));
            // `error` is populated when status is error/failed and `output`
            // is absent; surface whichever exists so the card is never blank.
            let output = match state.get("output") {
                Some(Value::Null) | None => state
                    .get("error")
                    .map(output_to_string)
                    .unwrap_or_default(),
                Some(v) => output_to_string(v),
            };
            // The call and its result are emitted together: a ZCode `tool`
            // part is a single row that is updated in place with the settled
            // state, so both halves are always available here. A call with no
            // output still gets an (empty) result so the frontend can close
            // the card — parts.ts pairs strictly by `tool_call_id`.
            vec![
                RawBlock::ToolCall {
                    id: id.clone(),
                    name,
                    args,
                },
                RawBlock::ToolResult { id, output },
            ]
        }
        "file" => {
            let name = part
                .get("filename")
                .and_then(Value::as_str)
                .unwrap_or("unknown");
            vec![RawBlock::Text(format!("[file: {name}]"))]
        }
        // step-start / step-finish / timeline / compaction are structural.
        _ => Vec::new(),
    }
}

fn output_to_string(v: &Value) -> String {
    match v {
        Value::String(s) => s.clone(),
        other => serde_json::to_string_pretty(other).unwrap_or_else(|_| other.to_string()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn structural_parts_are_skipped() {
        assert!(zcode_part_to_blocks(&serde_json::json!({"type":"step-start"})).is_empty());
        assert!(zcode_part_to_blocks(&serde_json::json!({"type":"step-finish"})).is_empty());
        assert!(zcode_part_to_blocks(&serde_json::json!({"type":"timeline"})).is_empty());
        assert!(zcode_part_to_blocks(&serde_json::json!({"type":"compaction"})).is_empty());
    }

    #[test]
    fn text_and_reasoning_parts_map_through() {
        match &zcode_part_to_blocks(&serde_json::json!({"type":"text","text":"hi"}))[..] {
            [RawBlock::Text(t)] => assert_eq!(t, "hi"),
            other => panic!("unexpected {other:?}"),
        }
        match &zcode_part_to_blocks(&serde_json::json!({"type":"reasoning","text":"why"}))[..] {
            [RawBlock::Reasoning(t)] => assert_eq!(t, "why"),
            other => panic!("unexpected {other:?}"),
        }
    }

    /// Regression: the tool arm must emit BOTH halves, sharing the call id.
    #[test]
    fn tool_part_emits_call_and_paired_result() {
        let part = serde_json::json!({
            "type": "tool",
            "callID": "call_abc",
            "tool": "Read",
            "state": {
                "status": "completed",
                "input": {"filePath": "/tmp/x"},
                "output": "contents"
            }
        });
        let blocks = zcode_part_to_blocks(&part);
        assert_eq!(blocks.len(), 2, "expected call + result, got {blocks:?}");
        match &blocks[0] {
            RawBlock::ToolCall { id, name, args } => {
                assert_eq!(id, "call_abc");
                assert_eq!(name, "Read");
                assert!(args.contains("filePath"));
            }
            other => panic!("expected ToolCall, got {other:?}"),
        }
        match &blocks[1] {
            RawBlock::ToolResult { id, output } => {
                assert_eq!(id, "call_abc");
                assert_eq!(output, "contents");
            }
            other => panic!("expected ToolResult, got {other:?}"),
        }
    }

    #[test]
    fn failed_tool_part_surfaces_error_as_output() {
        let part = serde_json::json!({
            "type": "tool",
            "callID": "call_err",
            "tool": "Bash",
            "state": {"status": "error", "input": {}, "output": null, "error": "boom"}
        });
        match &zcode_part_to_blocks(&part)[..] {
            [RawBlock::ToolCall { .. }, RawBlock::ToolResult { output, .. }] => {
                assert_eq!(output, "boom")
            }
            other => panic!("unexpected {other:?}"),
        }
    }

    #[test]
    fn title_falls_back_to_first_line() {
        assert_eq!(first_user_text_from("hello\nworld"), "hello");
        assert_eq!(first_user_text_from("   \n  second  "), "second");
    }

    #[test]
    fn missing_db_reports_unavailable() {
        let paths = SourcePaths::from_home("/nonexistent-home-xyz");
        let err = list_sessions(&paths).unwrap_err();
        assert!(matches!(err, ImportError::SourceUnavailable { .. }));
    }
}
