use std::sync::Mutex;

use oz_core_types::{ContentBlock, LlmError, Message, TokenUsage, ToolDefinition};
use oz_config::SessionConfig;

use crate::session::Session;
use crate::stream::parse_claude_sse;
use crate::retry::retry_with_backoff;
use crate::message_format::fix_messages;

pub struct ClaudeSession {
    config: SessionConfig,
    pub history: Mutex<Vec<Message>>,
    pub system: Option<String>,
    pub tools: Option<Vec<ToolDefinition>>,
    http_client: reqwest::Client,
}

impl ClaudeSession {
    pub fn new(config: SessionConfig) -> Self {
        let http_client = crate::build_http_client(&config.apibase, config.timeout.unwrap_or(120));
        ClaudeSession {
            config,
            history: Mutex::new(Vec::new()),
            system: None,
            tools: None,
            http_client,
        }
    }

    fn build_headers(&self) -> reqwest::header::HeaderMap {
        use reqwest::header::{HeaderMap, HeaderValue, CONTENT_TYPE};
        let mut headers = HeaderMap::new();
        headers.insert(CONTENT_TYPE, HeaderValue::from_static("application/json"));
        headers.insert("x-api-key", HeaderValue::from_str(&self.config.apikey).unwrap());
        headers.insert("anthropic-version", HeaderValue::from_static("2023-06-01"));
        if let Some(val) = &self.config.reasoning_effort {
            headers.insert("anthropic-thinking", HeaderValue::from_str(val).unwrap());
        }
        headers
    }
}

#[async_trait::async_trait]
impl Session for ClaudeSession {
    fn config(&self) -> &SessionConfig { &self.config }
    fn history(&self) -> &Mutex<Vec<Message>> { &self.history }
    fn history_mut(&self) -> &Mutex<Vec<Message>> { &self.history }
    fn set_system(&mut self, system: String) { self.system = Some(system); }
    fn set_tools(&mut self, tools: Vec<ToolDefinition>) { self.tools = Some(tools); }

    async fn raw_ask(&self, messages: &[Message]) -> Result<(Vec<ContentBlock>, Option<TokenUsage>), LlmError> {
        let messages = fix_messages(messages);
        let max_tokens = self.config.max_tokens.unwrap_or(8192);
        let url = format!("{}/v1/messages", self.config.apibase.trim_end_matches('/'));

        let mut payload = serde_json::json!({
            "model": self.config.model,
            "messages": messages,
            "max_tokens": max_tokens,
            "stream": true,
        });
        if let Some(temp) = self.config.temperature {
            if (temp - 1.0).abs() > f64::EPSILON {
                payload["temperature"] = serde_json::json!(temp);
            }
        }
        if let Some(ref system) = self.system {
            payload["system"] = serde_json::json!([{
                "type": "text",
                "text": system,
                "cache_control": { "type": "persistent" }
            }]);
        }

        let headers = self.build_headers();
        let http_client = self.http_client.clone();
        let apibase = self.config.apibase.clone();

        retry_with_backoff(
            || {
                let payload = payload.clone();
                let headers = headers.clone();
                let http_client = http_client.clone();
                let url = url.clone();
                let apibase = apibase.clone();
                Box::pin(async move {
                    let resp = http_client
                        .post(&url)
                        .headers(headers)
                        .json(&payload)
                        .send()
                        .await
                        .map_err(LlmError::RequestFailed)?;

                    let status = resp.status().as_u16();
                    if status >= 400 {
                        let body = resp.text().await.unwrap_or_default();
                        return Err(LlmError::HttpError { status, body });
                    }

                    let (blocks, _usage) = parse_claude_sse(resp, None, &apibase).await?;
                    Ok((blocks, _usage))
                })
            },
            &self.config,
        ).await
    }

    async fn ask(&self, prompt: &str) -> Result<Vec<ContentBlock>, LlmError> {
        let raw_messages = {
            let mut history = self.history.lock().map_err(|e| LlmError::Custom(e.to_string()))?;
            history.push(Message::user(prompt));
            if history.len() > 5 {
                crate::retry::trim_history(&mut history, self.config.context_win);
            }
            history.iter().map(|m| Message {
                role: m.role,
                content: m.content.clone(),
                tool_results: None,
            }).collect::<Vec<_>>()
        };
        let (blocks, _usage) = self.raw_ask(&raw_messages).await?;
        if !blocks.is_empty() {
            let has_error = blocks.first().map(|b| match b {
                ContentBlock::Text { text, .. } => text.starts_with("!!!Error:"),
                _ => false,
            }).unwrap_or(false);
            if !has_error {
                let mut history = self.history.lock().map_err(|e| LlmError::Custom(e.to_string()))?;
                history.push(Message::assistant_with_blocks(blocks.clone()));
            }
        }
        Ok(blocks)
    }

    fn format_messages(&self, messages: &[Message]) -> Vec<serde_json::Value> {
        let mut result = Vec::new();
        for msg in messages {
            let role = msg.role.as_str();
            let mut blocks: Vec<serde_json::Value> = Vec::new();
            for block in &msg.content {
                match block {
                    ContentBlock::Text { text, cache_control } => {
                        let mut b = serde_json::json!({"type": "text", "text": text});
                        if let Some(_cc) = cache_control {
                            b["cache_control"] = serde_json::json!({"type": "ephemeral"});
                        }
                        blocks.push(b);
                    }
                    ContentBlock::ToolUse { id, name, input } => {
                        blocks.push(serde_json::json!({
                            "type": "tool_use",
                            "id": id,
                            "name": name,
                            "input": input,
                        }));
                    }
                    ContentBlock::ToolResult { tool_use_id, content, is_error } => {
                        let content_val = match content {
                            oz_core_types::ContentContainer::Text(t) => {
                                serde_json::json!([{"type": "text", "text": t}])
                            }
                            oz_core_types::ContentContainer::Blocks(bs) => {
                                serde_json::to_value(bs).unwrap_or_default()
                            }
                        };
                        let mut block = serde_json::json!({
                            "type": "tool_result",
                            "tool_use_id": tool_use_id,
                            "content": content_val,
                        });
                        if let Some(err) = is_error {
                            if *err {
                                block["is_error"] = serde_json::json!(true);
                            }
                        }
                        blocks.push(block);
                    }
                    ContentBlock::Thinking { thinking, .. } => {
                        blocks.push(serde_json::json!({
                            "type": "thinking",
                            "thinking": thinking,
                        }));
                    }
                    _ => {}
                }
            }
            result.push(serde_json::json!({
                "role": role,
                "content": blocks,
            }));
        }
        result
    }
}
