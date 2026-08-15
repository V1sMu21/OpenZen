use std::sync::atomic::{AtomicU64, Ordering};

use oz_core_types::{ContentBlock, LlmError, StreamEvent, TokenUsage};
use tokio::sync::mpsc::UnboundedSender;

// ── Block ID generator ────────────────────────────────────────────────
//
// Each parser call needs to mint stable IDs for streaming text blocks,
// reasoning blocks, and tool calls. We use a process-wide AtomicU64
// counter so IDs are unique across concurrent parses (rare but
// possible when the agent loop spawns two LLM calls back-to-back).

static BLOCK_ID_COUNTER: AtomicU64 = AtomicU64::new(0);

fn next_id(prefix: &str) -> String {
    let n = BLOCK_ID_COUNTER.fetch_add(1, Ordering::Relaxed);
    format!("{prefix}{n}")
}

fn emit(tx: &Option<UnboundedSender<StreamEvent>>, event: StreamEvent) {
    if let Some(tx) = tx {
        let _ = tx.send(event);
    }
}

/// Internal tags stripped from chat_completions streams. Each
/// provider wraps reasoning in its own dialect; broad coverage
/// keeps replies in the visible channel for Qwen3 / MiniMax / GLM /
/// Step / Anthropic-style local backends.
const INTERNAL_TAGS: &[&str] = &[
    "antThinking",
    "thinking",
    "reasoning",
    "summary",
    "answer",
    "final",
    "response",
    "output",
    "result",
    "reply",
    "conclusion",
    "tool_code",
    "tool_call",
    "respond",
];

/// Find the next opening `<tag…>` (case-insensitive) in `text`, or
/// `<tag…/>` self-closing. Returns:
///   - `Some((None, start, tag_name))` for an open-tag form, OR
///   - `Some((Some(end), start, tag_name))` for a self-closing form,
///     where `end` is the byte offset just past the `/>` so the caller
///     can skip it.
fn find_next_internal_tag(text: &str) -> Option<(Option<usize>, usize, String)> {
    let bytes = text.as_bytes();
    let mut best: Option<(usize, usize, String, bool)> = None; // (start, open_tag_len, name, is_self_closing)
    for tag in INTERNAL_TAGS {
        let needle = format!("<{}", tag);
        let lower = text.to_ascii_lowercase();
        let search = lower.as_str();
        let mut from = 0;
        while let Some(pos) = search[from..].find(&needle.to_ascii_lowercase()) {
            let abs = from + pos;
            // Check that this is an open-tag boundary (i.e. the next
            // char is `>`, `/`, or whitespace, or end-of-tag-attribute
            // characters). The matched substring is `<tag`; we need
            // either `>`, `/>`, or attributes before `>`.
            let after = abs + needle.len();
            if after < bytes.len() {
                let c = bytes[after];
                if c == b'>' || c == b'/' || c == b' ' || c == b'\t' || c == b'\n' || c == b'\r' {
                    // Find the end of this opening tag
                    let close_rel = search[after..].find('>');
                    if let Some(close_abs) = close_rel.map(|x| x + after) {
                        // Self-closing if the char before `>` is `/`
                        let is_self_closing = bytes.get(close_abs.wrapping_sub(1)) == Some(&b'/');
                        let start = abs;
                        if best.as_ref().is_none_or(|(b, _, _, _)| start < *b) {
                            best = Some((start, close_abs + 1 - start, tag.to_string(), is_self_closing));
                        }
                        break;
                    }
                }
            }
            from = abs + 1;
        }
    }
    best.map(|(start, open_len, name, self_closing)| {
        let end_after = if self_closing { Some(start + open_len) } else { None };
        (end_after, start, name)
    })
}

/// Length of the opening tag at the start of `text`, including the
/// trailing `>`. Returns 0 if the text doesn't start with a known
/// opening tag.
fn find_open_tag_len(text: &str) -> usize {
    let lower = text.to_ascii_lowercase();
    for tag in INTERNAL_TAGS {
        let needle = format!("<{}", tag);
        if lower.starts_with(&needle) {
            // Need to be at an open-tag boundary.
            if let Some(c) = text.as_bytes().get(needle.len()).copied() {
                if c == b'>' || c == b' ' || c == b'\t' || c == b'\n' || c == b'\r' {
                    if let Some(rel) = lower.find('>') {
                        return rel + 1;
                    }
                }
            }
        }
    }
    0
}

