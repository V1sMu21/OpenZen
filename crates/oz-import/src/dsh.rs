//! Reader for DeepSeek Harness (DSH) session logs.
//!
//! Layout: `~/.dsh/sessions/<cwd-slug>/<session-id>/session[.vN].jsonl[.zstd]`.
//! Each line is one event `{type, seq, time, data}`.
//!
//! ## The concatenated-frame trap
//!
//! DSH appends to a `.jsonl.zstd` file by writing a **new zstd frame per
//! flush**, so the file is a chain of independent frames rather than one
//! stream. A decoder that stops at the first frame end yields only the
//! 291-byte session header and makes every session look empty. The `zstd`
//! crate's [`zstd::stream::read::Decoder`] walks concatenated frames by
//! default (`single_frame()` is opt-in), so streaming decode is the correct
//! path — do not switch to a one-shot frame decoder here.
//!
//! ## Relevant events
//!
//! - `session` — header; `id` / `createdAt` / `cwd` / `origin` sit at the
//!   TOP LEVEL of the event, not inside `data`.
//! - `user/message` — `data.content` = `[{type:"text",text}]`.
//! - `assistant/message` — `data.message.content` = ordered
//!   `reasoning` / `text` / `tool-call` blocks, and
//!   `data.message.source` = `{kind:"model",provider,model}`.
//! - `tool/result` — `data.message.content[0]` =
//!   `{type:"tool-result",toolCallId,content:[{type:"text",text}]}`.
//! - `session/title` — `data.title` / `data.source.kind`
//!   (`"user"` for a real title, `"fallback"` for a truncated message).
//!
//! `tool/call` events duplicate the `tool-call` block already present in the
//! owning `assistant/message` (verified: 81 vs 81, full overlap), so they are
//! only consumed as a fallback for call ids never seen in a message.

use std::collections::HashSet;
use std::fs::File;
use std::io::{BufReader, Read};
use std::path::{Path, PathBuf};

use serde_json::Value;

use crate::convert::{args_to_string, content_parts_to_text, AssistantTurn, Exchange, RawBlock};
use crate::model::{ImportError, ImportedSession, SessionSummary, SourcePaths};

/// Extensions/versions a session file may use. Returns the version number, or
/// `None` when the name is not a DSH session log (`session.lock`, etc.).
fn session_file_version(name: &str) -> Option<u32> {
    let lower = name.to_ascii_lowercase();
    let rest = lower.strip_prefix("session")?;
    let rest = rest.strip_suffix(".zstd").unwrap_or(rest);
    let rest = rest.strip_suffix(".jsonl")?;
    if rest.is_empty() {
        return Some(0); // `session.jsonl`
    }
    let v = rest.strip_prefix(".v")?;
    // Reject `session.v01` / `session.v0` — the real format starts at v1.
    if v.starts_with('0') {
        return None;
    }
    v.parse::<u32>().ok().filter(|n| *n >= 1)
}

/// Find the newest log file per session directory.
fn discover_files(root: &Path) -> Result<Vec<PathBuf>, ImportError> {
    if !root.is_dir() {
        return Err(ImportError::SourceUnavailable {
            source_id: "dsh",
            reason: format!("sessions directory not found at {}", root.display()),
        });
    }
    let mut out = Vec::new();
    for project in std::fs::read_dir(root)? {
        let project = project?;
        if !project.file_type()?.is_dir() {
            continue;
        }
        for session in std::fs::read_dir(project.path())? {
            let session = session?;
            if !session.file_type()?.is_dir() {
                continue;
            }
            let mut best: Option<(u32, PathBuf)> = None;
            for entry in std::fs::read_dir(session.path())? {
                let entry = entry?;
                if !entry.file_type()?.is_file() {
                    continue;
                }
                let name = entry.file_name().to_string_lossy().to_string();
                if let Some(version) = session_file_version(&name) {
                    if best.as_ref().is_none_or(|(v, _)| version > *v) {
                        best = Some((version, entry.path()));
                    }
                }
            }
            if let Some((_, path)) = best {
                out.push(path);
            }
        }
    }
    out.sort();
    Ok(out)
}

