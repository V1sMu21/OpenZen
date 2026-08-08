use std::sync::Mutex;

use oz_core_types::{ContentBlock, LlmError, Message, StreamEvent, TokenUsage, ToolDefinition};
use tokio::sync::mpsc::UnboundedSender;
use oz_config::{ApiMode, SessionConfig};

use crate::message_format::{msgs_claude2oai, stamp_oai_cache_markers};
use crate::retry::retry_with_backoff;
use crate::session::Session;
use crate::stream::parse_openai_sse;
use crate::is_local_apibase;

pub struct OaiSession {
    config: SessionConfig,
    pub history: Mutex<Vec<Message>>,
    pub system: Option<String>,
    pub tools: Option<Vec<ToolDefinition>>,
}

/// Total request timeout for streaming responses. reqwest's `.timeout()`
/// covers the whole body read, so a slow local stream that runs past it
/// gets cut off mid-response. Local quantized models need an hour; cloud
/// APIs keep the tight 10-minute cap.
fn http_timeout(apibase: &str) -> std::time::Duration {
    let secs = if is_local_apibase(apibase) { 3600 } else { 600 };
    std::time::Duration::from_secs(secs)
}

impl OaiSession {
    pub fn new(config: SessionConfig) -> Self {
        OaiSession { config, history: Mutex::new(Vec::new()), system: None, tools: None }
    }
}

#[async_trait::async_trait]
impl Session for OaiSession {
    fn config(&self) -> &SessionConfig { &self.config }
    fn history(&self) -> &Mutex<Vec<Message>> { &self.history }
    fn history_mut(&self) -> &Mutex<Vec<Message>> { &self.history }
    fn set_system(&mut self, system: String) { self.system = Some(system); }
    fn set_tools(&mut self, tools: Vec<ToolDefinition>) { self.tools = Some(tools); }

    async fn raw_ask(&self, messages: &[Message]) -> Result<(Vec<ContentBlock>, Option<TokenUsage>), LlmError> {
        let cfg = self.config.clone();
        let tools = self.tools.clone();
        let system = self.system.clone();
        let oai_msgs_base = msgs_claude2oai(messages, &cfg.model);

        let cfg_responses = cfg.clone();
        let cfg_for_responses = cfg_responses.clone();
        if cfg_responses.api_mode == ApiMode::Responses {
            let url = format!("{}/responses", cfg_responses.apibase.trim_end_matches('/'));
            retry_with_backoff(
                move || {
                    let oai_msgs = oai_msgs_base.clone();
                    let tools = tools.clone();
                    let cfg = cfg_for_responses.clone();
                    let url = url.clone();
                    Box::pin(async move {
                        let mut payload = serde_json::json!({
                            "model": cfg.model,
                            "input": oai_msgs,
                            "stream": true,
                        });
                        if let Some(ref tw) = tools {
                            payload["tools"] = serde_json::to_value(tw).unwrap_or_default();
                        }
                        let client = crate::build_http_client(&cfg.apibase, http_timeout(&cfg.apibase).as_secs());
                        let resp = client.post(&url)
                            .bearer_auth(&cfg.apikey)
                            .json(&payload)
                            .send().await
                            .map_err(|e| LlmError::RequestFailed(e))?;
                        let status = resp.status().as_u16();
                        if status >= 400 {
                            let body = resp.text().await.unwrap_or_default();
                            return Err(LlmError::HttpError { status, body });
                        }
                        parse_openai_sse(resp, "responses", None, None, &cfg.apibase).await
                    })
                },
                &cfg_responses,
            ).await
        } else {
            let url = format!("{}/chat/completions", cfg_responses.apibase.trim_end_matches('/'));
            let model_lower = cfg_responses.model.to_lowercase();
            let cfg_chat = cfg_responses.clone();
            let cfg_for_chat = cfg_chat.clone();

            retry_with_backoff(
                move || {
                    let mut oai_msgs = oai_msgs_base.clone();
                    let tools = tools.clone();
                    let cfg = cfg_for_chat.clone();
                    let url = url.clone();
                    let model_lower = model_lower.clone();
                    let system = system.clone();
                    Box::pin(async move {
                        if let Some(ref sys) = system {
                            oai_msgs.insert(0, serde_json::json!({"role": "system", "content": sys}));
                        }
                        stamp_oai_cache_markers(&mut oai_msgs, &cfg.model);
                        let mut payload = serde_json::json!({
                            "model": cfg.model,
                            "messages": oai_msgs,
                            "stream": true,
                            "stream_options": { "include_usage": true },
                        });
                        if let Some(temp) = cfg.temperature {
                            if (temp - 1.0).abs() > f64::EPSILON {
                                payload["temperature"] = serde_json::json!(temp);
                            }
                        }
                        if let Some(maxt) = cfg.max_tokens {
                            if model_lower.starts_with("gpt-5") || model_lower.starts_with("o1")
                                || model_lower.starts_with("o2") || model_lower.starts_with("o3")
                                || model_lower.starts_with("o4")
                            {
                                payload["max_completion_tokens"] = serde_json::json!(maxt);
                            } else {
                                payload["max_tokens"] = serde_json::json!(maxt);
                            }
                        }
                        if let Some(ref tw) = tools {
                            payload["tools"] = serde_json::to_value(tw).unwrap_or_default();
                            payload["tool_choice"] = serde_json::json!("required");
                        }
                        let client = crate::build_http_client(&cfg.apibase, http_timeout(&cfg.apibase).as_secs());
                        let resp = client.post(&url)
                            .bearer_auth(&cfg.apikey)
                            .json(&payload)
                            .send().await
                            .map_err(|e| LlmError::RequestFailed(e))?;
                        let status = resp.status().as_u16();
                        if status >= 400 {
                            let body = resp.text().await.unwrap_or_default();
                            return Err(LlmError::HttpError { status, body });
                        }
                        parse_openai_sse(resp, "chat_completions", None, None, &cfg.apibase).await
                    })
                },
                &cfg_chat,
            ).await
        }
    }

