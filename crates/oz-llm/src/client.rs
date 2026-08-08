use oz_core_types::{
    ContentBlock, ContentContainer, LlmClient, LlmError, Message,
    MockResponse, MockToolCall, Role, StreamEvent, ToolDefinition,
};
use tokio::sync::mpsc::UnboundedSender;

use crate::session::{blocks_to_response, Session};

pub struct NativeToolClient {
    backend: Box<dyn Session>,
    thinking_prompt_zh: &'static str,
    thinking_prompt_en: &'static str,
}

impl NativeToolClient {
    pub fn new(backend: Box<dyn Session>) -> Self {
        NativeToolClient {
            backend,
            thinking_prompt_zh: "\n### Action Protocol\nAlways include <summary>snapshot</summary> in each reply. If user request is not complete, call tools.\n### Parallel Tool Execution\nWhen the next step needs multiple independent tools (e.g. reading several files, multiple greps), emit them all in a single turn — the runtime executes them concurrently. Reserve serial ordering only when a call's input depends on another's output.\n### Self-Optimization (internal, do not narrate)\nPre-fetch obvious multi-read batches in the first turn. Fold independent greps/globs into one call. Don't repeat the same read-only probe >3 times. When >80% of a turn is thinking with few tool calls, batch the probes instead. Use working_mem for cross-turn facts. Stop serial patch chains after 2 patches and rewrite via write.",
            thinking_prompt_en: "\n### Action Protocol (always in effect)\nThe reply body should first include a minimal one-line (<30 words) physical snapshot in <summary></summary>: new info from last result + current intent. This goes into long-term working memory.\n**If the user's request is not yet complete, tool calls are required!**\n### Parallel Tool Execution\nWhen the next step needs multiple independent tools (e.g. reading several files, multiple greps), emit them all in a single turn — the runtime executes them concurrently. Reserve serial ordering only when a call's input depends on another's output.\n### Self-Optimization (internal, do not narrate)\nPre-fetch obvious multi-read batches in the first turn. Fold independent greps/globs into one call. Don't repeat the same read-only probe >3 times. When >80% of a turn is thinking with few tool calls, batch the probes instead. Use working_mem for cross-turn facts. Stop serial patch chains after 2 patches and rewrite via write.",
        }
    }
}

#[async_trait::async_trait]
impl LlmClient for NativeToolClient {
    async fn chat(&mut self, messages: &[Message], tools: &[ToolDefinition]) -> Result<MockResponse, LlmError> {
        // Fold ALL system messages (main prompt + any injected compression
        // summaries) into the backend system prompt. The last system message
        // must NOT clobber the earlier ones.
        let system_parts: Vec<String> = messages.iter()
            .filter(|m| m.role == Role::System)
            .map(|m| m.content_text())
            .filter(|t| !t.is_empty())
            .collect();
        if !system_parts.is_empty() {
            self.set_system_with_thinking(&system_parts.join("\n\n"));
        }

        // Send the caller's messages DIRECTLY. agent_loop passes the full
        // conversation every turn and its compression shrinks exactly this
        // vec — accumulating a private copy in backend.history (as this
        // method used to) bypassed compression and resent stale turns
        // forever, keeping est_tokens stuck at ~180K.
        let raw_messages = build_request_messages(messages);

        let (blocks, usage) = self.backend.raw_ask(&raw_messages).await?;
        let (thinking, content, mut tool_calls) = blocks_to_response(&blocks);

        if tool_calls.is_empty() {
            let (parsed, _remaining) = parse_text_tool_calls(&content);
            tool_calls = parsed;
        }

        let raw = serde_json::to_string(&blocks).unwrap_or_default();
        let stop = if tool_calls.is_empty() { "end_turn".into() } else { "tool_use".into() };
        Ok(MockResponse {
            thinking,
            content,
            tool_calls,
            raw,
            stop_reason: stop,
            usage,
        })
    }

    async fn stream_chat(
        &mut self,
        messages: &[Message],
        tools: &[ToolDefinition],
        event_tx: UnboundedSender<StreamEvent>,
        _speculative_tx: Option<UnboundedSender<StreamEvent>>,
    ) -> Result<MockResponse, LlmError> {
        if !tools.is_empty() {
            self.backend.set_tools(tools.to_vec());
        }

        let system_parts: Vec<String> = messages.iter()
            .filter(|m| m.role == Role::System)
            .map(|m| m.content_text())
            .filter(|t| !t.is_empty())
            .collect();
        if !system_parts.is_empty() {
            self.set_system_with_thinking(&system_parts.join("\n\n"));
        }

        let raw_messages = build_request_messages(messages);

        let (blocks, usage) = self.backend.raw_ask_streaming(&raw_messages, event_tx, _speculative_tx).await?;
        let (thinking, content, mut tool_calls) = blocks_to_response(&blocks);

        if tool_calls.is_empty() {
            let (parsed, _remaining) = parse_text_tool_calls(&content);
            tool_calls = parsed;
        }

        let raw = serde_json::to_string(&blocks).unwrap_or_default();
        let stop = if tool_calls.is_empty() { "end_turn".into() } else { "tool_use".into() };
        Ok(MockResponse {
            thinking,
            content,
            tool_calls,
            raw,
            stop_reason: stop,
            usage,
        })
    }
}