/// Decode a session log to text.
///
/// `.zstd` logs are chains of concatenated frames; the streaming decoder
/// joins them (see the module docs).
fn read_log(path: &Path) -> Result<String, ImportError> {
    let file = File::open(path)?;
    let mut bytes = Vec::new();
    let is_zstd = path
        .extension()
        .and_then(|e| e.to_str())
        .is_some_and(|e| e.eq_ignore_ascii_case("zstd"));
    if is_zstd {
        let mut decoder = zstd::stream::read::Decoder::new(BufReader::new(file))?;
        decoder.read_to_end(&mut bytes)?;
    } else {
        BufReader::new(file).read_to_end(&mut bytes)?;
    }
    Ok(String::from_utf8_lossy(&bytes).into_owned())
}

fn read_events(path: &Path) -> Result<Vec<Value>, ImportError> {
    let text = read_log(path)?;
    Ok(text
        .lines()
        .filter(|l| !l.trim().is_empty())
        .filter_map(|l| serde_json::from_str::<Value>(l).ok())
        .collect())
}

/// Strip `<system-reminder>…</system-reminder>` scaffolding that the harness
/// appends to prompts. Returns the remaining prose.
pub fn strip_system_reminders(text: &str) -> String {
    const OPEN: &str = "<system-reminder>";
    const CLOSE: &str = "</system-reminder>";
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(start) = rest.find(OPEN) {
        out.push_str(&rest[..start]);
        match rest[start..].find(CLOSE) {
            Some(end) => rest = &rest[start + end + CLOSE.len()..],
            None => {
                // Unterminated block: drop the remainder.
                rest = "";
                break;
            }
        }
    }
    out.push_str(rest);
    out
}

struct ParsedDsh {
    title: Option<String>,
    cwd: Option<String>,
    created_at: Option<i64>,
    origin: Option<String>,
    exchanges: Vec<Exchange>,
}

