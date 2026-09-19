use oz_core_types::{ContentBlock, ContentContainer, Message, Role};

/// Convert Claude content-block format messages to OpenAI format.
/// Matches Python _msgs_claude2oai
///
/// Strict gateways (opencode.ai zen "Go", GLM) validate the tool protocol the
/// way OpenAI defines it: a `tool` message must directly answer the assistant
/// `tool_calls` that precedes it, and every `tool_calls` entry must be
/// answered before the next non-tool message. Persisted history can violate
/// all of it — a terminal control call (`respond`) is stored as a tool_use
/// with no result, a result can outlive its call, and user text that travelled
/// with a batch of tool results used to be emitted before them, which put a
/// `user` message between `tool_calls` and its `tool` responses.
///
/// Repair rules: drop `tool_calls` whose input is not a JSON object (a stream
/// truncated mid-arguments was salvaged as a bare string, so the call could
/// never have run — removal is lossless); answer any remaining unpaired call
/// with a synthetic `tool` response; drop `tool` results whose call is not the
/// immediately preceding assistant's; emit tool responses before any user text
/// that shared the message.
pub fn msgs_claude2oai(messages: &[Message], _model: &str) -> Vec<serde_json::Value> {
    let mut result: Vec<serde_json::Value> = Vec::new();
    // Synthetic `tool` responses for calls whose result was never recorded.
    // They are inserted directly after the assistant message that made them.
    let mut pending_synth: Vec<serde_json::Value> = Vec::new();

    // Ids answered by the turn at `i + 1` (a user turn carrying tool results).
    let answered_after = |i: usize| -> Vec<&str> {
        let Some(next) = messages.get(i + 1) else {
            return Vec::new();
        };
        if next.role != Role::User {
            return Vec::new();
        }
        next.content
            .iter()
            .filter_map(|b| match b {
                ContentBlock::ToolResult { tool_use_id, .. } => Some(tool_use_id.as_str()),
                _ => None,
            })
            .collect()
    };
    // Replayable call ids declared by the assistant message at `i`.
    let declared_at = |i: usize| -> Vec<&str> {
        let Some(prev) = messages.get(i) else {
            return Vec::new();
        };
        if prev.role != Role::Assistant {
            return Vec::new();
        }
        prev.content
            .iter()
            .filter_map(|b| match b {
                ContentBlock::ToolUse { id, input, .. } if input.is_object() => Some(id.as_str()),
                _ => None,
            })
            .collect()
    };

    for (i, msg) in messages.iter().enumerate() {
        // A synthetic response belongs directly after its assistant message;
        // flush it before any message that is not the user turn it pairs with.
        if msg.role != Role::User && !pending_synth.is_empty() {
            result.append(&mut pending_synth);
        }
        let _role = msg.role.as_str();
        let content = &msg.content;
        let blocks: Vec<ContentBlock> = content.clone();

        match msg.role {
            Role::Assistant => {
                let mut text_parts: Vec<serde_json::Value> = Vec::new();
                let mut tool_calls: Vec<serde_json::Value> = Vec::new();
                let mut emitted_ids: Vec<String> = Vec::new();
                let mut reasoning = String::new();

                for b in &blocks {
                    match b {
                        ContentBlock::Thinking { thinking, .. } => reasoning.push_str(thinking),
                        ContentBlock::Text { text, .. } => {
                            text_parts.push(serde_json::json!({"type": "text", "text": text}));
                        }
                        ContentBlock::ToolUse { id, name, input } => {
                            // Non-object input: the stream was truncated
                            // mid-arguments and salvaged as a bare string. The
                            // call never ran, and emitting it makes strict
                            // gateways reject the whole request.
                            if !input.is_object() {
                                continue;
                            }
                            emitted_ids.push(id.clone());
                            tool_calls.push(serde_json::json!({
                                "id": id,
                                "type": "function",
                                "function": {
                                    "name": name,
                                    "arguments": serde_json::to_string(input).unwrap_or_default(),
                                }
                            }));
                        }
                        _ => {}
                    }
                }

                let mut m = serde_json::json!({"role": "assistant"});
                if !reasoning.is_empty() {
                    m["reasoning_content"] = serde_json::json!(reasoning);
                }
                if !text_parts.is_empty() {
                    m["content"] = serde_json::json!(text_parts);
                } else if tool_calls.is_empty() {
                    m["content"] = serde_json::json!(".");
                }
                if !tool_calls.is_empty() {
                    m["tool_calls"] = serde_json::json!(tool_calls);
                }
                result.push(m);

                // A call with no recorded result leaves the batch incomplete,
                // which strict gateways reject. Answer it synthetically so the
                // `tool_calls` block is self-contained.
                let answered = answered_after(i);
                for id in emitted_ids {
                    if !answered.contains(&id.as_str()) {
                        pending_synth.push(serde_json::json!({
                            "role": "tool",
                            "tool_call_id": id,
                            "content": "(no result recorded)",
                        }));
                    }
                }
            }
            Role::User => {
                let mut text_parts: Vec<serde_json::Value> = Vec::new();
                let mut tool_items: Vec<serde_json::Value> = Vec::new();

                // Replay a result only when its call is the immediately
                // preceding assistant's — an orphan `tool` message is exactly
                // what strict gateways reject.
                let declared = if i > 0 {
                    declared_at(i - 1)
                } else {
                    Vec::new()
                };

                for b in &blocks {
                    match b {
                        ContentBlock::ToolResult {
                            tool_use_id,
                            content,
                            ..
                        } => {
                            if !declared.contains(&tool_use_id.as_str()) {
                                continue;
                            }
                            let tr_content = match content {
                                ContentContainer::Text(t) => t.clone(),
                                ContentContainer::Blocks(bs) => bs
                                    .iter()
                                    .filter_map(|b| match b {
                                        ContentBlock::Text { text, .. } => Some(text.clone()),
                                        _ => None,
                                    })
                                    .collect::<Vec<_>>()
                                    .join("\n"),
                            };
                            tool_items.push(serde_json::json!({
                                "role": "tool",
                                "tool_call_id": tool_use_id,
                                "content": tr_content,
                            }));
                        }
                        ContentBlock::Text { text, .. } => {
                            text_parts.push(serde_json::json!({"type": "text", "text": text}));
                        }
                        ContentBlock::ImageUrl { url, .. } => {
                            text_parts.push(serde_json::json!({
                                "type": "image_url",
                                "image_url": {"url": url, "detail": "auto"}
                            }));
                        }
                        _ => {}
                    }
                }
                // Tool responses must directly follow the assistant
                // `tool_calls`; user text that shared the message goes after,
                // never between the calls and their responses.
                let mut items = std::mem::take(&mut pending_synth);
                items.extend(tool_items);
                result.extend(items);
                if !text_parts.is_empty() {
                    result.push(serde_json::json!({"role": "user", "content": text_parts}));
                }
            }
            _ => {
                result.push(serde_json::json!({
                    "role": msg.role.as_str(),
                    "content": msg.content,
                }));
            }
        }
    }
    // An assistant message that ended the history leaves its synthetic
    // responses unflushed — emit them so the batch is complete.
    result.append(&mut pending_synth);
    result
}