impl NativeToolClient {
    fn set_system_with_thinking(&mut self, extra_system: &str) {
        let lang = std::env::var("OZ_LANG").unwrap_or_default();
        let thinking = if lang == "en" { self.thinking_prompt_en } else { self.thinking_prompt_zh };
        let combined = if extra_system.is_empty() {
            thinking.to_string()
        } else {
            format!("{extra_system}\n\n{thinking}")
        };
        self.backend.set_system(combined);
    }
}

/// Clone caller messages into the wire format: system messages are dropped
/// (they were folded into the backend system prompt via set_system) and any
/// pending results from the LAST user message are merged into its content
/// blocks (the legacy `tool_results` field is never sent).
fn build_request_messages(messages: &[Message]) -> Vec<Message> {
    let last_user_idx = messages.iter().rposition(|m| m.role == Role::User);
    let mut out = Vec::with_capacity(messages.len());
    for (i, m) in messages.iter().enumerate() {
        if m.role == Role::System {
            continue;
        }
        let mut content = m.content.clone();
        if Some(i) == last_user_idx {
            if let Some(ref trs) = m.tool_results {
                for tr in trs {
                    content.push(ContentBlock::ToolResult {
                        tool_use_id: tr.tool_use_id.clone(),
                        content: ContentContainer::Text(tr.content.clone()),
                        is_error: None,
                    });
                }
            }
        }
        out.push(Message { role: m.role, content, tool_results: None });
    }
    out
}