/// Parse one event stream. `collect_blocks = false` skips building message
/// bodies, which keeps the listing pass cheap.
fn parse_events(events: &[Value], collect_blocks: bool) -> ParsedDsh {
    let mut parsed = ParsedDsh {
        title: None,
        cwd: None,
        created_at: None,
        origin: None,
        exchanges: Vec::new(),
    };

    let mut current: Option<Exchange> = None;
    let mut seen_calls: HashSet<String> = HashSet::new();
    let mut turn_last_ts: Option<i64> = None;

    for event in events {
        let kind = event.get("type").and_then(Value::as_str).unwrap_or("");
        let data = event.get("data").cloned().unwrap_or(Value::Null);
        let time = event.get("time").and_then(Value::as_i64);

        match kind {
            "session" => {
                // Header fields are top-level on this event.
                parsed.created_at = event.get("createdAt").and_then(Value::as_i64);
                parsed.cwd = event
                    .get("cwd")
                    .and_then(Value::as_str)
                    .map(str::to_string);
                parsed.origin = event
                    .get("origin")
                    .and_then(Value::as_str)
                    .map(str::to_string);
            }
            "session/title" => {
                let title = data.get("title").and_then(Value::as_str).unwrap_or("");
                let source_kind = data
                    .get("source")
                    .and_then(|s| s.get("kind"))
                    .and_then(Value::as_str)
                    .unwrap_or("");
                // A `fallback` title is a truncated first message rather than
                // a real title: better than nothing, but it never overrides
                // one that already resolved.
                if !title.trim().is_empty()
                    && (source_kind != "fallback" || parsed.title.is_none())
                {
                    parsed.title = Some(title.trim().to_string());
                }
            }
            "user/message" => {
                let content = data.get("content").cloned().unwrap_or(Value::Null);
                let parts = content.as_array().cloned().unwrap_or_default();
                let text = strip_system_reminders(&content_parts_to_text(&parts));
                let text = text.trim().to_string();
                if text.is_empty() {
                    continue;
                }
                if let Some(prev) = current.take() {
                    parsed.exchanges.push(prev);
                }
                seen_calls.clear();
                turn_last_ts = None;
                current = Some(Exchange {
                    user_text: text,
                    user_ts_ms: time,
                    assistant: None,
                });
            }
            "assistant/message" => {
                let Some(ex) = current.as_mut() else { continue };
                let message = data.get("message").cloned().unwrap_or(Value::Null);
                let turn = ex.assistant.get_or_insert_with(|| AssistantTurn {
                    ts_ms: time,
                    ..Default::default()
                });
                if turn.ts_ms.is_none() {
                    turn.ts_ms = time;
                }
                if let Some(t) = time {
                    turn_last_ts = Some(turn_last_ts.map_or(t, |p| p.max(t)));
                }
                if let Some(source) = message.get("source") {
                    if turn.model.is_none() {
                        turn.model = source
                            .get("model")
                            .and_then(Value::as_str)
                            .map(str::to_string);
                    }
                    if turn.provider.is_none() {
                        turn.provider = source
                            .get("provider")
                            .and_then(Value::as_str)
                            .map(str::to_string);
                    }
                }
                if !collect_blocks {
                    continue;
                }
                for block in message
                    .get("content")
                    .and_then(Value::as_array)
                    .map(Vec::as_slice)
                    .unwrap_or_default()
                {
                    match block.get("type").and_then(Value::as_str).unwrap_or("") {
                        "text" => {
                            let text = block.get("text").and_then(Value::as_str).unwrap_or("");
                            if !text.is_empty() {
                                turn.blocks.push(RawBlock::Text(text.to_string()));
                            }
                        }
                        "reasoning" => {
                            let text = block.get("text").and_then(Value::as_str).unwrap_or("");
                            if !text.is_empty() {
                                turn.blocks.push(RawBlock::Reasoning(text.to_string()));
                            }
                        }
                        "tool-call" => {
                            let id = block
                                .get("id")
                                .and_then(Value::as_str)
                                .unwrap_or("tool")
                                .to_string();
                            let name = block
                                .get("name")
                                .and_then(Value::as_str)
                                .unwrap_or("unknown")
                                .to_string();
                            let args = args_to_string(
                                block.get("arguments").unwrap_or(&Value::Null),
                            );
                            seen_calls.insert(id.clone());
                            turn.blocks.push(RawBlock::ToolCall { id, name, args });
                        }
                        _ => {}
                    }
                }
            }
            "tool/call" => {
                // Fallback only: these normally duplicate a `tool-call` block
                // already emitted from the owning `assistant/message`.
                if !collect_blocks {
                    continue;
                }
                let Some(ex) = current.as_mut() else { continue };
                let id = data
                    .get("callId")
                    .and_then(Value::as_str)
                    .unwrap_or("tool")
                    .to_string();
                if !seen_calls.insert(id.clone()) {
                    continue;
                }
                let name = data
                    .get("name")
                    .and_then(Value::as_str)
                    .unwrap_or("unknown")
                    .to_string();
                let args = args_to_string(data.get("arguments").unwrap_or(&Value::Null));
                let turn = ex.assistant.get_or_insert_with(AssistantTurn::default);
                turn.blocks.push(RawBlock::ToolCall { id, name, args });
            }
            "tool/result" => {
                if !collect_blocks {
                    continue;
                }
                let Some(ex) = current.as_mut() else { continue };
                if let Some(t) = time {
                    turn_last_ts = Some(turn_last_ts.map_or(t, |p| p.max(t)));
                }
                let message = data.get("message").cloned().unwrap_or(Value::Null);
                let Some(blocks) = message.get("content").and_then(Value::as_array) else {
                    continue;
                };
                let turn = ex.assistant.get_or_insert_with(AssistantTurn::default);
                for block in blocks {
                    if block.get("type").and_then(Value::as_str) != Some("tool-result") {
                        continue;
                    }
                    let id = block
                        .get("toolCallId")
                        .and_then(Value::as_str)
                        .unwrap_or("tool")
                        .to_string();
                    turn.blocks.push(RawBlock::ToolResult {
                        id,
                        output: tool_result_text(block),
                    });
                }
            }
            _ => {}
        }
    }

    if let Some(prev) = current.take() {
        parsed.exchanges.push(prev);
    }
    if let (Some(last), Some(ex)) = (turn_last_ts, parsed.exchanges.last_mut()) {
        if let Some(turn) = ex.assistant.as_mut() {
            if let Some(start) = turn.ts_ms {
                turn.duration_ms = Some((last - start).max(0));
            }
        }
    }
    parsed
}