/// Add cache_control markers for Anthropic models via OAI-compatible relay.
/// Matches Python _stamp_oai_cache_markers
pub fn stamp_oai_cache_markers(messages: &mut [serde_json::Value], model: &str) {
    let ml = model.to_lowercase();
    if !ml.contains("claude") && !ml.contains("anthropic") {
        return;
    }
    let user_idxs: Vec<usize> = messages
        .iter()
        .enumerate()
        .filter(|(_, m)| m["role"] == "user")
        .map(|(i, _)| i)
        .collect();
    for idx in user_idxs.iter().rev().take(2) {
        let content = messages[*idx]["content"].clone();
        if let Some(text) = content.as_str() {
            messages[*idx]["content"] = serde_json::json!([{
                "type": "text",
                "text": text,
                "cache_control": {"type": "ephemeral"}
            }]);
        } else if let Some(arr) = content.as_array() {
            if arr.last().is_some() {
                let mut new_arr: Vec<serde_json::Value> = arr.clone();
                if let Some(last_obj) = new_arr.last_mut() {
                    last_obj["cache_control"] = serde_json::json!({"type": "ephemeral"});
                }
                messages[*idx]["content"] = serde_json::json!(new_arr);
            }
        }
    }
}

/// Fix messages for Claude API — ensure alternating roles, pair tool_use/tool_result.
/// Matches Python _fix_messages
pub fn fix_messages(messages: &[Message]) -> Vec<serde_json::Value> {
    if messages.is_empty() {
        return Vec::new();
    }

    let mut fixed: Vec<serde_json::Value> = Vec::new();

    for msg in messages {
        let role = msg.role.as_str();
        let content_val = blocks_to_json_value(msg.content.clone());

        if let Some(last) = fixed.last() {
            if last["role"] == role {
                // Merge consecutive same-role messages
                let merged_content = merge_content_blocks(last["content"].clone(), content_val);
                let mut merged = last.clone();
                merged["content"] = merged_content;
                fixed.pop();
                fixed.push(merged);
                continue;
            }

            if last["role"] == "assistant" && role == "user" {
                // Check for missing tool_result pairs
                let uses = extract_tool_use_ids(last);
                let has = extract_tool_result_ids(&content_val);
                let missing: Vec<&str> = uses
                    .iter()
                    .filter(|id| !has.contains(*id))
                    .map(|s| s.as_str())
                    .collect();
                let mut adjusted_content = content_val.clone();
                for uid in &missing {
                    let err_block = serde_json::json!([{
                        "type": "tool_result",
                        "tool_use_id": uid,
                        "content": "(error)"
                    }]);
                    if let Some(arr) = adjusted_content.as_array() {
                        let mut new_arr = arr.clone();
                        if let Some(err_arr) = err_block.as_array() {
                            new_arr.extend(err_arr.clone());
                        }
                        adjusted_content = serde_json::json!(new_arr);
                    }
                }
                fixed.push(serde_json::json!({"role": role, "content": adjusted_content}));
                continue;
            }
        }

        fixed.push(serde_json::json!({"role": role, "content": content_val}));
    }

    while fixed
        .first()
        .map(|m| m["role"].as_str() != Some("user"))
        .unwrap_or(false)
    {
        fixed.remove(0);
    }

    fixed
}

