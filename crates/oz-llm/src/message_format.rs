use oz_core_types::{ContentBlock, ContentContainer, Message, Role};

/// Convert Claude content-block format messages to OpenAI format.
/// Matches Python _msgs_claude2oai
pub fn msgs_claude2oai(messages: &[Message], _model: &str) -> Vec<serde_json::Value> {
    let mut result: Vec<serde_json::Value> = Vec::new();
    for msg in messages {
        let _role = msg.role.as_str();
        let content = &msg.content;
        let blocks: Vec<ContentBlock> = content.clone();

        match msg.role {
            Role::Assistant => {
                let mut text_parts: Vec<serde_json::Value> = Vec::new();
                let mut tool_calls: Vec<serde_json::Value> = Vec::new();
                let mut reasoning = String::new();

                for b in &blocks {
                    match b {
                        ContentBlock::Thinking { thinking, .. } => reasoning.push_str(thinking),
                        ContentBlock::Text { text, .. } => {
                            text_parts.push(serde_json::json!({"type": "text", "text": text}));
                        }
                        ContentBlock::ToolUse { id, name, input } => {
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
            }
            Role::User => {
                let mut text_parts: Vec<serde_json::Value> = Vec::new();
                let mut tool_items: Vec<serde_json::Value> = Vec::new();

                for b in &blocks {
                    match b {
                        ContentBlock::ToolResult {
                            tool_use_id,
                            content,
                            ..
                        } => {
                            if !text_parts.is_empty() {
                                result.push(
                                    serde_json::json!({"role": "user", "content": text_parts}),
                                );
                                text_parts = Vec::new();
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
                        ContentBlock::ImageUrl {
                            url, media_type: _, ..
                        } => {
                            text_parts.push(serde_json::json!({
                                "type": "image_url",
                                "image_url": {"url": url, "detail": "auto"}
                            }));
                        }
                        _ => {}
                    }
                }
                if !text_parts.is_empty() {
                    result.push(serde_json::json!({"role": "user", "content": text_parts}));
                }
                result.extend(tool_items);
            }
            _ => {
                result.push(serde_json::json!({
                    "role": msg.role.as_str(),
                    "content": msg.content,
                }));
            }
        }
    }
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
        assert_eq!(result.len(), 1);
        assert!(result[0].get("tool_calls").is_some());
    }

    #[test]
    fn test_claude2oai_system_message() {
        let msgs = vec![Message::system("You are a helpful AI.")];
        let result = msgs_claude2oai(&msgs, "gpt-4");
        assert_eq!(result.len(), 1);
        assert_eq!(result[0]["role"], "system");
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