/// Flatten a `tool-result` block's nested `content` array to text.
fn tool_result_text(block: &Value) -> String {
    match block.get("content") {
        Some(Value::String(s)) => s.clone(),
        Some(Value::Array(items)) => {
            let mut out = String::new();
            for item in items {
                if let Some(t) = item.get("text").and_then(Value::as_str) {
                    if !out.is_empty() {
                        out.push('\n');
                    }
                    out.push_str(t);
                }
            }
            out
        }
        Some(other) => other.to_string(),
        None => String::new(),
    }
}

fn fallback_title(exchanges: &[Exchange]) -> Option<String> {
    let first = exchanges
        .iter()
        .map(|e| e.user_text.trim())
        .find(|t| !t.is_empty())?;
    let line = first.lines().find(|l| !l.trim().is_empty()).unwrap_or(first);
    let mut s: String = line.trim().chars().take(80).collect();
    if line.trim().chars().count() > 80 {
        s.push('…');
    }
    Some(s)
}

/// List importable DSH sessions, newest first.
pub fn list_sessions(paths: &SourcePaths) -> Result<Vec<SessionSummary>, ImportError> {
    let files = discover_files(&paths.dsh_sessions_dir)?;
    let mut out = Vec::new();
    for path in files {
        let Some(source_id) = path
            .parent()
            .and_then(|p| p.file_name())
            .map(|n| n.to_string_lossy().to_string())
        else {
            continue;
        };
        let Ok(events) = read_events(&path) else {
            continue;
        };
        let parsed = parse_events(&events, false);
        if parsed.exchanges.is_empty() {
            continue; // header-only / seeded session with no conversation
        }
        out.push(SessionSummary {
            source_id,
            title: parsed
                .title
                .clone()
                .filter(|t| !t.is_empty())
                .or_else(|| fallback_title(&parsed.exchanges))
                .unwrap_or_else(|| path.display().to_string()),
            directory: parsed.cwd.clone(),
            created_at: parsed.created_at.map(crate::convert::ms_to_rfc3339),
            message_count: count_messages(&parsed.exchanges),
            already_imported: false,
        });
    }
    out.sort_by(|a, b| b.created_at.cmp(&a.created_at));
    Ok(out)
}

fn count_messages(exchanges: &[Exchange]) -> usize {
    exchanges
        .iter()
        .map(|e| usize::from(!e.user_text.trim().is_empty()) + usize::from(e.assistant.is_some()))
        .sum()
}