    async fn raw_ask_streaming(
        &self,
        messages: &[Message],
        event_tx: UnboundedSender<StreamEvent>,
        speculative_tx: Option<UnboundedSender<StreamEvent>>,
    ) -> Result<(Vec<ContentBlock>, Option<TokenUsage>), LlmError> {
        let cfg = self.config.clone();
        let tools = self.tools.clone();
        let system = self.system.clone();
        let oai_msgs_base = msgs_claude2oai(messages, &cfg.model);

        let cfg_responses = cfg.clone();
        let cfg_for_responses = cfg_responses.clone();
        if cfg_responses.api_mode == ApiMode::Responses {
            let url = format!("{}/responses", cfg_responses.apibase.trim_end_matches('/'));
            retry_with_backoff(
                move || {
                    let oai_msgs = oai_msgs_base.clone();
                    let tools = tools.clone();
                    let cfg = cfg_for_responses.clone();
                    let url = url.clone();
                    let event_tx = event_tx.clone();
                    let spec_tx = speculative_tx.clone();
                    Box::pin(async move {
                        let mut payload = serde_json::json!({
                            "model": cfg.model,
                            "input": oai_msgs,
                            "stream": true,
                        });
                        if let Some(ref tw) = tools {
                            payload["tools"] = serde_json::to_value(tw).unwrap_or_default();
                        }
                        let client = crate::build_http_client(&cfg.apibase, http_timeout(&cfg.apibase).as_secs());
                        // Send-phase timeout: headers may never arrive even
                        // after connect succeeds (wedged server). Without it,
                        // send() blocks for http_timeout (1h local) and the
                        // agent looks frozen — fail fast, retry instead.
                        let header_timeout = if is_local_apibase(&cfg.apibase) { 180 } else { 60 };
                        let resp = match tokio::time::timeout(
                            std::time::Duration::from_secs(header_timeout),
                            client.post(&url)
                                .bearer_auth(&cfg.apikey)
                                .json(&payload)
                                .send(),
                        ).await {
                            Ok(Ok(r)) => r,
                            Ok(Err(e)) => return Err(LlmError::RequestFailed(e)),
                            Err(_) => return Err(LlmError::StreamError(format!(
                                "no response headers within {header_timeout}s"
                            ))),
                        };
                        let status = resp.status().as_u16();
                        if status >= 400 {
                            let body = resp.text().await.unwrap_or_default();
                            return Err(LlmError::HttpError { status, body });
                        }
                        parse_openai_sse(resp, "responses", Some(event_tx), spec_tx, &cfg.apibase).await
                    })
                },
                &cfg_responses,
            ).await
        } else {
            let url = format!("{}/chat/completions", cfg_responses.apibase.trim_end_matches('/'));
            let model_lower = cfg_responses.model.to_lowercase();
            let cfg_chat = cfg_responses.clone();
            let cfg_for_chat = cfg_chat.clone();

            retry_with_backoff(
                move || {
                    let mut oai_msgs = oai_msgs_base.clone();
                    let tools = tools.clone();
                    let cfg = cfg_for_chat.clone();
                    let url = url.clone();
                    let model_lower = model_lower.clone();
                    let system = system.clone();
                    let event_tx = event_tx.clone();
                    let spec_tx = speculative_tx.clone();
                    Box::pin(async move {
                        if let Some(ref sys) = system {
                            oai_msgs.insert(0, serde_json::json!({"role": "system", "content": sys}));
                        }
                        stamp_oai_cache_markers(&mut oai_msgs, &cfg.model);
                        let mut payload = serde_json::json!({
                            "model": cfg.model,
                            "messages": oai_msgs,
                            "stream": true,
                            "stream_options": { "include_usage": true },
                        });
                        if let Some(temp) = cfg.temperature {
                            if (temp - 1.0).abs() > f64::EPSILON {
                                payload["temperature"] = serde_json::json!(temp);
                            }
                        }
                        if let Some(maxt) = cfg.max_tokens {
                            if model_lower.starts_with("gpt-5") || model_lower.starts_with("o1")
                                || model_lower.starts_with("o2") || model_lower.starts_with("o3")
                                || model_lower.starts_with("o4")
                            {
                                payload["max_completion_tokens"] = serde_json::json!(maxt);
                            } else {
                                payload["max_tokens"] = serde_json::json!(maxt);
                            }
                        }
                        if let Some(ref tw) = tools {
                            payload["tools"] = serde_json::to_value(tw).unwrap_or_default();
                            payload["tool_choice"] = serde_json::json!("required");
                        }
                        let client = crate::build_http_client(&cfg.apibase, http_timeout(&cfg.apibase).as_secs());
                        // Send-phase timeout: headers may never arrive even
                        // after connect succeeds (wedged server). Without it,
                        // send() blocks for http_timeout (1h local) and the
                        // agent looks frozen — fail fast, retry instead.
                        let header_timeout = if is_local_apibase(&cfg.apibase) { 180 } else { 60 };
                        let resp = match tokio::time::timeout(
                            std::time::Duration::from_secs(header_timeout),
                            client.post(&url)
                                .bearer_auth(&cfg.apikey)
                                .json(&payload)
                                .send(),
                        ).await {
                            Ok(Ok(r)) => r,
                            Ok(Err(e)) => return Err(LlmError::RequestFailed(e)),
                            Err(_) => return Err(LlmError::StreamError(format!(
                                "no response headers within {header_timeout}s"
                            ))),
                        };
                        let status = resp.status().as_u16();
                        if status >= 400 {
                            let body = resp.text().await.unwrap_or_default();
                            return Err(LlmError::HttpError { status, body });
                        }
                        parse_openai_sse(resp, "chat_completions", Some(event_tx), spec_tx, &cfg.apibase).await
                    })
                },
                &cfg_chat,
            ).await
        }
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
        msgs_claude2oai(messages, &self.config.model)
    }
}
