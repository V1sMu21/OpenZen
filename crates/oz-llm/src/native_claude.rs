use std::sync::Mutex;

use oz_config::SessionConfig;
use oz_core_types::{ContentBlock, LlmError, Message, StreamEvent, TokenUsage, ToolDefinition};
use tokio::sync::mpsc::UnboundedSender;

use crate::message_format::{
    drop_unsigned_thinking, ensure_thinking_blocks, fix_messages, openai_tools_to_claude,
};
use crate::retry::retry_with_backoff;
use crate::session::Session;
use crate::stream::parse_claude_sse;

pub struct NativeClaudeSession {
    config: SessionConfig,
    pub history: Mutex<Vec<Message>>,
    pub system: Option<String>,
    pub tools: Option<Vec<ToolDefinition>>,
    device_id: String,
    session_id: String,
}

impl NativeClaudeSession {
    pub fn new(config: SessionConfig) -> Self {
        let device_id = format!("{}{}", uuid::Uuid::new_v4(), uuid::Uuid::new_v4());
        NativeClaudeSession {
            config,
            history: Mutex::new(Vec::new()),
            system: None,
            tools: None,
            device_id: device_id[..64].to_string(),
            session_id: uuid::Uuid::new_v4().to_string(),
        }
    }
}

#[async_trait::async_trait]
impl Session for NativeClaudeSession {
    fn config(&self) -> &SessionConfig {
        &self.config
    }
    fn history(&self) -> &Mutex<Vec<Message>> {
        &self.history
    }
    fn history_mut(&self) -> &Mutex<Vec<Message>> {
        &self.history
    }
    fn set_system(&mut self, system: String) {
        self.system = Some(system);
    }
    fn set_tools(&mut self, tools: Vec<ToolDefinition>) {
        self.tools = Some(tools);
    }

    async fn raw_ask(
        &self,
        messages: &[Message],
    ) -> Result<(Vec<ContentBlock>, Option<TokenUsage>), LlmError> {
        let mut messages = fix_messages(messages);
        messages = drop_unsigned_thinking(&messages);
        messages = ensure_thinking_blocks(&messages, &self.config.model);

        let max_tokens = self.config.max_tokens.unwrap_or(8192);
        let url = format!(
            "{}/v1/messages?beta=true",
            self.config.apibase.trim_end_matches('/')
        );
        let model = self.config.model.clone();

        let beta_parts = [
            "claude-code-20250219",
            "interleaved-thinking-2025-05-14",
            "redact-thinking-2026-02-12",
            "prompt-caching-scope-2026-01-05",
        ];

        let cfg_clone = self.config.clone();
        let tools = self.tools.clone();
        let system = self.system.clone();
        let device_id = self.device_id.clone();
        let session_id = self.session_id.clone();
        let beta_header = beta_parts.join(",");

        retry_with_backoff(
            move || {
                let cfg = cfg_clone.clone();
                let tools = tools.clone();
                let system = system.clone();
                let device_id = device_id.clone();
                let session_id = session_id.clone();
                let beta_header = beta_header.clone();
                let url = url.clone();
                let model = model.clone();
                let messages = messages.clone();

                Box::pin(async move {
                    use reqwest::header::{HeaderMap, HeaderValue, CONTENT_TYPE};

                    let mut payload = serde_json::json!({
                        "model": model,
                        "messages": messages,
                        "max_tokens": max_tokens,
                        "stream": true,
                    });
                    if let Some(temp) = cfg.temperature {
                        if (temp - 1.0).abs() > f64::EPSILON {
                            payload["temperature"] = serde_json::json!(temp);
                        }
                    }
                    payload["metadata"] = serde_json::json!({
                        "user_id": format!(r#"{{"device_id":"{}","account_uuid":"{}","session_id":"{}"}}"#,
                            device_id, uuid::Uuid::new_v4(), session_id)
                    });

                    if let Some(ref tw) = tools {
                        let claude_tools = openai_tools_to_claude(tw);
                        let tools_val: Vec<serde_json::Value> = claude_tools.iter().enumerate().map(|(i, t)| {
                            let mut v = serde_json::to_value(t).unwrap_or_default();
                            if i == claude_tools.len() - 1 {
                                v["cache_control"] = serde_json::json!({"type": "ephemeral"});
                            }
                            v
                        }).collect();
                        payload["tools"] = serde_json::json!(tools_val);
                        payload["tool_choice"] = serde_json::json!({"type": "any"});
                    }

                    payload["system"] = serde_json::json!([
                        {"type": "text", "text": "You are Claude Code, Anthropic's official CLI for Claude.", "cache_control": {"type": "ephemeral"}}
                    ]);
                    if let Some(ref sys) = system {
                        payload["system"] = serde_json::json!([
                            {"type": "text", "text": sys}
                        ]);
                    }

                    let mut headers = HeaderMap::new();
                    headers.insert(CONTENT_TYPE, HeaderValue::from_static("application/json"));
                    headers.insert("anthropic-version", HeaderValue::from_static("2023-06-01"));
                    headers.insert("anthropic-beta", HeaderValue::from_str(&beta_header).unwrap());
                    headers.insert("anthropic-dangerous-direct-browser-access", HeaderValue::from_static("true"));
                    headers.insert("user-agent", HeaderValue::from_static("claude-cli/2.1.113 (external, cli)"));
                    if cfg.apikey.starts_with("sk-ant-") {
                        headers.insert("x-api-key", HeaderValue::from_str(&cfg.apikey).unwrap());
                    } else {
                        headers.insert("authorization", HeaderValue::from_str(&format!("Bearer {}", cfg.apikey)).unwrap());
                    }

                    let client = crate::build_http_client(&cfg.apibase, 600);
                    let resp = client.post(&url).headers(headers).json(&payload).send().await
                        .map_err(LlmError::RequestFailed)?;
                    let status = resp.status().as_u16();
                    if status >= 400 {
                        let body = resp.text().await.unwrap_or_default();
                        return Err(LlmError::HttpError { status, body });
                    }
                    parse_claude_sse(resp, None, &cfg.apibase).await
                })
            },
            &self.config,
        ).await
    }