/// Parse one DSH session into OpenZen messages.
pub fn read_session(
    source_id: &str,
    paths: &SourcePaths,
) -> Result<ImportedSession, ImportError> {
    let files = discover_files(&paths.dsh_sessions_dir)?;
    let path = files
        .into_iter()
        .find(|p| {
            p.parent()
                .and_then(|d| d.file_name())
                .is_some_and(|n| n.to_string_lossy() == source_id)
        })
        .ok_or_else(|| ImportError::SessionNotFound(source_id.to_string()))?;

    let events = read_events(&path)?;
    let parsed = parse_events(&events, true);
    let title = parsed
        .title
        .clone()
        .filter(|t| !t.is_empty())
        .or_else(|| fallback_title(&parsed.exchanges))
        .unwrap_or_else(|| source_id.to_string());

    Ok(ImportedSession {
        source_id: source_id.to_string(),
        title,
        directory: parsed.cwd,
        created_at: parsed.created_at.and_then(chrono::DateTime::from_timestamp_millis),
        messages: crate::convert::exchanges_to_messages(&parsed.exchanges),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn recognises_session_file_versions() {
        assert_eq!(session_file_version("session.jsonl"), Some(0));
        assert_eq!(session_file_version("session.jsonl.zstd"), Some(0));
        assert_eq!(session_file_version("session.v3.jsonl.zstd"), Some(3));
        assert_eq!(session_file_version("session.v12.jsonl"), Some(12));
        assert_eq!(session_file_version("session.v0.jsonl"), None);
        assert_eq!(session_file_version("session.v01.jsonl"), None);
        assert_eq!(session_file_version("session.lock"), None);
        assert_eq!(session_file_version("events.jsonl"), None);
    }

    #[test]
    fn strips_system_reminder_blocks() {
        assert_eq!(strip_system_reminders("hello"), "hello");
        assert_eq!(
            strip_system_reminders("<system-reminder>noise</system-reminder>keep me"),
            "keep me"
        );
        assert_eq!(
            strip_system_reminders("before<system-reminder>x</system-reminder>after"),
            "beforeafter"
        );
        // Unterminated reminder must not leak into the conversation.
        assert_eq!(strip_system_reminders("prompt<system-reminder>dangling"), "prompt");
    }

    /// A whole session built from synthetic events: reasoning + text +
    /// tool-call, with the duplicate `tool/call` and a matching result.
    fn sample_events() -> Vec<Value> {
        vec![
            json!({"type":"session","version":3,"id":"abc","createdAt":1_700_000_000_000i64,
                   "cwd":"/tmp/work","delegationDepth":0}),
            json!({"type":"user/message","seq":2,"time":1_700_000_000_100i64,
                   "data":{"role":"user","content":[{"type":"text","text":"list files"}]}}),
            json!({"type":"assistant/message","seq":4,"time":1_700_000_000_200i64,
                   "data":{"turn":1,"step":1,"message":{"role":"assistant","source":{"kind":"model","provider":"p","model":"m"},
                   "content":[{"type":"reasoning","text":"thinking"},
                              {"type":"text","text":"sure"},
                              {"type":"tool-call","id":"call_1","name":"Bash","arguments":"{\"command\":\"ls\"}"}]}}}),
            json!({"type":"tool/call","seq":5,"time":1_700_000_000_200i64,
                   "data":{"turn":1,"step":1,"callId":"call_1","name":"Bash","arguments":"{\"command\":\"ls\"}"}}),
            json!({"type":"tool/result","seq":6,"time":1_700_000_000_900i64,
                   "data":{"turn":1,"step":1,"message":{"role":"user",
                   "content":[{"type":"tool-result","toolCallId":"call_1",
                               "content":[{"type":"text","text":"a.txt"}]}]}}}),
            json!({"type":"session/title","seq":9,"time":1_700_000_000_900i64,
                   "data":{"title":"Real title","source":{"kind":"user"}}}),
        ]
    }

    #[test]
    fn parses_a_session_into_exchanges() {
        let parsed = parse_events(&sample_events(), true);
        assert_eq!(parsed.exchanges.len(), 1);
        assert_eq!(parsed.cwd.as_deref(), Some("/tmp/work"));
        assert_eq!(parsed.title.as_deref(), Some("Real title"));

        let turn = parsed.exchanges[0].assistant.as_ref().unwrap();
        assert_eq!(turn.model.as_deref(), Some("m"));
        // Exactly one tool call: the `tool/call` duplicate must be dropped.
        let calls = turn
            .blocks
            .iter()
            .filter(|b| matches!(b, RawBlock::ToolCall { .. }))
            .count();
        assert_eq!(calls, 1, "duplicate tool/call was not deduped");
        assert_eq!(turn.duration_ms, Some(700));
    }

    #[test]
    fn plugin_system_reminder_messages_are_dropped() {
        let events = vec![
            json!({"type":"session","version":3,"id":"abc","createdAt":1i64,"cwd":"/tmp"}),
            json!({"type":"user/message","seq":1,"time":1i64,
                   "data":{"role":"user","source":{"kind":"plugin"},
                   "content":[{"type":"text","text":"<system-reminder>env notice</system-reminder>"}]}}),
        ];
        let parsed = parse_events(&events, true);
        assert!(parsed.exchanges.is_empty());
    }

    #[test]
    fn missing_root_reports_unavailable() {
        let paths = SourcePaths::from_home("/nonexistent-home-xyz");
        let err = list_sessions(&paths).unwrap_err();
        assert!(matches!(err, ImportError::SourceUnavailable { .. }));
    }
}