fn parse_text_tool_calls(content: &str) -> (Vec<MockToolCall>, String) {
    let mut tcs = Vec::new();
    let mut remaining = content.to_string();

    for prefix in &[r#"[{"type":"tool_use""#, r#"[{"type": "tool_use""#] {
        if let Some(pos) = remaining.find(prefix) {
            if remaining.ends_with("}]") {
                let json_str = &remaining[pos..];
                if let Ok(arr) = serde_json::from_str::<Vec<serde_json::Value>>(json_str) {
                    for item in &arr {
                        if item["type"] == "tool_use" {
                            let name = item["name"].as_str().unwrap_or("").to_string();
                            let input = item.get("input").cloned().unwrap_or_default();
                            let id = item.get("id").and_then(|v| v.as_str()).unwrap_or("").to_string();
                            tcs.push(MockToolCall::with_id(name, input, id));
                        }
                    }
                    remaining = remaining[..pos].trim().to_string();
                    return (tcs, remaining);
                }
            }
        }
    }

    // NOTE: the regex crate does not support look-around, so nested-tag
    // exclusion is approximated with a lazy `[\s\S]{15,}?` match.
    if let Ok(re) = regex::Regex::new(r#"<(tool_use|tool_call)>([\s\S]{15,}?)</(?:tool_use|tool_call)>"#) {
        let mut new_remaining = remaining.clone();
        for cap in re.captures_iter(&remaining) {
            let inner = cap.get(2).map(|m| m.as_str()).unwrap_or("");
            if let Ok(d) = serde_json::from_str::<serde_json::Value>(inner) {
                let name = d.get("name").or_else(|| d.get("function")).and_then(|v| v.as_str()).unwrap_or("").to_string();
                let input = d.get("arguments").or_else(|| d.get("args")).or_else(|| d.get("input")).cloned().unwrap_or_default();
                if !name.is_empty() {
                    tcs.push(MockToolCall::new(name, input));
                }
            }
        }
        if !tcs.is_empty() {
            if let Ok(re2) = regex::Regex::new(r#"<(?:tool_use|tool_call)>.*?</(?:tool_use|tool_call)>"#) {
                new_remaining = re2.replace_all(&remaining, "").to_string();
            }
            remaining = new_remaining.trim().to_string();
        }
    }

    // Cursor / Claude Code style: <function name="...">...</function> or <invoke name="...">...</invoke>.
    // Inner content is either a JSON object or a series of <parameter name="key">value</parameter>.
    let param_re = regex::Regex::new(
        r#"<parameter(?:\s+name="([^"]+)"|=([^>\s]+))?\s*>([\s\S]*?)</parameter>"#,
    ).ok();
    if let Ok(fn_re) = regex::Regex::new(
        r#"<(function|invoke)(?:\s+name="([^"]+)"|=([^>\s]+))?\s*>([\s\S]*?)</(function|invoke)>"#,
    ) {
        for cap in fn_re.captures_iter(&remaining) {
            let name = cap.get(2).or_else(|| cap.get(3))
                .map(|m| m.as_str().trim().to_string())
                .unwrap_or_default();
            let inner = cap.get(4).map(|m| m.as_str()).unwrap_or("").trim();

            if name.is_empty() { continue; }

            let input = if let Ok(d) = serde_json::from_str::<serde_json::Value>(inner) {
                d
            } else {
                let mut args = serde_json::Map::new();
                if let Some(ref param_re) = param_re {
                    for pcap in param_re.captures_iter(inner) {
                        let key = pcap.get(1).or_else(|| pcap.get(2))
                            .map(|m| m.as_str().trim().to_string())
                            .unwrap_or_default();
                        let val = pcap.get(3).map(|m| m.as_str().trim()).unwrap_or("");
                        if !key.is_empty() {
                            args.insert(key, serde_json::Value::String(val.to_string()));
                        }
                    }
                }
                if args.is_empty() {
                    // Fallback: treat the raw inner content as a `response` argument
                    let mut fallback = serde_json::Map::new();
                    if !inner.is_empty() {
                        fallback.insert("response".into(), serde_json::Value::String(inner.to_string()));
                    }
                    serde_json::Value::Object(fallback)
                } else {
                    serde_json::Value::Object(args)
                }
            };

            tcs.push(MockToolCall::new(name, input));
        }
        if !tcs.is_empty() {
            // Strip matched function/invoke blocks and any stray <parameter> tags
            let strip_re = regex::Regex::new(
                r#"<(function|invoke)(?:\s+[^>]*)?\s*>[\s\S]*?</(function|invoke)>|<parameter(?:\s+[^>]*)?\s*>[\s\S]*?</parameter>"#,
            ).unwrap();
            remaining = strip_re.replace_all(&remaining, "").to_string().trim().to_string();
        }
    }

    // ── Layer 4: Markdown code block containing JSON tool call ──
    // Models like DeepSeek/Qwen often output:
    // ```json
    // {"name": "read", "arguments": {"path": "/tmp/foo.txt"}}
    // ```
    // Best-effort: regex only handles shallow nesting. Complex nested
    // args rely on the native function-calling path (tool_choice: required).
    if tcs.is_empty() {
        if let Ok(re) = regex::Regex::new(
            r"(?s)```(?:json)?\s*\n?(\{(?:[^{}]|\{[^{}]*\})*\})\s*\n?```"
        ) {
            for cap in re.captures_iter(&remaining) {
                let json_str = cap.get(1).map(|m| m.as_str()).unwrap_or("");
                if let Ok(d) = serde_json::from_str::<serde_json::Value>(json_str) {
                    let name = d.get("name").and_then(|v| v.as_str()).unwrap_or("").to_string();
                    if !name.is_empty() {
                        let args = d.get("arguments")
                            .or_else(|| d.get("args"))
                            .or_else(|| d.get("parameters"))
                            .cloned().unwrap_or_default();
                        tcs.push(MockToolCall::new(name, args));
                    }
                }
            }
            if !tcs.is_empty() {
                let strip_re = regex::Regex::new(
                    r"```(?:json)?\s*\n?\{[^`]*\}\s*\n?```",
                ).unwrap_or_else(|_| regex::Regex::new(r"").unwrap());
                remaining = strip_re.replace_all(&remaining, "").to_string().trim().to_string();
            }
        }
    }

    // ── Layer 5: Bare JSON object embedded in text ──
    // Models like DeepSeek/Qwen sometimes embed a tool-call JSON
    // directly in prose without any markup wrapper.
    if tcs.is_empty() {
        if let Ok(re) = regex::Regex::new(
            r#""name"\s*:\s*"([^"]+)"\s*,\s*"(?:arguments|args|parameters)"\s*:\s*(\{(?:[^{}]|\{[^{}]*\})*\})"#
        ) {
            if let Some(cap) = re.captures(&remaining) {
                let name = cap.get(1).map(|m| m.as_str()).unwrap_or("").to_string();
                let args_str = cap.get(2).map(|m| m.as_str()).unwrap_or("{}");
                if !name.is_empty() {
                    if let Ok(args) = serde_json::from_str::<serde_json::Value>(args_str) {
                        tcs.push(MockToolCall::new(name, args));
                        remaining = re.replace(&remaining, "").to_string().trim().to_string();
                    }
                }
            }
        }
    }

    (tcs, remaining)
}