fn blocks_to_json_value(blocks: Vec<ContentBlock>) -> serde_json::Value {
    serde_json::to_value(blocks).unwrap_or_default()
}

fn merge_content_blocks(a: serde_json::Value, b: serde_json::Value) -> serde_json::Value {
    let mut result = Vec::new();
    if let Some(arr) = a.as_array() {
        result.extend(arr.iter().cloned());
    } else if let Some(text) = a.as_str() {
        result.push(serde_json::json!({"type": "text", "text": text}));
    }
    if let Some(arr) = b.as_array() {
        result.push(serde_json::json!({"type": "text", "text": "\n"}));
        result.extend(arr.iter().cloned());
    } else if let Some(text) = b.as_str() {
        result.push(serde_json::json!({"type": "text", "text": "\n"}));
        result.push(serde_json::json!({"type": "text", "text": text}));
    }
    serde_json::json!(result)
}

fn extract_tool_use_ids(msg: &serde_json::Value) -> Vec<String> {
    let mut ids = Vec::new();
    if let Some(content) = msg["content"].as_array() {
        for block in content {
            if block["type"] == "tool_use" {
                if let Some(id) = block["id"].as_str() {
                    ids.push(id.to_string());
                }
            }
        }
    }
    ids
}

fn extract_tool_result_ids(content: &serde_json::Value) -> Vec<String> {
    let mut ids = Vec::new();
    if let Some(arr) = content.as_array() {
        for block in arr {
            if block["type"] == "tool_result" {
                if let Some(id) = block["tool_use_id"].as_str() {
                    ids.push(id.to_string());
                }
            }
        }
    }
    ids
}