    async fn raw_ask_streaming(
        &self,
        messages: &[Message],
        event_tx: UnboundedSender<StreamEvent>,
        _speculative_tx: Option<UnboundedSender<StreamEvent>>,
    ) -> Result<(Vec<ContentBlock>, Option<TokenUsage>), LlmError> {
        let mut messages = fix_messages(messages);
        messages = drop_unsigned_thinking(&messages);
        messages = ensure_thinking_blocks(&messages, &self.config.model);

        let max_tokens = self.config.max_tokens.unwrap_or(8192);
        let url = format!(
            "{}/v1/messages?beta=true",
            self.config.apibase.trim_end_matches('/')
        );
        let model = self.config.model.clone();

        let beta_parts = [
            "claude-code-20250219",
            "interleaved-thinking-2025-05-14",
            "redact-thinking-2026-02-12",
            "prompt-caching-scope-2026-01-05",
        ];

        let cfg_clone = self.config.clone();
        let tools = self.tools.clone();
        let system = self.system.clone();
        let device_id = self.device_id.clone();
        let session_id = self.session_id.clone();
        let beta_header = beta_parts.join(",");

        // Retry only the send/status phase — a mid-stream failure is NOT
        // re-sent here: the agent loop owns turn-level retry, and re-sending
        // would duplicate TextDelta events already rendered (P3/A3).
        let resp = retry_with_backoff(
            move || {
                let cfg = cfg_clone.clone();
                let tools = tools.clone();
                let system = system.clone();
                let device_id = device_id.clone();
                let session_id = session_id.clone();
                let beta_header = beta_header.clone();
                let url = url.clone();
                let model = model.clone();
                let messages = messages.clone();

                Box::pin(async move {
                    use reqwest::header::{HeaderMap, HeaderValue, CONTENT_TYPE};

                    let mut payload = serde_json::json!({
                        "model": model,
                        "messages": messages,
                        "max_tokens": max_tokens,
                        "stream": true,
                    });
                    if let Some(temp) = cfg.temperature {
                        if (temp - 1.0).abs() > f64::EPSILON {
                            payload["temperature"] = serde_json::json!(temp);
                        }
                    }
                    payload["metadata"] = serde_json::json!({
                        "user_id": format!(r#"{{"device_id":"{}","account_uuid":"{}","session_id":"{}"}}"#,
                            device_id, uuid::Uuid::new_v4(), session_id)
                    });

                    if let Some(ref tw) = tools {
                        let claude_tools = openai_tools_to_claude(tw);
                        let tools_val: Vec<serde_json::Value> = claude_tools.iter().enumerate().map(|(i, t)| {
                            let mut v = serde_json::to_value(t).unwrap_or_default();
                            if i == claude_tools.len() - 1 {
                                v["cache_control"] = serde_json::json!({"type": "ephemeral"});
                            }
                            v
                        }).collect();
                        payload["tools"] = serde_json::json!(tools_val);
                        payload["tool_choice"] = serde_json::json!({"type": "any"});
                    }

                    payload["system"] = serde_json::json!([
                        {"type": "text", "text": "You are Claude Code, Anthropic's official CLI for Claude.", "cache_control": {"type": "ephemeral"}}
                    ]);
                    if let Some(ref sys) = system {
                        payload["system"] = serde_json::json!([
                            {"type": "text", "text": sys}
                        ]);
                    }

                    let mut headers = HeaderMap::new();
                    headers.insert(CONTENT_TYPE, HeaderValue::from_static("application/json"));
                    headers.insert("anthropic-version", HeaderValue::from_static("2023-06-01"));
                    headers.insert("anthropic-beta", HeaderValue::from_str(&beta_header).unwrap());
                    headers.insert("anthropic-dangerous-direct-browser-access", HeaderValue::from_static("true"));
                    headers.insert("user-agent", HeaderValue::from_static("claude-cli/2.1.113 (external, cli)"));
                    if cfg.apikey.starts_with("sk-ant-") {
                        headers.insert("x-api-key", HeaderValue::from_str(&cfg.apikey).unwrap());
                    } else {
                        headers.insert("authorization", HeaderValue::from_str(&format!("Bearer {}", cfg.apikey)).unwrap());
                    }

                    let client = crate::build_http_client(&cfg.apibase, 600);
                    let resp = client.post(&url).headers(headers).json(&payload).send().await
                        .map_err(LlmError::RequestFailed)?;
                    let status = resp.status().as_u16();
                    if status >= 400 {
                        let body = resp.text().await.unwrap_or_default();
                        return Err(LlmError::HttpError { status, body });
                    }
                    Ok(resp)
                })
            },
            &self.config,
        ).await?;
        parse_claude_sse(resp, Some(event_tx), &self.config.apibase).await
    }

