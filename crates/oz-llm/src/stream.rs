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
    "think",
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
                            best = Some((
                                start,
                                close_abs + 1 - start,
                                tag.to_string(),
                                is_self_closing,
                            ));
                        }
                        break;
                    }
                }
            }
            from = abs + 1;
        }
    }
    best.map(|(start, open_len, name, self_closing)| {
        let end_after = if self_closing {
            Some(start + open_len)
        } else {
            None
        };
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
    if crate::is_local_apibase(apibase) {
        300
    } else {
        60
    }
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
        let chunk =
            tokio::time::timeout(std::time::Duration::from_secs(timeout_secs), stream.next())
                .await
                .map_err(|_| {
                    LlmError::StreamError(format!(
                        "Claude SSE stream timed out (no data for {}s)",
                        timeout_secs
                    ))
                })?;
        let chunk = match chunk {
            Some(c) => c.map_err(|e| LlmError::StreamError(e.to_string()))?,
            None => break,
        };
        buffer.push_str(&String::from_utf8_lossy(&chunk));

        // Scan by offset and drain once per chunk: rebuilding the rest of
        // the buffer for every line made this loop O(buffer^2) on large
        // chunks.
        let mut consumed = 0usize;
        while let Some(rel) = buffer[consumed..].find('\n') {
            let line = buffer[consumed..consumed + rel].trim().to_string();
            consumed += rel + 1;
            if line.is_empty() {
                continue;
            }
            if !line.starts_with("data:") {
                continue;
            }

            let data_str = line[5..].trim().to_string();
            if data_str == "[DONE]" {
                // Flush any open blocks before returning (same as end-of-stream cleanup below).
                if let Some(id) = current_text_id.take() {
                    emit(&event_tx, StreamEvent::TextEnd { id });
                }
                if let Some(id) = current_reasoning_id.take() {
                    emit(&event_tx, StreamEvent::ReasoningEnd { id });
                }
                if let (Some(tc_id), Some(name)) =
                    (current_tool_call_id.take(), current_tool_name.take())
                {
                    emit(
                        &event_tx,
                        StreamEvent::ToolInputAvailable {
                            tool_call_id: tc_id,
                            name,
                            args: std::mem::take(&mut tool_json_buf),
                        },
                    );
                }
                return Ok((content_blocks, usage));
            }

            let evt: serde_json::Value = match serde_json::from_str(&data_str) {
                Ok(v) => v,
                Err(e) => {
                    tracing::warn!(
                        "[SSE] JSON parse error: {e}, line: {}",
                        &data_str[..data_str.len().min(200)]
                    );
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
                                emit(
                                    &event_tx,
                                    StreamEvent::ToolInputStart {
                                        tool_call_id: tc_id,
                                        name,
                                    },
                                );
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
                                            emit(
                                                &event_tx,
                                                StreamEvent::TextDelta {
                                                    id: id.clone(),
                                                    text: text.to_string(),
                                                },
                                            );
                                        }
                                    }
                                    block["text"] = serde_json::json!(
                                        block["text"].as_str().unwrap_or("").to_owned() + text
                                    );
                                }
                            }
                        }
                        Some("thinking_delta") => {
                            if let Some(block) = current_block.as_mut() {
                                if block["type"] == "thinking" {
                                    let t = delta["thinking"].as_str().unwrap_or("");
                                    if !t.is_empty() {
                                        if let Some(ref id) = current_reasoning_id {
                                            emit(
                                                &event_tx,
                                                StreamEvent::ReasoningDelta {
                                                    id: id.clone(),
                                                    text: t.to_string(),
                                                },
                                            );
                                        }
                                    }
                                    block["thinking"] = serde_json::json!(
                                        block["thinking"].as_str().unwrap_or("").to_owned() + t
                                    );
                                }
                            }
                        }
                        Some("signature_delta") => {
                            if let Some(block) = current_block.as_mut() {
                                if block["type"] == "thinking" {
                                    block["signature"] = serde_json::json!(delta["signature"]
                                        .as_str()
                                        .unwrap_or(""));
                                }
                            }
                        }
                        Some("input_json_delta") => {
                            let piece = delta["partial_json"].as_str().unwrap_or("");
                            if !piece.is_empty() {
                                tool_json_buf.push_str(piece);
                                if let Some(ref tc_id) = current_tool_call_id {
                                    emit(
                                        &event_tx,
                                        StreamEvent::ToolInputDelta {
                                            tool_call_id: tc_id.clone(),
                                            delta: piece.to_string(),
                                        },
                                    );
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
                                serde_json::from_str(&tool_json_buf)
                                    .unwrap_or(serde_json::Value::Object(Default::default()))
                            };
                            block["input"] = input.clone();
                            tool_json_buf.clear();

                            if let (Some(tc_id), Some(name)) =
                                (current_tool_call_id.take(), current_tool_name.take())
                            {
                                let args_str = serde_json::to_string(&input).unwrap_or_default();
                                emit(
                                    &event_tx,
                                    StreamEvent::ToolInputAvailable {
                                        tool_call_id: tc_id,
                                        name,
                                        args: args_str,
                                    },
                                );
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
                            input_tokens: if input > 0 {
                                input
                            } else {
                                current.input_tokens
                            },
                            output_tokens: if output > 0 {
                                output
                            } else {
                                current.output_tokens
                            },
                            total_tokens: Some(
                                (if input > 0 {
                                    input
                                } else {
                                    current.input_tokens
                                }) + (if output > 0 {
                                    output
                                } else {
                                    current.output_tokens
                                }),
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
        buffer.drain(..consumed);
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
        emit(
            &event_tx,
            StreamEvent::ToolInputAvailable {
                tool_call_id: tc_id,
                name,
                args: tool_json_buf.clone(),
            },
        );
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
    let mut tool_calls: std::collections::HashMap<u32, OpenAIToolCall> =
        std::collections::HashMap::new();
    // Track which tool calls have already been dispatched via speculative_tx
    let mut tool_call_dispatched: std::collections::HashSet<u32> = std::collections::HashSet::new();

    loop {
        let chunk =
            tokio::time::timeout(std::time::Duration::from_secs(timeout_secs), stream.next())
                .await
                .map_err(|_| {
                    LlmError::StreamError(format!(
                        "OpenAI SSE stream timed out (no data for {}s)",
                        timeout_secs
                    ))
                })?;
        let chunk = match chunk {
            Some(c) => c.map_err(|e| LlmError::StreamError(e.to_string()))?,
            None => break,
        };
        buffer.push_str(&String::from_utf8_lossy(&chunk));

        // Same offset-scan as the Claude path: O(chunk) instead of
        // O(buffer^2).
        let mut consumed = 0usize;
        while let Some(rel) = buffer[consumed..].find('\n') {
            let line = buffer[consumed..consumed + rel].trim().to_string();
            consumed += rel + 1;
            if line.is_empty() || !line.starts_with("data:") {
                continue;
            }

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
                    content_blocks.push(ContentBlock::Thinking {
                        thinking: std::mem::take(&mut current_thinking),
                        signature: None,
                    });
                }
                // Emit ToolInputAvailable for any unclosed tool calls
                for idx in 0..tool_calls.len() as u32 {
                    if let Some(t) = tool_calls.get(&idx) {
                        if t.available_emitted {
                            continue;
                        }
                        let parsed_args: serde_json::Value = normalize_tool_args_for(
                            &t.name,
                            serde_json::from_str(&t.args)
                                .unwrap_or_else(|_| serde_json::Value::String(t.args.clone())),
                        );
                        emit(
                            &event_tx,
                            StreamEvent::ToolInputAvailable {
                                tool_call_id: t.id.clone(),
                                name: t.name.clone(),
                                args: serde_json::to_string(&parsed_args)
                                    .unwrap_or_else(|_| t.args.clone()),
                            },
                        );
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
                                                    emit(
                                                        &event_tx,
                                                        StreamEvent::TextDelta {
                                                            id: id.clone(),
                                                            text: before.to_string(),
                                                        },
                                                    );
                                                } else {
                                                    let id = next_id("t");
                                                    current_text_id = Some(id.clone());
                                                    emit(
                                                        &event_tx,
                                                        StreamEvent::TextStart { id: id.clone() },
                                                    );
                                                    emit(
                                                        &event_tx,
                                                        StreamEvent::TextDelta {
                                                            id,
                                                            text: before.to_string(),
                                                        },
                                                    );
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
                                                    emit(
                                                        &event_tx,
                                                        StreamEvent::ReasoningStart { id },
                                                    );
                                                }
                                                let open_len =
                                                    find_open_tag_len(&remaining[start..]);
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
                                                        emit(
                                                            &event_tx,
                                                            StreamEvent::ReasoningDelta {
                                                                id: id.clone(),
                                                                text: think.to_string(),
                                                            },
                                                        );
                                                    }
                                                }
                                                in_thinking_tag = false;
                                                remaining = &remaining[end..];
                                            } else {
                                                current_thinking.push_str(remaining);
                                                if let Some(ref id) = current_reasoning_id {
                                                    emit(
                                                        &event_tx,
                                                        StreamEvent::ReasoningDelta {
                                                            id: id.clone(),
                                                            text: remaining.to_string(),
                                                        },
                                                    );
                                                }
                                                break;
                                            }
                                        }
                                        (false, None) => {
                                            current_text.push_str(remaining);
                                            if current_text_id.is_none() {
                                                let id = next_id("t");
                                                current_text_id = Some(id.clone());
                                                emit(
                                                    &event_tx,
                                                    StreamEvent::TextStart { id: id.clone() },
                                                );
                                            }
                                            if let Some(ref id) = current_text_id {
                                                emit(
                                                    &event_tx,
                                                    StreamEvent::TextDelta {
                                                        id: id.clone(),
                                                        text: remaining.to_string(),
                                                    },
                                                );
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
                                                        emit(
                                                            &event_tx,
                                                            StreamEvent::ReasoningDelta {
                                                                id: id.clone(),
                                                                text: think.to_string(),
                                                            },
                                                        );
                                                    }
                                                }
                                                in_thinking_tag = false;
                                                remaining = &remaining[end..];
                                                continue;
                                            }
                                            current_thinking.push_str(remaining);
                                            if let Some(ref id) = current_reasoning_id {
                                                emit(
                                                    &event_tx,
                                                    StreamEvent::ReasoningDelta {
                                                        id: id.clone(),
                                                        text: remaining.to_string(),
                                                    },
                                                );
                                            }
                                            break;
                                        }
                                    }
                                }
                            }
                        }

                        if let Some(reasoning) =
                            delta.get("reasoning_content").and_then(|v| v.as_str())
                        {
                            if !reasoning.is_empty() {
                                current_thinking.push_str(reasoning);
                                if current_reasoning_id.is_none() {
                                    let id = next_id("r");
                                    current_reasoning_id = Some(id.clone());
                                    emit(&event_tx, StreamEvent::ReasoningStart { id: id.clone() });
                                }
                                if let Some(ref id) = current_reasoning_id {
                                    emit(
                                        &event_tx,
                                        StreamEvent::ReasoningDelta {
                                            id: id.clone(),
                                            text: reasoning.to_string(),
                                        },
                                    );
                                }
                            }
                        }

                        if let Some(tcs) = delta.get("tool_calls").and_then(|v| v.as_array()) {
                            for tc in tcs {
                                let idx =
                                    tc.get("index").and_then(|v| v.as_u64()).unwrap_or(0) as u32;

                                if let Some(tc_id) = tc.get("id").and_then(|v| v.as_str()) {
                                    if !tc_id.is_empty() {
                                        tool_calls
                                            .entry(idx)
                                            .or_insert_with(|| OpenAIToolCall {
                                                id: tc_id.to_string(),
                                                ..Default::default()
                                            })
                                            .id = tc_id.to_string();
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
                                                emit(
                                                    &event_tx,
                                                    StreamEvent::ToolInputStart {
                                                        tool_call_id: tc_id,
                                                        name: name_owned,
                                                    },
                                                );
                                            }
                                        }
                                    }
                                    if let Some(args) =
                                        func.get("arguments").and_then(|v| v.as_str())
                                    {
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
                                                emit(
                                                    &event_tx,
                                                    StreamEvent::ToolInputStart {
                                                        tool_call_id: tc_id.clone(),
                                                        name: String::new(),
                                                    },
                                                );
                                            }
                                            emit(
                                                &event_tx,
                                                StreamEvent::ToolInputDelta {
                                                    tool_call_id: tc_id,
                                                    delta: args.to_string(),
                                                },
                                            );
                                        }
                                    }

                                    if let Some(ref spec_tx) = speculative_tx {
                                        if !tool_call_dispatched.contains(&idx) {
                                            let tc_name = tool_calls
                                                .get(&idx)
                                                .map(|t| t.name.as_str())
                                                .unwrap_or("");
                                            if !tc_name.is_empty() {
                                                let tc_args = tool_calls
                                                    .get(&idx)
                                                    .map(|t| t.args.as_str())
                                                    .unwrap_or("");
                                                if serde_json::from_str::<serde_json::Value>(
                                                    tc_args,
                                                )
                                                .is_ok()
                                                {
                                                    let tc_id = tool_calls
                                                        .get(&idx)
                                                        .map(|t| t.id.as_str())
                                                        .unwrap_or("");
                                                    // Normalize degenerate shapes here
                                                    // so the speculative executor AND
                                                    // the ToolInputAvailable event the
                                                    // UI/history see carry the real
                                                    // payload, not a content-wrapped
                                                    // imitation.
                                                    let normalized = normalize_tool_args_for(
                                                        tc_name,
                                                        serde_json::from_str::<serde_json::Value>(
                                                            tc_args,
                                                        )
                                                        .unwrap_or_default(),
                                                    );
                                                    let _ =
                                                        spec_tx.send(StreamEvent::ToolCallReady {
                                                            id: tc_id.to_string(),
                                                            name: tc_name.to_string(),
                                                            args: normalized.to_string(),
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
                tracing::warn!(
                    "[oz-llm] usage raw (openai): {}",
                    serde_json::to_string(u).unwrap_or_default()
                );
                let prompt = u.get("prompt_tokens").and_then(|v| v.as_u64()).unwrap_or(0);
                let completion = u
                    .get("completion_tokens")
                    .and_then(|v| v.as_u64())
                    .unwrap_or(0);
                let total = u.get("total_tokens").and_then(|v| v.as_u64());
                if prompt > 0 || completion > 0 || total.is_some() {
                    usage = Some(TokenUsage {
                        input_tokens: prompt,
                        output_tokens: completion,
                        total_tokens: total,
                    });
                }
            } else if let Some(resp_usage) = evt
                .get("response")
                .and_then(|v| v.get("usage"))
                .and_then(|v| v.as_object())
            {
                let prompt = resp_usage
                    .get("input_tokens")
                    .and_then(|v| v.as_u64())
                    .unwrap_or(0);
                let completion = resp_usage
                    .get("output_tokens")
                    .and_then(|v| v.as_u64())
                    .unwrap_or(0);
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
        buffer.drain(..consumed);
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
        content_blocks.push(ContentBlock::Thinking {
            thinking: current_thinking,
            signature: None,
        });
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
            let parsed_args: serde_json::Value = normalize_tool_args_for(
                &t.name,
                serde_json::from_str(&raw_args)
                    .unwrap_or_else(|_| serde_json::Value::String(raw_args.clone())),
            );
            emit(
                &event_tx,
                StreamEvent::ToolInputAvailable {
                    tool_call_id: tc_id,
                    name: t.name.clone(),
                    args: serde_json::to_string(&parsed_args).unwrap_or_else(|_| raw_args.clone()),
                },
            );
            content_blocks.push(ContentBlock::ToolUse {
                id: t.id.clone(),
                name: t.name.clone(),
                input: parsed_args,
            });
        }
    }

    Ok((content_blocks, usage))
}

// ── Leaked parameter-call repair ─────────────────────────────────────
//
// Dialects a tool-call-extracting gateway may leak as the accumulated
// `arguments` string (see `repair_parameter_style_args` below).
const XML_OPEN: &str = "<parameter name=\"";
const XML_OPEN_ESCAPED: &str = "<parameter name=\\\"";
const HALF_OPEN: &str = "{\"";
const PARAM_CLOSER: &str = "</parameter>";
/// DeepSeek's full-width DSML token: `<｜DSML｜  parameter …>` and the
/// nameless `<｜DSML｜: "…"` form.
const DSML_TOKEN: &str = "\u{ff5c}DSML\u{ff5c}";

/// Byte offset of the earliest DSML marker in `s`, in any spelling the
/// gateway has been seen to produce: `<｜DSML｜`, `</｜DSML｜:`, `｜/DSML｜:`
/// (2026-10-23 12:26: `verify_task.py｜/DSML｜:` — the closer form the
/// earlier helpers missed), `/｜DSML｜`, and a bare `DSML｜`.
fn dsml_marker_index(s: &str) -> Option<usize> {
    [
        "</\u{ff5c}DSML",
        "<\u{ff5c}DSML",
        "\u{ff5c}/DSML",
        "/\u{ff5c}DSML",
        DSML_TOKEN,
        "DSML\u{ff5c}",
    ]
    .iter()
    .filter_map(|needle| s.find(needle))
    .min()
}

/// Heuristic cap so garbage input cannot drive an unbounded key scan.
const MAX_PARAM_KEY_LEN: usize = 80;

/// True when `tail` (the part of a value that follows a parameter/DSML
/// boundary) continues with ANOTHER tool call's serialized arguments —
/// `", "type">bash`, `", "code">…`, `", "timeout": 240`, `", "file_path">…`.
///
/// This is the merge signature seen on 2026-10-23 11:31: the gateway folded
/// three following `code_run` calls into the `file_path` value of a `write`
/// call. Cutting at the boundary yields a clean-looking path, but the
/// surviving `content` belongs to a different call, so the call must be
/// rejected rather than executed with the wrong payload.
fn merged_calls_tail(tail: &str, tool: Option<&str>) -> bool {
    // Only curated tools have a canonical parameter set to compare against.
    // For MCP / dynamic tools, recovering what we can is safer than guessing
    // that a foreign-looking field means a merge.
    let Some(own) = tool.and_then(oz_core_types::canonical_param_names) else {
        return false;
    };
    const KEYS: [&str; 6] = [
        "type",
        "code",
        "file_path",
        "content",
        "timeout",
        "question",
    ];
    KEYS.iter().any(|k| {
        // A key the call legitimately owns is a recoverable leak (that is the
        // ask_user / write shapes above); a key from ANOTHER tool's schema is
        // the merge signature (`type`/`timeout` inside a write call).
        if own.contains(k) {
            return false;
        }
        tail.contains(&format!("\", \"{k}\">"))
            || tail.contains(&format!(",\"{k}\">"))
            || tail.contains(&format!("\", \"{k}\":"))
    })
}

/// Earliest leaked-parameter opener in `rest`, of any dialect. Returns
/// `(start, key, value_start)` with offsets relative to `rest`, or `None`.
fn next_parameter_opener(rest: &str) -> Option<(usize, String, usize)> {
    let mut best: Option<(usize, String, usize)> = None;
    for (needle, escaped) in [
        (HALF_OPEN, false),
        (XML_OPEN, false),
        (XML_OPEN_ESCAPED, true),
    ] {
        let mut from = 0usize;
        while let Some(p) = rest[from..].find(needle) {
            let abs = from + p;
            let after_open = abs + needle.len();
            let quote = if escaped { "\\\"" } else { "\"" };
            let Some(key_rel) = rest[after_open..].find(quote) else {
                break;
            };
            let key = rest[after_open..after_open + key_rel].trim().to_string();
            if key.is_empty() || key.len() > MAX_PARAM_KEY_LEN || key.contains('\n') {
                from = abs + 1;
                continue;
            }
            let key_close = after_open + key_rel + quote.len();
            // Half form: the key must be terminated directly by `>` — a
            // proper JSON object has `":`, so `{"key">` is the leak
            // signature rather than a truncated JSON object.
            if !escaped && needle == HALF_OPEN && !rest[key_close..].starts_with('>') {
                from = abs + 1;
                continue;
            }
            // XML forms may carry attributes (`string="true"`) between the
            // key and the tag's `>`; the value starts past that `>`.
            let value_start = if needle == HALF_OPEN {
                key_close + 1
            } else {
                match rest[key_close..].find('>') {
                    Some(g) => key_close + g + 1,
                    None => break,
                }
            };
            if best.as_ref().is_none_or(|(b, _, _)| abs < *b) {
                best = Some((abs, key, value_start));
            }
            break;
        }
    }
    best
}

/// Recover tool arguments from a provider's half-converted parameter-call
/// syntax. Some gateways translate the model's XML-style tool calls
/// (DeepSeek DSML, Claude-Code `<parameter>` dialect) into chat_completions
/// `tool_calls` deltas but leak malformed fragments as the accumulated
/// `arguments` string; strict JSON parsing fails, and the raw string used to
/// become the whole argument object — for the terminal `respond` call that
/// made the final reply unrecoverable, which cascaded into the agent loop
/// promoting the entire thinking transcript as the user-visible reply.
///
/// Recognized shapes (the closing tag is optional — the gateway may truncate
/// the call mid-value):
///   `{"key">value</parameter>`                 — opening tag reduced to `{`
///   `<parameter name="key">value</parameter>`  — intact XML form
///   `<parameter name=\"key\">value</parameter>` — JSON-escaped variant
///
/// Values always become strings; duplicate keys are last-wins. Returns
/// `None` when no recognizable fragment is present so callers keep their
/// existing fallback. Heuristic by design: it only ever sees strings that
/// already failed strict JSON parsing.
pub fn repair_parameter_style_args(raw: &str, tool: Option<&str>) -> Option<serde_json::Value> {
    // The gateway may hand us the real arguments object as a JSON *string*
    // (double-encoded). Unwrap before scanning so escaped quotes are real
    // quotes again; without this the scanners below see `\"key\"` and every
    // repair path misses.
    let mut owned = raw.trim().to_string();
    for _ in 0..2 {
        if !owned.starts_with('"') {
            break;
        }
        match serde_json::from_str::<String>(&owned) {
            Ok(inner) if inner.contains('{') || inner.contains("<parameter") => owned = inner,
            _ => break,
        }
    }
    let s = owned.trim();
    if s.is_empty() {
        return None;
    }
    let salvaged = salvage_leaked_fields(s, tool);
    // Cheap rejection before scanning: one of the dialects must appear.
    let has_marker = s.contains(XML_OPEN)
        || s.contains(XML_OPEN_ESCAPED)
        || s.contains("\">")
        || s.contains("\"\u{ff5c}>")
        || s.contains("\"|>")
        || s.contains("<parameter:");
    if !has_marker && salvaged.is_empty() {
        return None;
    }
    // Tier 1 — JSON reconstruction. The gateway sometimes splices junk
    // between a half-open key and a JSON continuation that is otherwise
    // intact (observed: `{"file_path"><parameter: "/p", "content": "…\n"}`).
    // Rebuild the object from the opener's key plus everything from the
    // first quote of the remainder; accept it only when it parses. This
    // recovers the CONTENT-bearing calls that pair extraction loses.
    if let Some((_, key, value_start)) = next_parameter_opener(s) {
        if let Some(q) = s[value_start..].find('"') {
            // The first quote may belong to a NAMELESS DSML value that
            // follows the key's own `</parameter>` boundary — reconstructing
            // with it would assign the wrong value to the opener key.
            let gap = &s[value_start..value_start + q];
            let boundary_in_gap = gap.contains(PARAM_CLOSER);
            let mut candidate = String::with_capacity(s.len() - value_start - q + key.len() + 8);
            if !boundary_in_gap {
                candidate.push('{');
                candidate.push('"');
                candidate.push_str(&key);
                candidate.push_str("\": ");
                candidate.push_str(&s[value_start + q..]);
                // The tail usually still carries its own closing brace; only add
                // one when the gateway truncated the call before it.
                if serde_json::from_str::<serde_json::Value>(&candidate).is_err() {
                    candidate.push('}');
                }
                if let Ok(mut v) = serde_json::from_str::<serde_json::Value>(&candidate) {
                    if v.is_object() {
                        if let Some(obj) = v.as_object_mut() {
                            for (k, val) in salvaged.clone() {
                                obj.entry(k).or_insert(val);
                            }
                        }
                        return Some(v);
                    }
                }
            }
        }
    }
    // Tier 2 — pair extraction from the leaked parameter fragments.
    let mut out = serde_json::Map::new();
    let mut i = 0usize;
    while let Some((_, key, value_start)) = next_parameter_opener(&s[i..]) {
        let rest = &s[i..];
        // The value runs to the next `</parameter>` or the next opener of any
        // dialect, whichever comes first; otherwise to end-of-string (the
        // gateway truncated the call there).
        let body = &rest[value_start..];
        let mut end = body.len();
        for marker in [PARAM_CLOSER, XML_OPEN, XML_OPEN_ESCAPED, HALF_OPEN] {
            if let Some(p) = body.find(marker) {
                if p < end {
                    end = p;
                }
            }
        }
        let mut value = body[..end].trim().to_string();
        // A truncated tail often ends with dangling closing noise (`: "`),
        // the quote the model would have closed its value with. Only strip
        // on truncated values: a value closed by `</parameter>` may
        // legitimately end with a quote or a colon.
        if end == body.len() {
            let had_dangling_quote = value.ends_with('"');
            while value.ends_with('"') {
                value.pop();
            }
            if had_dangling_quote {
                let trimmed = value.trim_end();
                if let Some(stripped) = trimmed.strip_suffix(':') {
                    value = stripped.trim_end().to_string();
                }
            }
        }
        out.insert(key, serde_json::Value::String(value));
        // All offsets above are `rest`-relative; advance the absolute cursor
        // by the same amount and skip a trailing closer so it cannot become
        // the next value.
        let mut advance = value_start + end;
        if rest[advance..].starts_with(PARAM_CLOSER) {
            advance += PARAM_CLOSER.len();
        }
        i += advance;
    }
    // Merge last-resort field salvage (covers separator corruption that the
    // opener scanner cannot see, e.g. `"question">Go: "…"`).
    for (k, value) in salvaged {
        out.entry(k).or_insert(value);
    }
    // Nameless DSML values (`<｜DSML｜: "…"`): the gateway dropped the
    // parameter name entirely. The value almost always belongs to the
    // tool's primary parameter — map it there when the tool is known and
    // the key is still missing.
    if !out.is_empty() {
        if let Some(v) = extract_nameless_dsml_value(s) {
            // The nameless value belongs to the tool's FIRST MISSING
            // canonical parameter (write with a bare file_path → content).
            if let Some(names) = tool.and_then(oz_core_types::canonical_param_names) {
                if let Some(slot) = names.into_iter().find(|k| !out.contains_key(*k)) {
                    out.insert(slot.to_string(), serde_json::Value::String(v));
                }
            }
        }
    }
    if out.is_empty() {
        return None;
    }
    Some(serde_json::Value::Object(out))
}

/// Decode one JSON string literal at the start of `text`.
fn extract_json_string(text: &str) -> Option<String> {
    let quoted = text.trim_start().strip_prefix('"')?;
    let bytes = quoted.as_bytes();
    let mut i = 0usize;
    while i < bytes.len() {
        match bytes[i] {
            b'\\' => i += 2,
            b'"' => {
                let mut body = String::with_capacity(i + 2);
                body.push('"');
                body.push_str(&quoted[..i]);
                body.push('"');
                return serde_json::from_str::<String>(&body).ok();
            }
            _ => i += 1,
        }
    }
    None
}

/// Extract the value of a NAMELESS fragment — the gateway lost the
/// parameter name, but the quoted JSON-escaped value is intact. Two
/// dialects appear in practice: `<｜DSML｜: "…"` (DeepSeek DSML) and
/// `<parameter: "…"` / `<parameter > "…"` (the converter dropped `name=`).
/// Named forms are handled by the pair scanner; this skips them.
fn extract_nameless_dsml_value(s: &str) -> Option<String> {
    const DSML: &str = "\u{ff5c}DSML\u{ff5c}";
    let mut from = 0usize;
    while let Some(p) = s[from..].find(DSML) {
        let after = &s[from + p + DSML.len()..];
        from += p + DSML.len();
        if after.trim_start().starts_with("parameter") {
            continue;
        }
        let rest = after.trim_start().strip_prefix(':')?.trim_start();
        return extract_json_string(rest);
    }
    // `<parameter:` / `<parameter >` with no `name=` before the value.
    for opener in ["<parameter:", "<parameter :", "<parameter >"] {
        if let Some(p) = s.find(opener) {
            let after = s[p + opener.len()..].trim_start();
            let after = after
                .strip_prefix('>')
                .or_else(|| after.strip_prefix(':'))
                .unwrap_or(after)
                .trim_start();
            if let Some(value) = extract_json_string(after) {
                return Some(value);
            }
        }
    }
    None
}

/// Extract named parameters from DSML fragments embedded in a value or
/// trailing text: `<｜DSML｜  parameter name="k" string="true">v</parameter>`
/// (quotes may arrive JSON-escaped as `\"`). DeepSeek's DSML dialect uses
/// fullwidth brackets, so the literal `<｜DSML｜` prefix is required — plain
/// `<parameter …>` in legitimate content is never touched.
fn extract_dsml_params(text: &str) -> Vec<(String, String)> {
    const DSML: &str = "\u{ff5c}DSML\u{ff5c}";
    let mut out = Vec::new();
    let mut from = 0usize;
    while let Some(p) = text[from..].find(DSML) {
        let seg = &text[from + p..];
        let Some(kpos) = seg.find("parameter name=") else {
            from += p + DSML.len();
            continue;
        };
        let after = &seg[kpos + "parameter name=".len()..];
        // Key in plain quotes or JSON-escaped quotes, then attribute junk
        // (` string="true"`) up to the `>` that starts the value.
        let (key, key_end) = if let Some(r) = after.strip_prefix("\\\"") {
            match r.find("\\\"") {
                Some(e) => (r[..e].to_string(), e + 4),
                None => {
                    from += p + DSML.len();
                    continue;
                }
            }
        } else {
            match after.strip_prefix('"').and_then(|r| r.find('"')) {
                Some(e) => (after[1..1 + e].to_string(), e + 2),
                None => {
                    from += p + DSML.len();
                    continue;
                }
            }
        };
        let Some(vrel) = after[key_end..].find('>') else {
            from += p + DSML.len();
            continue;
        };
        let value_start = kpos + "parameter name=".len() + key_end + vrel + 1;
        let value = match seg[value_start..].find("</parameter>") {
            Some(e) => seg[value_start..value_start + e].to_string(),
            // No closer: the segment ends where the JSON value ended —
            // take it verbatim (trailing newlines are content).
            None => seg[value_start..].to_string(),
        };
        let consumed = value.len().max(1);
        if !key.is_empty() {
            out.push((key, value));
        }
        from += p + value_start + consumed;
    }
    out
}

/// Parse the JSON value that starts at the beginning of `text` (after
/// leading whitespace), returning the value and the number of bytes it
/// consumed. Used by the salvage scanner to read a quoted string or an
/// array without requiring the rest of the blob to be valid JSON.
fn parse_json_value_prefix(text: &str) -> Option<(serde_json::Value, usize)> {
    let trimmed = text.trim_start();
    let first = trimmed.chars().next()?;
    if !matches!(first, '"' | '[' | '{' | '-' | '0'..='9' | 't' | 'f' | 'n') {
        return None;
    }
    let mut values = serde_json::Deserializer::from_str(trimmed).into_iter::<serde_json::Value>();
    let value = values.next()?.ok()?;
    Some((value, values.byte_offset()))
}

/// Keys a leaked call may legitimately carry. Curated tools expose their
/// canonical parameter list; `respond` (which has no alias table) still
/// needs its one field when a call is truncated mid-value.
fn salvage_keys(tool: Option<&str>) -> Vec<&'static str> {
    match tool {
        Some("respond") => vec!["response"],
        _ => tool
            .and_then(oz_core_types::canonical_param_names)
            .unwrap_or_default(),
    }
}

/// Collect every value following `"key"` in a corrupted arguments blob.
///
/// The gateway can splice arbitrary junk between the closing quote of a key
/// and its value — `"question">Go: "…"` (see the 2026-10-23 session),
/// `"file_path"｜>work/x.py</parameter><parameter: "…"`. Normal JSON has
/// `":"`; here the separator itself is damaged, so the scanner accepts a
/// `:` plus a JSON value (the duplicate half of a leaked call often keeps
/// one valid field), a `>` / `｜>` plus a short junk prefix and a JSON value,
/// or a `>` / `｜>` plus an unquoted value terminated by a parameter
/// boundary. Values are only ever read, never synthesized. Several occurrences of the
/// same key are common (the gateway repeats the call); the caller picks the
/// best one.
fn field_values(raw: &str, key: &str) -> Vec<serde_json::Value> {
    let needle = format!("\"{key}\"");
    let mut out = Vec::new();
    let mut from = 0usize;
    while let Some(rel) = raw[from..].find(&needle) {
        let pos = from + rel;
        from = pos + needle.len();
        // Only a key position counts: `{`, `,`, whitespace, or the end of a
        // parameter closer. A mention of `"question"` inside prose does not.
        let before_ok = raw[..pos]
            .chars()
            .next_back()
            .is_none_or(|c| c.is_whitespace() || matches!(c, '{' | ',' | ';' | '>' | ']'));
        if !before_ok {
            continue;
        }
        let mut after = raw[from..].trim_start();
        // The separator may be damaged in more than one way; consume all
        // leading separator glyphs.
        let mut stripped = true;
        while stripped {
            stripped = false;
            for sep in [':', '>', '\u{ff5c}', '|', '='] {
                if let Some(rest) = after.strip_prefix(sep) {
                    after = rest.trim_start();
                    stripped = true;
                }
            }
        }
        if after.is_empty() {
            continue;
        }
        // Intact `:` form (the duplicate half of a leaked call usually
        // keeps one valid field).
        if let Some((value, _)) = parse_json_value_prefix(after) {
            out.push(value);
            continue;
        }
        // Corrupted separator with a short prose gap: `Go: "…"`, `K=…\n…`.
        // A newline, a closer, or another parameter opener means the quoted
        // text belongs to a different field, so never cross it.
        let boundary = [
            after.find("</parameter>"),
            after.find("<parameter"),
            dsml_marker_index(after),
            after.find('\n'),
        ]
        .into_iter()
        .flatten()
        .min()
        .unwrap_or(after.len());
        let quote = after.find('"');
        if let Some(q) = quote {
            if q < boundary && q <= 64 {
                if let Some((value, _)) = parse_json_value_prefix(&after[q..]) {
                    out.push(value);
                    continue;
                }
            }
        }
        // Unquoted value (`"file_path"｜>work/extract.py</parameter>`).
        let end = boundary;
        let value = after[..end]
            .trim()
            .trim_matches(|c| c == ',' || c == '}')
            .trim();
        if !value.is_empty() && value.len() <= 16 * 1024 {
            out.push(serde_json::Value::String(value.to_string()));
        }
    }
    out
}

/// Pick one value from repeated/torn occurrences. The longest non-empty
/// string wins: the gateway often repeats a field, and the truncated copy
/// is usually the short one. Arrays/objects keep the first non-empty
/// occurrence.
fn pick_field_value(values: Vec<serde_json::Value>) -> Option<serde_json::Value> {
    let mut best_string: Option<String> = None;
    let mut best_container: Option<serde_json::Value> = None;
    for value in values {
        match value {
            serde_json::Value::String(s) if !s.trim().is_empty() => {
                if best_string.as_ref().is_none_or(|b| s.len() > b.len()) {
                    best_string = Some(s);
                }
            }
            serde_json::Value::Array(a) if !a.is_empty() => {
                best_container.get_or_insert(serde_json::Value::Array(a));
            }
            other if !other.is_null() => {
                best_container.get_or_insert(other);
            }
            _ => {}
        }
    }
    best_string
        .map(serde_json::Value::String)
        .or(best_container)
}

/// Last-resort reconstruction for calls whose JSON never parses. Scans the
/// tool's canonical fields by name; this is what recovers
/// `{"candidates": […], "question">Go: "…"}` (the corruption seen on
/// 2026-10-23) and `{"file_path"｜>…</parameter><parameter: "…"}`.
fn salvage_leaked_fields(
    raw: &str,
    tool: Option<&str>,
) -> serde_json::Map<String, serde_json::Value> {
    let mut out = serde_json::Map::new();
    let keys = salvage_keys(tool);
    if keys.is_empty() {
        return out;
    }
    // Merged-call corruption cannot be split back into the original calls:
    // the gateway folded several calls' fields into one value. Return the
    // rejection marker instead of a plausible-looking but wrong payload.
    if merged_calls_tail(raw, tool) {
        return oz_core_types::corrupt_call_marker(
            "the gateway merged several tool calls into one arguments blob; \
             its fields cannot be attributed to this call",
        )
        .as_object()
        .cloned()
        .unwrap_or_default();
    }
    for key in &keys {
        if let Some(value) = pick_field_value(field_values(raw, key)) {
            out.insert((*key).to_string(), value);
        }
    }
    // A nameless DSML/parameter value belongs to the first field the call
    // did not otherwise provide (write → content, code_run → code).
    if let Some(value) = extract_nameless_dsml_value(raw) {
        if let Some(key) = keys.iter().find(|k| !out.contains_key(**k)) {
            out.insert((*key).to_string(), serde_json::Value::String(value));
        }
    }
    out
}

/// Cut DSML pollution out of parsed string values and lift the swallowed
/// parameters back into the object.
///
/// Observed corruption (2026-10-03 evening session): the gateway delivers
/// VALID JSON whose string value silently continues past the model's
/// parameter boundary —
///   `{"file_path": "/p.sh</parameter>\n<｜DSML｜  parameter name=\"content\"
///    string=\"true\">#!/bin/bash\n…"}``
/// The JSON parses fine, so the repair path never runs, but `content` is
/// buried inside `file_path` and the tool fails `missing content`. When the
/// value contains `</parameter>` followed by DSML/parameter junk, cut it at
/// the boundary and promote the embedded named parameters.
fn sanitize_dsml_pollution(v: &mut serde_json::Value, tool: Option<&str>) {
    let serde_json::Value::Object(map) = v else {
        return;
    };
    let mut lifted: Vec<(String, String)> = Vec::new();
    // Set when a polluted tail carries another call's serialized arguments:
    // the call is unrecoverable and must be rejected, never executed.
    let mut merged: Option<String> = None;
    for (_, val) in map.iter_mut() {
        let serde_json::Value::String(s) = val else {
            continue;
        };
        // The boundary may be a named closer, a nameless opener, or the
        // DSML bracket itself. The 2026-10-23 run produced the last form:
        // `"file_path": "work/x.py</｜DSML｜:"` — valid JSON with the
        // dangling token swallowed into the path.
        let boundary = [
            s.find("</parameter>"),
            s.find("<parameter"),
            dsml_marker_index(s),
        ]
        .into_iter()
        .flatten()
        .min();
        let Some(boundary) = boundary else {
            continue;
        };
        let rest = &s[boundary..];
        // The value has already been JSON-parsed, so escaped unicode is
        // decoded — checking the literal fullwidth token is enough.
        // A DSML marker or a parameter opener after the boundary is junk; a
        // bare `</parameter>` that only mentions the tag is legitimate text.
        let followed_by_junk = rest.contains("<parameter") || dsml_marker_index(rest).is_some();
        if !followed_by_junk {
            continue;
        }
        if merged_calls_tail(rest, tool) {
            merged = Some(
                "the gateway merged several tool calls into one arguments blob; \
                 its fields cannot be attributed to this call"
                    .to_string(),
            );
        }
        lifted.extend(extract_dsml_params(rest));
        *s = s[..boundary].trim_end().to_string();
    }
    for (k, val) in lifted {
        map.entry(k).or_insert(serde_json::Value::String(val));
    }
    if let Some(reason) = merged {
        map.insert(
            oz_core_types::CORRUPT_CALL_KEY.to_string(),
            serde_json::Value::String(reason),
        );
    }
}

/// Recover degenerate tool-argument shapes some models emit, so tools get a
/// schema-shaped object and clean history stops teaching the model the bad
/// pattern:
/// - doubly-JSON-encoded arguments: the accumulated `arguments` string parses
///   to a JSON string whose text is the real argument object;
/// - single-key `{"content": "<json text>"}` wrappers (imitated by models
///   that saw sanitized history): the inner text is the real object;
/// - half-converted parameter-call fragments leaked by tool-call-extracting
///   gateways (`{"response">…`, see `repair_parameter_style_args`).
///
/// Applied repeatedly so multi-level wrappers collapse fully. Unrecognized
/// shapes pass through unchanged.
pub fn normalize_tool_args(v: serde_json::Value) -> serde_json::Value {
    normalize_tool_args_inner(v, None)
}

/// Shared body of [`normalize_tool_args`] / [`normalize_tool_args_for`].
/// `tool` is threaded through so the DSML sanitizer can tell a recoverable
/// leak from a gateway-merged foreign field.
fn normalize_tool_args_inner(
    mut current: serde_json::Value,
    tool: Option<&str>,
) -> serde_json::Value {
    // DSML pollution can ride inside otherwise-valid JSON (see
    // sanitize_dsml_pollution) — clean parsed objects before the unwrap
    // loop so degenerate-shape checks see the intended payload.
    if current.is_object() {
        sanitize_dsml_pollution(&mut current, tool);
    }
    for _ in 0..4 {
        let next = match &current {
            serde_json::Value::String(s) => serde_json::from_str::<serde_json::Value>(s)
                .map(|parsed| {
                    if parsed.is_object() {
                        let mut inner = parsed;
                        sanitize_dsml_pollution(&mut inner, tool);
                        inner
                    } else {
                        parsed
                    }
                })
                .ok()
                .filter(|inner| inner.is_object())
                .or_else(|| repair_parameter_style_args(s, None)),
            serde_json::Value::Object(m) if m.len() == 1 => {
                match m.get("content") {
                    // Wrapper with the payload as a JSON-encoded string…
                    Some(serde_json::Value::String(s)) => {
                        serde_json::from_str::<serde_json::Value>(s)
                            .ok()
                            .filter(|inner| inner.is_object())
                    }
                    // …or directly as an object.
                    Some(inner @ serde_json::Value::Object(_)) => Some(inner.clone()),
                    _ => None,
                }
            }
            _ => None,
        };
        match next {
            Some(inner) => {
                tracing::warn!(
                    "[oz-llm] normalized degenerate tool args: {} -> {}",
                    serde_json::to_string(&current)
                        .unwrap_or_default()
                        .chars()
                        .take(120)
                        .collect::<String>(),
                    serde_json::to_string(&inner)
                        .unwrap_or_default()
                        .chars()
                        .take(120)
                        .collect::<String>(),
                );
                current = inner;
            }
            None => break,
        }
    }
    current
}

/// [`normalize_tool_args`] plus canonical argument-key recovery for a specific
/// tool: models with weak schema adherence emit the right payload under the
/// wrong key (`cmd`/`command` for `code`, `path`/`file` for `file_path`, or the
/// tool name itself as the key), which the tool then rejects as a missing
/// argument. `oz_core_types::canonicalize_tool_args` re-keys those onto the
/// canonical names, so the event, the persisted history and the executed call
/// all agree — and a model that imitates its own history imitates the correct
/// shape. Execution is guarded a second time in `ToolRegistry::dispatch`.
pub fn normalize_tool_args_for(tool: &str, v: serde_json::Value) -> serde_json::Value {
    // Nameless-DSML repair needs the tool's primary key, which only this
    // function knows. Two string shapes reach here: strict JSON rejects the
    // blob (the usual leaked-call case), or it parses to another *string*
    // (the gateway double-encoded it). Both are repaired against the tool.
    // The repaired value then goes through the ordinary pipeline ON PURPOSE:
    // `normalize_tool_args` runs the DSML sanitizer, which is what strips a
    // polluted `file_path` (the 2026-10-23 11:31 run returned the repaired
    // object early and the polluted path reached the write tool).
    let mut v = v;
    if let serde_json::Value::String(s) = &v {
        let repair_source = match serde_json::from_str::<serde_json::Value>(s) {
            Ok(serde_json::Value::String(inner)) => Some(inner),
            Ok(_) => None,
            Err(_) => Some(s.clone()),
        };
        if let Some(source) = repair_source {
            if let Some(repaired) = repair_parameter_style_args(&source, Some(tool)) {
                v = repaired;
            }
        }
    }
    let normalized = normalize_tool_args_inner(v, Some(tool));
    let canonical = oz_core_types::canonicalize_tool_args(tool, normalized.clone());
    if canonical != normalized {
        tracing::warn!(
            "[oz-llm] re-keyed tool args for {tool}: keys {:?} -> keys {:?}",
            oz_core_types::arg_keys(&normalized),
            oz_core_types::arg_keys(&canonical),
        );
    }
    canonical
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

    #[test]
    fn normalize_unwraps_double_encoded_arguments() {
        let inner = serde_json::json!({"code": "cd /tmp"});
        let wrapped = serde_json::Value::String(inner.to_string());
        assert_eq!(normalize_tool_args(wrapped), inner);
    }

    #[test]
    fn normalize_unwraps_single_content_wrapper() {
        let inner = serde_json::json!({"code": "ls"});
        let wrapped = serde_json::json!({"content": inner.to_string()});
        assert_eq!(normalize_tool_args(wrapped), inner);
    }

    #[test]
    fn normalize_leaves_plain_content_string_alone() {
        // A write call's file body is not JSON — must pass through untouched.
        let v = serde_json::json!({"content": "#!/usr/bin/env node\nplain text"});
        assert_eq!(normalize_tool_args(v.clone()), v);
    }

    #[test]
    fn normalize_leaves_valid_objects_alone() {
        let v = serde_json::json!({"code": "echo hi", "timeout": 30});
        assert_eq!(normalize_tool_args(v.clone()), v);
    }

    #[test]
    fn normalize_leaves_unparseable_string_alone() {
        // Truncated stream salvage: keep the string so the tool reports a
        // structured missing-arg error instead of executing guessed input.
        let v = serde_json::Value::String("{\"code\":\"cd ~/x".into());
        assert_eq!(normalize_tool_args(v.clone()), v);
    }

    #[test]
    fn normalize_leaves_non_json_content_wrapper_alone() {
        let v = serde_json::json!({"content": "{\"code\": broken"});
        assert_eq!(normalize_tool_args(v.clone()), v);
    }

    #[test]
    fn normalize_for_tool_unwraps_then_rekeys() {
        // The wrapper and the alias can appear together: collapse the wrapper
        // first, then map the alias onto the canonical `code`.
        let v = serde_json::json!({"content": "{\"cmd\":\"ls -la\"}"});
        let got = normalize_tool_args_for("code_run", v);
        assert_eq!(got["code"], "ls -la");
    }

    #[test]
    fn normalize_for_tool_rekeys_alias_only_payload() {
        let got = normalize_tool_args_for(
            "code_run",
            serde_json::json!({"cmd": "ls", "mode": "bash", "timeout": 5}),
        );
        assert_eq!(got["code"], "ls");
        assert_eq!(got["type"], "bash");
    }

    #[test]
    fn normalize_for_tool_keeps_write_body_untouched() {
        // A file body is not arguments — it must survive verbatim.
        let v = serde_json::json!({"file_path": "/tmp/a.txt", "content": "{\"not\":\"args\"}"});
        let got = normalize_tool_args_for("write", v.clone());
        assert_eq!(got, v);
    }

    #[test]
    fn normalize_for_tool_passes_canonical_args_through() {
        let v = serde_json::json!({"code": "echo hi", "timeout": 30});
        assert_eq!(normalize_tool_args_for("code_run", v.clone()), v);
    }

    // ── repair_parameter_style_args (real gateway leaks, 2026-10-03 noon
    //    session fd68d2e3: respond args arrived as half-converted DSML) ──

    #[test]
    fn repair_recovers_truncated_respond_reply() {
        // Raw accumulated `arguments` string: leading stray quote, the
        // opening tag reduced to `{`, no closing tag, dangling `: "`.
        let raw =
            r#""{"response">我按你描述锁定了账号，并把它归到具体的绘画技术体系，再给出可落地的: ""#;
        let got = repair_parameter_style_args(raw, None).expect("should repair");
        assert_eq!(
            got["response"],
            "我按你描述锁定了账号，并把它归到具体的绘画技术体系，再给出可落地的"
        );
    }

    #[test]
    fn repair_recovers_truncated_multiline_reply() {
        let raw = r#""{"response">已按验收断言修复并通过复验。

**做了什么**
-: ""#;
        let got = repair_parameter_style_args(raw, None).expect("should repair");
        assert_eq!(
            got["response"],
            "已按验收断言修复并通过复验。\n\n**做了什么**\n-"
        );
    }

    #[test]
    fn repair_recovers_mixed_half_converted_and_escaped_fragments() {
        // A web_fetch call that arrived as a frankenstring: half-converted
        // first parameter, intact-but-escaped DSML parameters after it, a
        // JSON-ish tail the extractor cannot express as pairs.
        let raw = concat!(
            r#"{"max_chars">2500</parameter>"#,
            "\n",
            r#"<parameter: "SDXL Flux 人体解剖 LoRA 骨骼 肌肉 结构 素描 模型 civitai</parameter>"#,
            "\n",
            r#"<parameter name=\"num_results\">5</parameter>"#,
            "\n",
            r#"<parameter name=\"query\" string=\"true\">明婷画画 怎么画的 工具 数位板 比例 测量 透视</parameter>"#,
        );
        let got = repair_parameter_style_args(raw, None).expect("should repair");
        assert_eq!(got["max_chars"], "2500");
        assert_eq!(got["num_results"], "5");
        assert_eq!(got["query"], "明婷画画 怎么画的 工具 数位板 比例 测量 透视");
    }

    #[test]
    fn repair_handles_intact_xml_parameters() {
        let raw = "<parameter name=\"url\">https://example.com/x</parameter>\n<parameter name=\"q\" string=\"true\">multi\nline</parameter>";
        let got = repair_parameter_style_args(raw, None).expect("should repair");
        assert_eq!(got["url"], "https://example.com/x");
        assert_eq!(got["q"], "multi\nline");
    }

    #[test]
    fn repair_preserves_quotes_in_closed_values() {
        // The dangling-quote strip only applies to truncated tails; a value
        // closed by `</parameter>` may legitimately end with a quote.
        let raw = "<parameter name=\"q\">say \"hi\"</parameter>";
        let got = repair_parameter_style_args(raw, None).expect("should repair");
        assert_eq!(got["q"], "say \"hi\"");
    }

    #[test]
    fn repair_reconstructs_json_after_junk_prefix() {
        // Real 2026-10-03 leak: the model's complete JSON arguments arrived
        // with a mangled half-open key and a `<parameter: ` splice in front
        // of the first value. Pair extraction recovers only file_path; the
        // reconstruction tier must recover content too.
        let raw = concat!(
            r##"{"file_path"><parameter: "/Users/w/work/probe_env.sh", "##,
            r##""content": "#!/bin/bash\nC=/w/comfyUI\necho \"== ver ==\"\n"}"##
        );
        let got = repair_parameter_style_args(raw, None).expect("should repair");
        assert_eq!(got["file_path"], "/Users/w/work/probe_env.sh");
        assert_eq!(
            got["content"],
            "#!/bin/bash\nC=/w/comfyUI\necho \"== ver ==\"\n"
        );
    }

    #[test]
    fn repair_reconstruction_never_fires_on_pair_shapes() {
        // The pre-existing fixtures must keep resolving through pair
        // extraction exactly as before (tier 1 must not swallow them).
        let raw =
            r#""{"response">我按你描述锁定了账号，并把它归到具体的绘画技术体系，再给出可落地的: ""#;
        let got = repair_parameter_style_args(raw, None).expect("should repair");
        assert_eq!(
            got["response"],
            "我按你描述锁定了账号，并把它归到具体的绘画技术体系，再给出可落地的"
        );
        let raw2 = "<parameter name=\"url\">https://example.com/x</parameter>\n<parameter name=\"q\" string=\"true\">multi\nline</parameter>";
        let got2 = repair_parameter_style_args(raw2, None).expect("should repair");
        assert_eq!(got2["url"], "https://example.com/x");
        assert_eq!(got2["q"], "multi\nline");
        // Truncated bare-value shape still degrades to pair extraction.
        let raw3 = r#"{"file_path">/Users/w/work/probe_env.sh</parameter>"#;
        let got3 = repair_parameter_style_args(raw3, None).expect("should repair");
        assert_eq!(got3["file_path"], "/Users/w/work/probe_env.sh");
    }

    #[test]
    fn repair_rejects_plain_text_and_truncated_json() {
        // No parameter opener at all.
        assert_eq!(
            repair_parameter_style_args("plain text no params", None),
            None
        );
        // Truncated proper JSON — the `":` terminator must NOT be mistaken
        // for the half-converted `">` leak signature (this shape keeps the
        // string fallback so the tool reports a structured missing-arg
        // error instead of executing guessed input).
        assert_eq!(
            repair_parameter_style_args("{\"code\":\"cd ~/x", None),
            None
        );
        assert_eq!(repair_parameter_style_args("{\"a\": \"b\"}", None), None);
        assert_eq!(repair_parameter_style_args("", None), None);
    }

    #[test]
    fn sanitize_lifts_dsml_params_swallowed_into_values() {
        // 2026-10-03 evening: VALID JSON whose string value continues past
        // the model's parameter boundary — the gateway embedded the next
        // DSML parameter inside the value. Strict parse succeeds, so only
        // the value-level sanitizer can recover the buried parameter.
        let raw = concat!(
            r##"{"file_path": "/Users/w/work/comfy_probe.sh</parameter>\n"##,
            r##"<｜DSML｜  parameter name=\"content\" string=\"true\">#!/bin/bash\n# 启动\nset -uo pipefail\n"}"##
        );
        let got = normalize_tool_args_for("write", serde_json::Value::String(raw.into()));
        assert_eq!(got["file_path"], "/Users/w/work/comfy_probe.sh");
        assert_eq!(got["content"], "#!/bin/bash\n# 启动\nset -uo pipefail\n");

        let raw2 = concat!(
            r##"{"content": "探测本机 ComfyUI 能力</parameter>\n"##,
            r##"<｜DSML｜  parameter name=\"priority\" string=\"true\">high</parameter>\n"}"##
        );
        let got2 = normalize_tool_args_for("todowrite", serde_json::Value::String(raw2.into()));
        assert_eq!(got2["content"], "探测本机 ComfyUI 能力");
        assert_eq!(got2["priority"], "high");
    }

    #[test]
    fn sanitize_leaves_legitimate_parameter_text_alone() {
        // A value that merely MENTIONS </parameter> without DSML junk after
        // it must survive byte-for-byte.
        let v = serde_json::json!({"content": "the closing tag is </parameter> here"});
        assert_eq!(normalize_tool_args(v.clone()), v);
        // Already-clean objects pass through.
        let v2 = serde_json::json!({"file_path": "/tmp/a.sh", "content": "#!/bin/bash\necho hi"});
        assert_eq!(normalize_tool_args(v2.clone()), v2);
    }

    #[test]
    fn repair_maps_nameless_dsml_value_to_primary_param() {
        // 2026-10-03 evening (write relay-howto.md): half-open key, bare
        // value, then the gateway's nameless DSML value carrying the whole
        // markdown body. The nameless value must land on `content`.
        let raw = concat!(
            r##"{"file_path">/Users/w/work/relay-howto.md</parameter>\n"##,
            r##"<｜DSML｜: "# 还原\"明婷画画\"式作画过程\n\n> 2026-10-03\n"}"##
        );
        let got = normalize_tool_args_for("write", serde_json::Value::String(raw.into()));
        assert_eq!(got["file_path"], "/Users/w/work/relay-howto.md");
        assert_eq!(
            got["content"],
            "# 还原\"明婷画画\"式作画过程\n\n> 2026-10-03\n"
        );
    }

    #[test]
    fn normalize_repairs_leaked_parameter_string() {
        // End-to-end: the openai stream terminator wraps an unparseable
        // arguments string as Value::String; normalize must recover the
        // object so the agent loop can read `response` again.
        let v = serde_json::Value::String(r#""{"response">最终回复正文""#.into());
        let got = normalize_tool_args_for("respond", v);
        assert_eq!(got["response"], "最终回复正文");
    }

    // ── 2026-10-23 desktop session: separator corruption + double encoding ──

    #[test]
    fn repair_recovers_question_after_go_separator_leak() {
        // Real bytes from session 4596ac63: the gateway replaced the JSON
        // `": "` separator with `">Go: "` and repeated the question dozens
        // of times. The candidates array stayed valid. The tool previously
        // received an empty question and the loop waited 5 minutes for it.
        let raw = concat!(
            r##"{"candidates": ["只接 haiku-5-5（Go），sonnet 先跳过", "sonnet 走 Zen 按量侧"], "##,
            r##""question">Go: "探测完成。核心结论：haiku 可用，sonnet 不可用。", "##,
            r##""question">Go: "探测完成。核心结论：haiku 可用，sonnet 不可用。"}"##
        );
        let got = repair_parameter_style_args(raw, Some("ask_user")).expect("repair");
        assert_eq!(
            got["question"],
            "探测完成。核心结论：haiku 可用，sonnet 不可用。"
        );
        assert_eq!(got["candidates"][0], "只接 haiku-5-5（Go），sonnet 先跳过");
        assert_eq!(got["candidates"][1], "sonnet 走 Zen 按量侧");
    }

    #[test]
    fn normalize_recovers_double_encoded_ask_user_with_go_leak() {
        // The same call as it arrives from the stream: a JSON *string* whose
        // body is the corrupted arguments object.
        let inner = concat!(
            r##"{"candidates": ["a", "b"], "##,
            r##""question">Go: "第一行"}"##
        );
        let wrapped = serde_json::Value::String(serde_json::to_string(inner).unwrap());
        let got = normalize_tool_args_for("ask_user", wrapped);
        assert_eq!(got["question"], "第一行");
        assert_eq!(got["candidates"][0], "a");
    }

    #[test]
    fn repair_recovers_fullwidth_separator_and_nameless_content() {
        // Real bytes: `{"file_path"｜>work/dsh/extract_all.py</parameter>
        // <parameter: "…content…", "file_path": "…"}`. The opening quote is
        // missing and the content parameter lost its name.
        let raw = concat!(
            r##"{"file_path"｜>work/dsh/extract_all.py</parameter>"##,
            "\n",
            r##"<parameter: "import json\nprint(1)\n", "file_path": "work/dsh/extract_all.py"}"##
        );
        let got = normalize_tool_args_for("write", serde_json::Value::String(raw.into()));
        assert_eq!(got["file_path"], "work/dsh/extract_all.py");
        assert_eq!(got["content"], "import json\nprint(1)\n");
    }

    #[test]
    fn repair_keeps_the_intact_duplicate_of_a_torn_field() {
        // Real bytes: a valid `code` followed by a second `"code">K=…`
        // fragment. The valid one must win; the junk must not become code.
        let raw = concat!(
            r##"{"code": "cd /tmp && ls", "type": "bash", "##,
            r##""code">K=secret\necho: "cd /tmp && ls", "type": "bash"}"##
        );
        let got = normalize_tool_args_for("code_run", serde_json::Value::String(raw.into()));
        assert_eq!(got["code"], "cd /tmp && ls");
        assert_eq!(got["type"], "bash");
    }

    #[test]
    fn real_20261023_ask_user_leak_is_repaired() {
        // Exact accumulated arguments from desktop session 4596ac63
        // (09:37 local). The gateway replaced `": "` with `">Go: "` and
        // repeated the same question; without repair the tool saw an empty
        // question and the loop parked on it eight times.
        let raw = include_str!("../tests/fixtures/ask_user_go_leak_20261023.txt").trim();
        let got = normalize_tool_args_for("ask_user", serde_json::Value::String(raw.to_string()));
        let question = got["question"].as_str().unwrap_or("");
        assert!(!question.trim().is_empty(), "question must be recovered");
        assert!(question.contains("探测"), "got: {question:?}");
        assert_eq!(got["candidates"].as_array().map(|a| a.len()), Some(4));
    }

    #[test]
    fn real_20261023_ask_user_question_survives_every_go_leak_variant() {
        // The same question reached the tool in three different leaked shapes
        // within one run (session 4596ac63, 10:33 local). In two of them the
        // gateway had also repeated the `candidates` array *in front of* the
        // question, so a repair that stopped at the first `>Go: ` boundary
        // returned the candidates text as the question — or an empty one,
        // which is what produced 16 blank dialogs in the UI.
        for (label, raw) in [
            (
                "repeated",
                include_str!("../tests/fixtures/ask_user_repeated_go_leak_20261023.txt"),
            ),
            (
                "short",
                include_str!("../tests/fixtures/ask_user_go_leak_short_20261023.txt"),
            ),
            (
                "mid",
                include_str!("../tests/fixtures/ask_user_go_leak_mid_20261023.txt"),
            ),
        ] {
            let raw = raw.trim();
            let got =
                normalize_tool_args_for("ask_user", serde_json::Value::String(raw.to_string()));
            let question = got["question"].as_str().unwrap_or("");
            assert!(
                !question.trim().is_empty(),
                "{label}: question must never come back empty"
            );
            assert!(
                question.contains("探测"),
                "{label}: question lost its text: {question:?}"
            );
            assert!(
                !question.contains("\"candidates\""),
                "{label}: candidates leaked into the question: {question:?}"
            );
            assert_eq!(
                got["candidates"].as_array().map(|a| a.len()),
                Some(4),
                "{label}: candidates must survive too"
            );
        }
    }

    #[test]
    fn real_20261023_merged_calls_do_not_pollute_the_path() {
        // Real bytes from the 11:31 run on the installed build: the gateway
        // merged THREE following code_run calls into the `file_path` VALUE of
        // a write call. The path is cleaned at the DSML boundary, but the
        // surviving `content` belongs to a code_run — so the call must carry
        // the corruption marker and never be executed.
        let raw = include_str!("../tests/fixtures/write_merged_calls_20261023.txt").trim();
        let v: serde_json::Value = serde_json::from_str(raw).expect("fixture is valid JSON");
        let got = normalize_tool_args_for("write", v);
        assert_eq!(
            got["file_path"], "/home/user/.openzen/workspace/work/dsh/probe2.mjs",
            "path must be cut at the DSML boundary; got {}",
            got["file_path"]
        );
        assert!(
            !got["file_path"].as_str().unwrap_or("").contains("DSML"),
            "no DSML marker may survive in the path"
        );
        assert!(
            oz_core_types::corrupt_call_reason(&got).is_some(),
            "merged calls must be marked corrupted; got {got}"
        );
    }

    #[test]
    fn real_20261023_write_leak_is_repaired() {
        // Exact accumulated arguments: `"file_path"｜>path</parameter>
        // <parameter: "<content>"` — no opening quote, no parameter name.
        let raw = include_str!("../tests/fixtures/write_fullwidth_leak_20261023.txt").trim();
        let got = normalize_tool_args_for("write", serde_json::Value::String(raw.to_string()));
        assert_eq!(got["file_path"], "work/dsh/extract_all.py");
        let content = got["content"].as_str().unwrap_or("");
        assert!(content.contains("import json"), "content: {content:?}");
    }

    #[test]
    fn sanitize_cuts_dangling_dsml_marker_from_path() {
        // Real bytes: valid JSON whose file_path swallowed `</｜DSML｜:`.
        let raw = r#"{"file_path": "work/dsh/extract.py</｜DSML｜:"}"#;
        let got = normalize_tool_args_for("write", serde_json::Value::String(raw.into()));
        assert_eq!(got["file_path"], "work/dsh/extract.py");
    }

    #[test]
    fn closer_dsml_spelling_is_cut_from_path() {
        // Real bytes from the 2026-10-23 12:26 run: the closer form
        // `｜/DSML｜:` (not `</｜DSML｜`) trailed the path.
        let raw = "{\"file_path\": \"/Users/w/work/dsh/verify_task.py\u{ff5c}/DSML\u{ff5c}:\"}";
        let got = normalize_tool_args_for("write", serde_json::Value::String(raw.into()));
        assert_eq!(got["file_path"], "/Users/w/work/dsh/verify_task.py");
        assert!(oz_core_types::corrupt_call_reason(&got).is_none());
    }
}