/// Drop unsigned thinking blocks — some models need this.
pub fn drop_unsigned_thinking(messages: &[serde_json::Value]) -> Vec<serde_json::Value> {
    messages
        .iter()
        .map(|m| {
            let mut m = m.clone();
            if let Some(content) = m["content"].as_array() {
                let filtered: Vec<serde_json::Value> = content
                    .iter()
                    .filter(|b| {
                        !(b["type"] == "thinking"
                            && b.get("signature")
                                .and_then(|s| s.as_str())
                                .unwrap_or("")
                                .is_empty())
                    })
                    .cloned()
                    .collect();
                m["content"] = serde_json::json!(filtered);
            }
            m
        })
        .collect()
}

/// DeepSeek needs thinking blocks in history.
pub fn ensure_thinking_blocks(
    messages: &[serde_json::Value],
    model: &str,
) -> Vec<serde_json::Value> {
    if !model.to_lowercase().contains("deepseek") {
        return messages.to_vec();
    }
    messages.iter().map(|m| {
        if m["role"] != "assistant" { return m.clone(); }
        let mut m = m.clone();
        if let Some(content) = m["content"].as_array() {
            let has_thinking = content.iter().any(|b| b["type"] == "thinking");
            if !has_thinking {
                let mut new_content = vec![
                    serde_json::json!({"type": "thinking", "thinking": "...", "signature": "placeholder"})
                ];
                new_content.extend(content.iter().cloned());
                m["content"] = serde_json::json!(new_content);
            }
        }
        m
    }).collect()
}