    async fn ask(&self, prompt: &str) -> Result<Vec<ContentBlock>, LlmError> {
        let raw_messages = {
            let mut history = self
                .history
                .lock()
                .map_err(|e| LlmError::Custom(e.to_string()))?;
            history.push(Message::user(prompt));
            if history.len() > 5 {
                crate::retry::trim_history(&mut history, self.config.context_win);
            }
            history
                .iter()
                .map(|m| Message {
                    role: m.role,
                    content: m.content.clone(),
                    tool_results: None,
                })
                .collect::<Vec<_>>()
        };
        let (blocks, _usage) = self.raw_ask(&raw_messages).await?;
        if !blocks.is_empty() {
            let has_error = blocks
                .first()
                .map(|b| match b {
                    ContentBlock::Text { text, .. } => text.starts_with("!!!Error:"),
                    _ => false,
                })
                .unwrap_or(false);
            if !has_error {
                let mut history = self
                    .history
                    .lock()
                    .map_err(|e| LlmError::Custom(e.to_string()))?;
                history.push(Message::assistant_with_blocks(blocks.clone()));
            }
        }
        Ok(blocks)
    }

    fn format_messages(&self, messages: &[Message]) -> Vec<serde_json::Value> {
        let mut result: Vec<serde_json::Value> = messages
            .iter()
            .map(|m| serde_json::json!({"role": m.role.as_str(), "content": m.content}))
            .collect();
        let user_idxs: Vec<usize> = result
            .iter()
            .enumerate()
            .filter(|(_, m)| m["role"] == "user")
            .map(|(i, _)| i)
            .collect();
        for idx in user_idxs.iter().rev().take(2) {
            if let Some(content) = result[*idx]["content"].as_array_mut() {
                if let Some(last) = content.last_mut() {
                    last["cache_control"] = serde_json::json!({"type": "ephemeral"});
                }
            }
        }
        result
    }
}