/// Find the byte offset of the closing `</tag>` for the named tag,
/// case-insensitive. Returns `None` if not found in `text`.
fn find_close_tag(text: &str, tag: &str) -> Option<usize> {
    let lower = text.to_ascii_lowercase();
    let needle = format!("</{}>", tag.to_ascii_lowercase());
    lower.find(&needle).map(|p| p + needle.len())
}

/// Find the earliest `</tag>` of any internal tag in `text`. Used as
/// a recovery path when the model writes a stray close tag inside a
/// mismatched open — e.g. `</summary>` inside `<thinking>` — which
/// would otherwise leave the rest of the response trapped in the
/// reasoning channel.
fn find_any_close_tag(text: &str) -> Option<(usize, String)> {
    let lower = text.to_ascii_lowercase();
    let mut best: Option<(usize, String)> = None;
    for tag in INTERNAL_TAGS {
        let needle = format!("</{}>", tag.to_ascii_lowercase());
        if let Some(p) = lower.find(&needle) {
            if best.as_ref().is_none_or(|(b, _)| p < *b) {
                best = Some((p, tag.to_string()));
            }
        }
    }
    best
}

// ── Claude parser ─────────────────────────────────────────────────────

/// Max seconds to wait between consecutive SSE chunks before declaring
/// the stream stalled. Local quantized models (omlx/ollama) can prefill
/// for minutes without emitting a chunk, so they get a much larger
/// ceiling than the 60s used for cloud APIs.
fn chunk_timeout_secs(apibase: &str) -> u64 {
    if crate::is_local_apibase(apibase) { 300 } else { 60 }
}