/// Convert OAI tool format to Claude tool format.
/// Matches Python openai_tools_to_claude
pub fn openai_tools_to_claude(tools: &[oz_core_types::ToolDefinition]) -> Vec<serde_json::Value> {
    tools
        .iter()
        .map(|t| {
            let fn_ = &t.function;
            serde_json::json!({
                "name": fn_.name,
                "description": fn_.description,
                "input_schema": fn_.parameters,
            })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    // ---- msgs_claude2oai ----

    #[test]
    fn test_claude2oai_empty() {
        let result = msgs_claude2oai(&[], "gpt-4");
        assert!(result.is_empty());
    }

    #[test]
    fn test_claude2oai_simple_user() {
        let msgs = vec![Message::user("hello")];
        let result = msgs_claude2oai(&msgs, "gpt-4");
        assert_eq!(result.len(), 1);
        assert_eq!(result[0]["role"], "user");
        assert!(result[0].get("content").is_some());
    }

    #[test]
    fn test_claude2oai_simple_assistant() {
        let msgs = vec![Message::assistant("world")];
        let result = msgs_claude2oai(&msgs, "gpt-4");
        assert_eq!(result.len(), 1);
        assert_eq!(result[0]["role"], "assistant");
    }

    #[test]
    fn test_claude2oai_assistant_with_thinking() {
        let msg = Message::assistant_with_blocks(vec![
            ContentBlock::Thinking {
                thinking: "let me think...".into(),
                signature: None,
            },
            ContentBlock::text("the answer is 42"),
        ]);
        let result = msgs_claude2oai(&[msg], "gpt-4");
        assert_eq!(result.len(), 1);
        assert_eq!(result[0]["reasoning_content"], "let me think...");
        assert!(result[0].get("content").is_some());
    }

    #[test]
    fn test_claude2oai_assistant_with_tool_use() {
        let msg = Message::assistant_with_blocks(vec![ContentBlock::tool_use(
            "tu_1",
            "read_file",
            serde_json::json!({"path": "/tmp/x.txt"}),
        )]);
        let result = msgs_claude2oai(&[msg], "gpt-4");
        // The call is kept and, with no result following, answered
        // synthetically so the batch is valid on its own.
        assert_eq!(result.len(), 2);
        assert!(result[0].get("tool_calls").is_some());
        assert_eq!(result[1]["role"], "tool");
        assert_eq!(result[1]["tool_call_id"], "tu_1");
    }

    #[test]
    fn test_claude2oai_non_object_tool_input_dropped_with_result() {
        // A stream truncated mid-arguments is salvaged as a bare string input.
        // The call cannot execute (its result is an error), and encoding it
        // makes strict gateways reject the request — or teaches the model the
        // malformed shape. Drop the call and its result together.
        let call_id = "tu_1";
        let assistant = Message::assistant_with_blocks(vec![ContentBlock::tool_use(
            call_id,
            "write",
            serde_json::json!(" truncated file body..."),
        )]);
        let user = Message::user_with_blocks(vec![ContentBlock::ToolResult {
            tool_use_id: call_id.into(),
            content: oz_core_types::ContentContainer::Text(
                "{\"error\":\"write: missing file_path\"}".into(),
            ),
            is_error: Some(true),
        }]);
        let result = msgs_claude2oai(&[assistant, user], "glm-5.3-flash");
        for m in &result {
            assert!(
                m.get("tool_calls").is_none(),
                "poisoned call must be dropped: {m}"
            );
            assert!(m["role"] != "tool", "poisoned result must be dropped: {m}");
        }
    }

    #[test]
    fn test_claude2oai_system_message() {
        let msgs = vec![Message::system("You are a helpful AI.")];
        let result = msgs_claude2oai(&msgs, "gpt-4");
        assert_eq!(result.len(), 1);
        assert_eq!(result[0]["role"], "system");
    }

    /// Assert the OpenAI tool protocol across a converted history: every
    /// `tool` message directly follows the assistant `tool_calls` declaring
    /// its id, every declared id is answered before the next non-tool
    /// message, and no `tool` message floats on its own.
    fn assert_tool_protocol(msgs: &[serde_json::Value]) {
        let mut i = 0;
        while i < msgs.len() {
            if msgs[i]["role"] != "assistant" {
                i += 1;
                continue;
            }
            let declared: Vec<&str> = msgs[i]["tool_calls"]
                .as_array()
                .map(|a| a.iter().filter_map(|c| c["id"].as_str()).collect())
                .unwrap_or_default();
            if declared.is_empty() {
                i += 1;
                continue;
            }
            let mut answered: Vec<&str> = Vec::new();
            let mut j = i + 1;
            while j < msgs.len() && msgs[j]["role"] == "tool" {
                let id = msgs[j]["tool_call_id"].as_str().unwrap_or("");
                assert!(
                    declared.contains(&id),
                    "tool message answers an id the preceding assistant did not declare: {id}"
                );
                answered.push(id);
                j += 1;
            }
            for id in &declared {
                assert!(answered.contains(id), "tool_calls id never answered: {id}");
            }
            i = j;
        }
        for (idx, m) in msgs.iter().enumerate() {
            if m["role"] == "tool" {
                let follows = idx > 0
                    && (msgs[idx - 1]["role"] == "assistant" || msgs[idx - 1]["role"] == "tool");
                assert!(follows, "orphan tool message at index {idx}");
            }
        }
    }

    #[test]
    fn test_claude2oai_tool_results_precede_accompanying_user_text() {
        // The failing opencode.ai shape: a user turn carries a batch of tool
        // results AND the user's next question (build_history merges them).
        // The text must not land between `tool_calls` and its `tool` responses.
        let assistant = Message::assistant_with_blocks(vec![ContentBlock::tool_use(
            "call_1",
            "read_file",
            serde_json::json!({"path": "/tmp/x"}),
        )]);
        let user = Message::user_with_blocks(vec![
            ContentBlock::tool_result("call_1", "file body"),
            ContentBlock::text("now do the next thing"),
        ]);
        let out = msgs_claude2oai(&[assistant, user], "deepseek-flash");
        assert_tool_protocol(&out);
        assert_eq!(out[0]["role"], "assistant");
        assert_eq!(out[1]["role"], "tool");
        assert_eq!(out[2]["role"], "user");
        assert_eq!(out[2]["content"][0]["text"], "now do the next thing");
    }

    #[test]
    fn test_claude2oai_unanswered_tool_call_gets_synthetic_response() {
        // A terminal control call (`respond`) is persisted as a tool_use with
        // no result. The batch must still be complete or strict gateways 400.
        let assistant = Message::assistant_with_blocks(vec![
            ContentBlock::tool_use("call_a", "read_file", serde_json::json!({})),
            ContentBlock::tool_use(
                "call_respond",
                "respond",
                serde_json::json!({"response": "done"}),
            ),
        ]);
        let user = Message::user_with_blocks(vec![ContentBlock::tool_result("call_a", "ok")]);
        let out = msgs_claude2oai(&[assistant, user], "deepseek-flash");
        assert_tool_protocol(&out);
        let answered: Vec<&str> = out
            .iter()
            .filter(|m| m["role"] == "tool")
            .filter_map(|m| m["tool_call_id"].as_str())
            .collect();
        assert!(
            answered.contains(&"call_respond"),
            "unpaired call must be answered: {answered:?}"
        );
    }

    #[test]
    fn test_claude2oai_orphan_tool_result_is_dropped() {
        // A result with no preceding assistant call would serialize as a bare
        // `tool` message — exactly the error the gateway reports.
        let user =
            Message::user_with_blocks(vec![ContentBlock::tool_result("call_ghost", "stale")]);
        let out = msgs_claude2oai(&[user], "deepseek-flash");
        assert!(
            out.iter().all(|m| m["role"] != "tool"),
            "orphan result must be dropped: {out:?}"
        );
    }

    // ---- msgs_oai2claude ----

    #[test]
    fn test_claude2oai_roundtrip_structure() {
        let msgs = vec![Message::user("hi"), Message::assistant("hello!")];
        let oai = msgs_claude2oai(&msgs, "gpt-4");
        assert_eq!(oai.len(), 2);
        assert_eq!(oai[0]["role"], "user");
        assert_eq!(oai[1]["role"], "assistant");
    }

    // ---- fix_messages ----

    #[test]
    fn test_fix_messages_empty() {
        let result = fix_messages(&[]);
        assert!(result.is_empty());
    }

    #[test]
    fn test_fix_messages_merges_consecutive() {
        let msgs = vec![Message::user("first"), Message::user("second")];
        let result = fix_messages(&msgs);
        assert_eq!(result.len(), 1);
    }

    #[test]
    fn test_fix_messages_alternating() {
        let msgs = vec![Message::user("hello"), Message::assistant("hi")];
        let result = fix_messages(&msgs);
        assert_eq!(result.len(), 2);
    }

    #[test]
    fn test_fix_messages_single_user() {
        let msgs = vec![Message::user("hello")];
        let result = fix_messages(&msgs);
        assert_eq!(result.len(), 1);
        assert_eq!(result[0]["role"], "user");
    }

    #[test]
    fn test_fix_removes_leading_non_user() {
        let msgs = vec![Message::system("system prompt"), Message::user("hello")];
        let result = fix_messages(&msgs);
        assert!(!result.is_empty());
        assert_eq!(result[0]["role"], "user");
    }

    // ---- drop_unsigned_thinking ----

    #[test]
    fn test_drop_unsigned_thinking() {
        let input = vec![serde_json::json!({
            "role": "assistant",
            "content": [
                {"type": "thinking", "thinking": "sig-free", "signature": ""},
                {"type": "text", "text": "hello"},
            ]
        })];
        let result = drop_unsigned_thinking(&input);
        assert_eq!(result.len(), 1);
        let blocks = result[0]["content"].as_array().unwrap();
        assert_eq!(blocks.len(), 1);
    }

    // ---- ensure_thinking_blocks ----

    #[test]
    fn test_ensure_thinking_blocks_non_deepseek() {
        let input = vec![
            serde_json::json!({"role": "assistant", "content": [{"type": "text", "text": "hi"}]}),
        ];
        let result = ensure_thinking_blocks(&input, "gpt-4");
        assert_eq!(result.len(), 1);
    }

    #[test]
    fn test_ensure_thinking_blocks_deepseek() {
        let input = vec![
            serde_json::json!({"role": "assistant", "content": [{"type": "text", "text": "hi"}]}),
        ];
        let result = ensure_thinking_blocks(&input, "deepseek-chat");
        assert_eq!(result.len(), 1);
        let blocks = result[0]["content"].as_array().unwrap();
        assert!(blocks.iter().any(|b| b["type"] == "thinking"));
    }
}