/// Parse Claude SSE stream. Emits typed start-delta-end protocol events
/// (`TextStart`/`TextDelta`/`TextEnd`, `ReasoningStart`/`ReasoningDelta`/
/// `ReasoningEnd`, `ToolInputStart`/`ToolInputDelta`/`ToolInputAvailable`)
/// directly to `event_tx` for downstream consumers (TUI, Tauri, WebUI SSE).
pub async fn parse_claude_sse(
    resp: reqwest::Response,
    event_tx: Option<UnboundedSender<StreamEvent>>,
    apibase: &str,
) -> Result<(Vec<ContentBlock>, Option<TokenUsage>), LlmError> {
    let mut content_blocks: Vec<ContentBlock> = Vec::new();
    let mut current_block: Option<serde_json::Value> = None;
    let mut tool_json_buf = String::new();
    let mut _stop_reason: Option<String> = None;
    let mut usage: Option<TokenUsage> = None;
    let mut got_message_stop = false;
    let mut warn: Option<String> = None;

    // Per-block open IDs. Emitted in *Start, referenced by deltas, closed
    // in *End. Only one of each kind is open at a time.
    let mut current_text_id: Option<String> = None;
    let mut current_reasoning_id: Option<String> = None;
    // For tool_use, the open ID is also the tool_call_id.
    let mut current_tool_call_id: Option<String> = None;
    let mut current_tool_name: Option<String> = None;

    use futures::StreamExt;

    let timeout_secs = chunk_timeout_secs(apibase);

    let mut stream = resp.bytes_stream();
    let mut buffer = String::new();

    loop {
        let chunk = tokio::time::timeout(
            std::time::Duration::from_secs(timeout_secs),
            stream.next(),
        )
        .await
        .map_err(|_| LlmError::StreamError(
            format!("Claude SSE stream timed out (no data for {}s)", timeout_secs)
        ))?;
        let chunk = match chunk {
            Some(c) => c.map_err(|e| LlmError::StreamError(e.to_string()))?,
            None => break,
        };
        buffer.push_str(&String::from_utf8_lossy(&chunk));

        while let Some(newline) = buffer.find('\n') {
            let line = buffer[..newline].to_string();
            buffer = buffer[newline + 1..].to_string();
            let line = line.trim().to_string();
            if line.is_empty() { continue; }
            if !line.starts_with("data:") { continue; }

            let data_str = line[5..].trim().to_string();
            if data_str == "[DONE]" {
                // Flush any open blocks before returning (same as end-of-stream cleanup below).
                if let Some(id) = current_text_id.take() {
                    emit(&event_tx, StreamEvent::TextEnd { id });
                }
                if let Some(id) = current_reasoning_id.take() {
                    emit(&event_tx, StreamEvent::ReasoningEnd { id });
                }
                if let (Some(tc_id), Some(name)) = (current_tool_call_id.take(), current_tool_name.take()) {
                    emit(&event_tx, StreamEvent::ToolInputAvailable {
                        tool_call_id: tc_id,
                        name,
                        args: std::mem::take(&mut tool_json_buf),
                    });
                }
                return Ok((content_blocks, usage));
            }

            let evt: serde_json::Value = match serde_json::from_str(&data_str) {
                Ok(v) => v,
                Err(e) => {
                    tracing::warn!("[SSE] JSON parse error: {e}, line: {}", &data_str[..data_str.len().min(200)]);
                    continue;
                }
            };

            let evt_type = evt["type"].as_str().unwrap_or("");

            match evt_type {
                "message_start" => {
                    if let Some(u) = evt["message"]["usage"].as_object() {
                        let input = u.get("input_tokens").and_then(|v| v.as_u64()).unwrap_or(0);
                        let output = u.get("output_tokens").and_then(|v| v.as_u64()).unwrap_or(0);
                        if input > 0 || output > 0 {
                            usage = Some(TokenUsage {
                                input_tokens: input,
                                output_tokens: output,
                                total_tokens: Some(input + output),
                            });
                        }
                    }
                }
                "content_block_start" => {
                    current_block = Some(evt["content_block"].clone());
                    if let Some(block) = current_block.as_ref() {
                        match block["type"].as_str() {
                            Some("tool_use") => {
                                tool_json_buf.clear();
                                let name = block["name"].as_str().unwrap_or("").to_string();
                                let id = block["id"].as_str().unwrap_or("").to_string();
                                // Use the upstream-provided tool call id if any,
                                // otherwise mint our own.
                                let tc_id = if !id.is_empty() { id } else { next_id("tc") };
                                current_tool_call_id = Some(tc_id.clone());
                                current_tool_name = Some(name.clone());
                                emit(&event_tx, StreamEvent::ToolInputStart {
                                    tool_call_id: tc_id,
                                    name,
                                });
                            }
                            Some("text") => {
                                let id = next_id("t");
                                current_text_id = Some(id.clone());
                                emit(&event_tx, StreamEvent::TextStart { id });
                            }
                            Some("thinking") => {
                                let id = next_id("r");
                                current_reasoning_id = Some(id.clone());
                                emit(&event_tx, StreamEvent::ReasoningStart { id });
                            }
                            _ => {}
                        }
                    }
                }
                "content_block_delta" => {
                    let delta = &evt["delta"];
                    match delta["type"].as_str() {
                        Some("text_delta") => {
                            if let Some(block) = current_block.as_mut() {
                                if block["type"] == "text" {
                                    let text = delta["text"].as_str().unwrap_or("");
                                    if !text.is_empty() {
                                        if let Some(ref id) = current_text_id {
                                            emit(&event_tx, StreamEvent::TextDelta {
                                                id: id.clone(),
                                                text: text.to_string(),
                                            });
                                        }
                                    }
                                    block["text"] = serde_json::json!(block["text"].as_str().unwrap_or("").to_owned() + text);
                                }
                            }
                        }
                        Some("thinking_delta") => {
                            if let Some(block) = current_block.as_mut() {
                                if block["type"] == "thinking" {
                                    let t = delta["thinking"].as_str().unwrap_or("");
                                    if !t.is_empty() {
                                        if let Some(ref id) = current_reasoning_id {
                                            emit(&event_tx, StreamEvent::ReasoningDelta {
                                                id: id.clone(),
                                                text: t.to_string(),
                                            });
                                        }
                                    }
                                    block["thinking"] = serde_json::json!(block["thinking"].as_str().unwrap_or("").to_owned() + t);
                                }
                            }
                        }
                        Some("signature_delta") => {
                            if let Some(block) = current_block.as_mut() {
                                if block["type"] == "thinking" {
                                    block["signature"] = serde_json::json!(delta["signature"].as_str().unwrap_or(""));
                                }
                            }
                        }
                        Some("input_json_delta") => {
                            let piece = delta["partial_json"].as_str().unwrap_or("");
                            if !piece.is_empty() {
                                tool_json_buf.push_str(piece);
                                if let Some(ref tc_id) = current_tool_call_id {
                                    emit(&event_tx, StreamEvent::ToolInputDelta {
                                        tool_call_id: tc_id.clone(),
                                        delta: piece.to_string(),
                                    });
                                }
                            }
                        }
                        _ => {}
                    }
                }
                "content_block_stop" => {
                    if let Some(mut block) = current_block.take() {
                        if block["type"] == "tool_use" {
                            let input: serde_json::Value = if tool_json_buf.is_empty() {
                                serde_json::Value::Object(Default::default())
                            } else {
                                serde_json::from_str(&tool_json_buf).unwrap_or(serde_json::Value::Object(Default::default()))
                            };
                            block["input"] = input.clone();
                            tool_json_buf.clear();

                            if let (Some(tc_id), Some(name)) = (current_tool_call_id.take(), current_tool_name.take()) {
                                let args_str = serde_json::to_string(&input).unwrap_or_default();
                                emit(&event_tx, StreamEvent::ToolInputAvailable {
                                    tool_call_id: tc_id,
                                    name,
                                    args: args_str,
                                });
                            }
                        } else if block["type"] == "text" {
                            if let Some(id) = current_text_id.take() {
                                emit(&event_tx, StreamEvent::TextEnd { id });
                            }
                        } else if block["type"] == "thinking" {
                            if let Some(id) = current_reasoning_id.take() {
                                emit(&event_tx, StreamEvent::ReasoningEnd { id });
                            }
                        }
                        content_blocks.push(ContentBlock::from_json(block));
                    }
                }
                "message_delta" => {
                    _stop_reason = evt["delta"]["stop_reason"].as_str().map(|s| s.to_string());
                    if let Some(u) = evt["usage"].as_object() {
                        let input = u.get("input_tokens").and_then(|v| v.as_u64()).unwrap_or(0);
                        let output = u.get("output_tokens").and_then(|v| v.as_u64()).unwrap_or(0);
                        let current = usage.unwrap_or_default();
                        usage = Some(TokenUsage {
                            input_tokens: if input > 0 { input } else { current.input_tokens },
                            output_tokens: if output > 0 { output } else { current.output_tokens },
                            total_tokens: Some(
                                (if input > 0 { input } else { current.input_tokens })
                                + (if output > 0 { output } else { current.output_tokens })
                            ),
                        });
                    }
                }
                "message_stop" => {
                    got_message_stop = true;
                }
                "ping" => {}
                _ => {
                    if warn.is_none() {
                        warn = Some(format!("Unknown event type: {evt_type}"));
                    }
                }
            }
        }
    }

    // Defensive: close any block that was open at end-of-stream without a
    // matching stop event.
    if let Some(id) = current_text_id.take() {
        emit(&event_tx, StreamEvent::TextEnd { id });
    }
    if let Some(id) = current_reasoning_id.take() {
        emit(&event_tx, StreamEvent::ReasoningEnd { id });
    }
    if let (Some(tc_id), Some(name)) = (current_tool_call_id.take(), current_tool_name.take()) {
        emit(&event_tx, StreamEvent::ToolInputAvailable {
            tool_call_id: tc_id,
            name,
            args: tool_json_buf.clone(),
        });
    }

    if got_message_stop {
        if let Some(msg) = warn {
            tracing::warn!("[SSE] {msg}");
        }
    }

    Ok((content_blocks, usage))
}

// ── OpenAI parser ─────────────────────────────────────────────────────

/// Per-tool-call state in the OpenAI chat_completions stream. The
/// parser must accumulate `name` / `id` / `args` from successive
/// deltas and only emit `ToolInputAvailable` at the end of stream.
#[derive(Default, Debug)]
struct OpenAIToolCall {
    id: String,
    name: String,
    args: String,
    started: bool,
    available_emitted: bool,
}

/// Parse OpenAI SSE stream (chat completions or responses API).
/// `format` is "responses" or "chat_completions".
/// Emits typed start-delta-end protocol events directly to `event_tx`.
/// When `speculative_tx` is provided, `ToolCallReady` events are sent
/// as soon as a tool call's JSON arguments are fully accumulated,
/// enabling speculative pre-execution in the agent loop.
/// Returns `(content_blocks, usage)`. `usage` is `Some` when the
/// server emitted the optional `stream_options.include_usage` final
/// chunk (OpenAI) or a `usage` field on a regular chunk (responses
/// API). `None` for backends that don't report usage.
pub async fn parse_openai_sse(
    resp: reqwest::Response,
    _format: &str,
    event_tx: Option<UnboundedSender<StreamEvent>>,
    speculative_tx: Option<UnboundedSender<StreamEvent>>,
    apibase: &str,
) -> Result<(Vec<ContentBlock>, Option<TokenUsage>), LlmError> {
    use futures::StreamExt;

    let mut content_blocks: Vec<ContentBlock> = Vec::new();
    let mut buffer = String::new();
    let mut stream = resp.bytes_stream();
    let timeout_secs = chunk_timeout_secs(apibase);
    let mut current_text = String::new();
    let mut current_thinking = String::new();
    // Track whether we are inside a `<thinking>` tag
    // (MiniMax `reasoning_split=false` injects reasoning as `<thinking>...</thinking>` in content).
    let mut in_thinking_tag = false;

    // Per-index open block IDs (text / reasoning).
    let mut current_text_id: Option<String> = None;
    let mut current_reasoning_id: Option<String> = None;
    let mut usage: Option<TokenUsage> = None;

    // Tool call accumulation (chat_completions format sends tool_calls in the delta)
    // Keyed by tool_call index
    let mut tool_calls: std::collections::HashMap<u32, OpenAIToolCall> = std::collections::HashMap::new();
    // Track which tool calls have already been dispatched via speculative_tx
    let mut tool_call_dispatched: std::collections::HashSet<u32> = std::collections::HashSet::new();

    loop {
        let chunk = tokio::time::timeout(
            std::time::Duration::from_secs(timeout_secs),
            stream.next(),
        )
        .await
        .map_err(|_| LlmError::StreamError(
            format!("OpenAI SSE stream timed out (no data for {}s)", timeout_secs)
        ))?;
        let chunk = match chunk {
            Some(c) => c.map_err(|e| LlmError::StreamError(e.to_string()))?,
            None => break,
        };
        buffer.push_str(&String::from_utf8_lossy(&chunk));

        while let Some(newline) = buffer.find('\n') {
            let line = buffer[..newline].trim().to_string();
            buffer = buffer[newline + 1..].to_string();
            if line.is_empty() || !line.starts_with("data:") { continue; }

            let data_str = line[5..].trim().to_string();
            if data_str == "[DONE]" { 
                // Flush open blocks and return cleanly
                if let Some(id) = current_text_id.take() {
                    emit(&event_tx, StreamEvent::TextEnd { id });
                }
                if let Some(id) = current_reasoning_id.take() {
                    emit(&event_tx, StreamEvent::ReasoningEnd { id });
                }
                if !current_text.is_empty() {
                    content_blocks.push(ContentBlock::text(std::mem::take(&mut current_text)));
                }
                if !current_thinking.is_empty() {
                    content_blocks.push(ContentBlock::Thinking { thinking: std::mem::take(&mut current_thinking), signature: None });
                }
                // Emit ToolInputAvailable for any unclosed tool calls
                for idx in 0..tool_calls.len() as u32 {
                    if let Some(t) = tool_calls.get(&idx) {
                        if t.available_emitted { continue; }
                        let parsed_args: serde_json::Value = serde_json::from_str(&t.args).unwrap_or_else(|_| serde_json::Value::String(t.args.clone()));
                        emit(&event_tx, StreamEvent::ToolInputAvailable {
                            tool_call_id: t.id.clone(),
                            name: t.name.clone(),
                            args: serde_json::to_string(&parsed_args).unwrap_or_else(|_| t.args.clone()),
                        });
                        content_blocks.push(ContentBlock::ToolUse {
                            id: t.id.clone(),
                            name: t.name.clone(),
                            input: parsed_args,
                        });
                    }
                }
                return Ok((content_blocks, usage));
            }

            let evt: serde_json::Value = match serde_json::from_str(&data_str) {
                Ok(v) => v,
                Err(_) => continue,
            };

            // chat_completions format
            if let Some(choices) = evt["choices"].as_array() {
                for choice in choices {
                    if let Some(delta) = choice["delta"].as_object() {
                        // Some backends leak reasoning/internal markers
                        // (`<thinking>`, `<antThinking>`, `<summary>`,
                        // `<tool_code>`, `<respond>`) inside the visible
                        // content stream. Dispatch them to the reasoning
                        // channel so they never render to the user.
                        if let Some(text) = delta.get("content").and_then(|v| v.as_str()) {
                            if !text.is_empty() {
                                let mut remaining = text;
                                while !remaining.is_empty() {
                                    let next_tag = find_next_internal_tag(remaining);
                                    match (in_thinking_tag, next_tag) {
                                        (false, Some((tag, start, _tag_name))) => {
                                            let before = &remaining[..start];
                                            if !before.is_empty() {
                                                current_text.push_str(before);
                                                if let Some(ref id) = current_text_id {
                                                    emit(&event_tx, StreamEvent::TextDelta {
                                                        id: id.clone(),
                                                        text: before.to_string(),
                                                    });
                                                } else {
                                                    let id = next_id("t");
                                                    current_text_id = Some(id.clone());
                                                    emit(&event_tx, StreamEvent::TextStart { id: id.clone() });
                                                    emit(&event_tx, StreamEvent::TextDelta {
                                                        id,
                                                        text: before.to_string(),
                                                    });
                                                }
                                            }
                                            if let Some(end) = tag {
                                                // Self-closing tag — content already
                                                // captured; skip past it.
                                                in_thinking_tag = false;
                                                remaining = &remaining[end..];
                                            } else {
                                                if let Some(id) = current_text_id.take() {
                                                    emit(&event_tx, StreamEvent::TextEnd { id });
                                                }
                                                in_thinking_tag = true;
                                                if current_reasoning_id.is_none() {
                                                    let id = next_id("r");
                                                    current_reasoning_id = Some(id.clone());
                                                    emit(&event_tx, StreamEvent::ReasoningStart { id });
                                                }
                                                let open_len = find_open_tag_len(&remaining[start..]);
                                                remaining = &remaining[start + open_len..];
                                            }
                                        }
                                        (true, Some((_, _, tag_name))) => {
                                            // Try the close tag for the tag the model just
                                            // opened; if the model instead wrote a stray
                                            // close of some other internal tag (the common
                                            // failure mode is `</summary>` mid-`<thinking>`),
                                            // fall back to the first internal close we see so
                                            // the actual reply still reaches the user.
                                            let close = find_close_tag(remaining, &tag_name)
                                                .or_else(|| {
                                                    find_any_close_tag(remaining).map(|(p, _)| p)
                                                });
                                            if let Some(end) = close {
                                                let think = &remaining[..end];
                                                if !think.is_empty() {
                                                    current_thinking.push_str(think);
                                                    if let Some(ref id) = current_reasoning_id {
                                                        emit(&event_tx, StreamEvent::ReasoningDelta {
                                                            id: id.clone(),
                                                            text: think.to_string(),
                                                        });
                                                    }
                                                }
                                                in_thinking_tag = false;
                                                remaining = &remaining[end..];
                                            } else {
                                                current_thinking.push_str(remaining);
                                                if let Some(ref id) = current_reasoning_id {
                                                    emit(&event_tx, StreamEvent::ReasoningDelta {
                                                        id: id.clone(),
                                                        text: remaining.to_string(),
                                                    });
                                                }
                                                break;
                                            }
                                        }
                                        (false, None) => {
                                            current_text.push_str(remaining);
                                            if current_text_id.is_none() {
                                                let id = next_id("t");
                                                current_text_id = Some(id.clone());
                                                emit(&event_tx, StreamEvent::TextStart { id: id.clone() });
                                            }
                                            if let Some(ref id) = current_text_id {
                                                emit(&event_tx, StreamEvent::TextDelta {
                                                    id: id.clone(),
                                                    text: remaining.to_string(),
                                                });
                                            }
                                            break;
                                        }
                                        (true, None) => {
                                            // No opening tag found in the buffer. Check
                                            // for a stray close tag — model may have just
                                            // finished thinking and started the reply
                                            // without opening a new wrapper. Split on the
                                            // first such close so the reply reaches the
                                            // text channel.
                                            if let Some((end, _)) = find_any_close_tag(remaining) {
                                                let think = &remaining[..end];
                                                if !think.is_empty() {
                                                    current_thinking.push_str(think);
                                                    if let Some(ref id) = current_reasoning_id {
                                                        emit(&event_tx, StreamEvent::ReasoningDelta {
                                                            id: id.clone(),
                                                            text: think.to_string(),
                                                        });
                                                    }
                                                }
                                                in_thinking_tag = false;
                                                remaining = &remaining[end..];
                                                continue;
                                            }
                                            current_thinking.push_str(remaining);
                                            if let Some(ref id) = current_reasoning_id {
                                                emit(&event_tx, StreamEvent::ReasoningDelta {
                                                    id: id.clone(),
                                                    text: remaining.to_string(),
                                                });
                                            }
                                            break;
                                        }
                                    }
                                }
                            }
                        }

                        if let Some(reasoning) = delta.get("reasoning_content").and_then(|v| v.as_str()) {
                            if !reasoning.is_empty() {
                                current_thinking.push_str(reasoning);
                                if current_reasoning_id.is_none() {
                                    let id = next_id("r");
                                    current_reasoning_id = Some(id.clone());
                                    emit(&event_tx, StreamEvent::ReasoningStart { id: id.clone() });
                                }
                                if let Some(ref id) = current_reasoning_id {
                                    emit(&event_tx, StreamEvent::ReasoningDelta {
                                        id: id.clone(),
                                        text: reasoning.to_string(),
                                    });
                                }
                            }
                        }

                        if let Some(tcs) = delta.get("tool_calls").and_then(|v| v.as_array()) {
                            for tc in tcs {
                                let idx = tc.get("index").and_then(|v| v.as_u64()).unwrap_or(0) as u32;

                                if let Some(tc_id) = tc.get("id").and_then(|v| v.as_str()) {
                                    if !tc_id.is_empty() {
                                        tool_calls.entry(idx).or_insert_with(|| OpenAIToolCall {
                                            id: tc_id.to_string(),
                                            ..Default::default()
                                        }).id = tc_id.to_string();
                                    }
                                }

                                if let Some(func) = tc.get("function") {
                                    if let Some(name) = func.get("name").and_then(|v| v.as_str()) {
                                        if !name.is_empty() {
                                            let entry = tool_calls.entry(idx).or_default();
                                            if !entry.started {
                                                let tc_id = if !entry.id.is_empty() {
                                                    entry.id.clone()
                                                } else {
                                                    let minted = next_id("tc");
                                                    entry.id = minted.clone();
                                                    minted
                                                };
                                                let name_owned = name.to_string();
                                                entry.name = name_owned.clone();
                                                entry.started = true;
                                                emit(&event_tx, StreamEvent::ToolInputStart {
                                                    tool_call_id: tc_id,
                                                    name: name_owned,
                                                });
                                            }
                                        }
                                    }
                                    if let Some(args) = func.get("arguments").and_then(|v| v.as_str()) {
                                        if !args.is_empty() {
                                            let entry = tool_calls.entry(idx).or_default();
                                            entry.args.push_str(args);
                                            let tc_id = if entry.id.is_empty() {
                                                let minted = next_id("tc");
                                                entry.id = minted.clone();
                                                minted
                                            } else {
                                                entry.id.clone()
                                            };
                                            if !entry.started {
                                                entry.started = true;
                                                emit(&event_tx, StreamEvent::ToolInputStart {
                                                    tool_call_id: tc_id.clone(),
                                                    name: String::new(),
                                                });
                                            }
                                            emit(&event_tx, StreamEvent::ToolInputDelta {
                                                tool_call_id: tc_id,
                                                delta: args.to_string(),
                                            });
                                        }
                                    }

                                    if let Some(ref spec_tx) = speculative_tx {
                                        if !tool_call_dispatched.contains(&idx) {
                                            let tc_name = tool_calls.get(&idx).map(|t| t.name.as_str()).unwrap_or("");
                                            if !tc_name.is_empty() {
                                                let tc_args = tool_calls.get(&idx).map(|t| t.args.as_str()).unwrap_or("");
                                                if serde_json::from_str::<serde_json::Value>(tc_args).is_ok() {
                                                    let tc_id = tool_calls.get(&idx).map(|t| t.id.as_str()).unwrap_or("");
                                                    let _ = spec_tx.send(StreamEvent::ToolCallReady {
                                                        id: tc_id.to_string(),
                                                        name: tc_name.to_string(),
                                                        args: tc_args.to_string(),
                                                    });
                                                    tool_call_dispatched.insert(idx);
                                                }
                                            }
                                        }
                                    }
                                }
                            }
                        }
                    }
                }
            }

            if let Some(u) = evt.get("usage").and_then(|v| v.as_object()) {
                tracing::warn!("[oz-llm] usage raw (openai): {}", serde_json::to_string(u).unwrap_or_default());
                let prompt = u.get("prompt_tokens").and_then(|v| v.as_u64()).unwrap_or(0);
                let completion = u.get("completion_tokens").and_then(|v| v.as_u64()).unwrap_or(0);
                let total = u.get("total_tokens").and_then(|v| v.as_u64());
                if prompt > 0 || completion > 0 || total.is_some() {
                    usage = Some(TokenUsage {
                        input_tokens: prompt,
                        output_tokens: completion,
                        total_tokens: total,
                    });
                }
            } else if let Some(resp_usage) = evt.get("response").and_then(|v| v.get("usage")).and_then(|v| v.as_object()) {
                let prompt = resp_usage.get("input_tokens").and_then(|v| v.as_u64()).unwrap_or(0);
                let completion = resp_usage.get("output_tokens").and_then(|v| v.as_u64()).unwrap_or(0);
                let total = resp_usage.get("total_tokens").and_then(|v| v.as_u64());
                if prompt > 0 || completion > 0 || total.is_some() {
                    usage = Some(TokenUsage {
                        input_tokens: prompt,
                        output_tokens: completion,
                        total_tokens: total,
                    });
                }
            }
        }
    }

    // Close any open text / reasoning blocks at end-of-stream.
    if let Some(id) = current_text_id.take() {
        emit(&event_tx, StreamEvent::TextEnd { id });
    }
    if let Some(id) = current_reasoning_id.take() {
        emit(&event_tx, StreamEvent::ReasoningEnd { id });
    }

    if !current_text.is_empty() {
        content_blocks.push(ContentBlock::text(current_text));
    }
    if !current_thinking.is_empty() {
        content_blocks.push(ContentBlock::Thinking { thinking: current_thinking, signature: None });
    }

    // Build ContentBlock::ToolUse from accumulated tool call data and emit
    // ToolInputAvailable for any that weren't closed mid-stream.
    for idx in 0..tool_calls.len() as u32 {
        if let Some(t) = tool_calls.get(&idx) {
            if t.available_emitted {
                continue;
            }
            let raw_args = t.args.clone();
            let tc_id = t.id.clone();
            let parsed_args: serde_json::Value = serde_json::from_str(&raw_args).unwrap_or_else(|_| {
                serde_json::Value::String(raw_args.clone())
            });
            emit(&event_tx, StreamEvent::ToolInputAvailable {
                tool_call_id: tc_id,
                name: t.name.clone(),
                args: serde_json::to_string(&parsed_args).unwrap_or_else(|_| raw_args.clone()),
            });
            content_blocks.push(ContentBlock::ToolUse {
                id: t.id.clone(),
                name: t.name.clone(),
                input: parsed_args,
            });
        }
    }

    Ok((content_blocks, usage))
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::sync::mpsc;

    #[tokio::test]
    async fn id_generator_is_monotonic() {
        let a = next_id("t");
        let b = next_id("t");
        assert!(a != b);
        assert!(a.starts_with('t'));
        assert!(b.starts_with('t'));
    }

    #[tokio::test]
    async fn emit_no_tx_is_noop() {
        emit(&None, StreamEvent::TextEnd { id: "t1".into() });
    }

    #[tokio::test]
    async fn closed_channel_does_not_panic() {
        let (tx, rx) = mpsc::unbounded_channel::<StreamEvent>();
        drop(rx);
        emit(&Some(tx), StreamEvent::TextStart { id: "t1".into() });
    }

    #[test]
    fn tool_call_default_is_unstarted() {
        let t = OpenAIToolCall::default();
        assert!(!t.started);
        assert!(!t.available_emitted);
        assert_eq!(t.id, "");
        assert_eq!(t.name, "");
        assert_eq!(t.args, "");
    }
}
