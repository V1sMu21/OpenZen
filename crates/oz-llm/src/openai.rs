use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use oz_config::{ApiMode, SessionConfig};
use oz_core_types::{ContentBlock, LlmError, Message, StreamEvent, TokenUsage, ToolDefinition};
use tokio::sync::mpsc::UnboundedSender;

use crate::is_local_apibase;
use crate::message_format::{msgs_claude2oai, stamp_oai_cache_markers};
use crate::retry::retry_with_backoff;
use crate::session::Session;
use crate::stream::parse_openai_sse;

pub struct OaiSession {
    config: SessionConfig,
    pub history: Mutex<Vec<Message>>,
    pub system: Option<String>,
    pub tools: Option<Vec<ToolDefinition>>,
    /// One client per session, reused across calls and retries — the
    /// previous code rebuilt the client (and its connection pool) for
    /// every request, defeating keep-alive (P3/A3).
    http_client: reqwest::Client,
    /// Set once an endpoint rejects a forced `tool_choice: "required"`.
    /// opencode.ai's Go gateway runs reasoning models in thinking mode,
    /// which only accepts `auto`; after the first rejection the session
    /// stops probing and sends `auto` directly.
    tool_choice_relaxed: Arc<AtomicBool>,
}

/// Value to send for `tool_choice` when tools are present. Forcing
/// `"required"` makes the agent loop act on every turn, but a thinking-mode
/// model rejects it outright, so a session that has already hit that
/// rejection falls back to `"auto"`.
fn tool_choice_value(relaxed: &AtomicBool) -> serde_json::Value {
    if relaxed.load(Ordering::Relaxed) {
        serde_json::json!("auto")
    } else {
        serde_json::json!("required")
    }
}

/// Detect the thinking-mode rejection of a forced tool choice
/// (`400 ... Thinking mode does not support this tool_choice`). When it
/// matches, mark the session relaxed and rewrite the payload to `"auto"`
/// so the caller can resend; returns whether that happened.
fn relax_tool_choice(
    status: u16,
    body: &str,
    payload: &mut serde_json::Value,
    relaxed: &AtomicBool,
) -> bool {
    if status == 400
        && payload.get("tool_choice").is_some()
        && (body.contains("tool_choice") || body.contains("tool choice"))
    {
        relaxed.store(true, Ordering::Relaxed);
        payload["tool_choice"] = serde_json::json!("auto");
        true
    } else {
        false
    }
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
        let http_client =
            crate::build_session_http_client(&config, http_timeout(&config.apibase).as_secs());
        OaiSession {
            config,
            history: Mutex::new(Vec::new()),
            system: None,
            tools: None,
            http_client,
            tool_choice_relaxed: Arc::new(AtomicBool::new(false)),
        }
    }
}

#[async_trait::async_trait]
impl Session for OaiSession {
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
        let cfg = self.config.clone();
        let tools = self.tools.clone();
        let system = self.system.clone();
        let oai_msgs_base = msgs_claude2oai(messages, &cfg.model);

        let cfg_responses = cfg.clone();
        let cfg_for_responses = cfg_responses.clone();
        if cfg_responses.api_mode == ApiMode::Responses {
            let url = format!("{}/responses", cfg_responses.apibase.trim_end_matches('/'));
            let http_client = self.http_client.clone();
            retry_with_backoff(
                move || {
                    let oai_msgs = oai_msgs_base.clone();
                    let tools = tools.clone();
                    let cfg = cfg_for_responses.clone();
                    let url = url.clone();
                    let http_client = http_client.clone();
                    Box::pin(async move {
                        let mut payload = serde_json::json!({
                            "model": cfg.model,
                            "input": oai_msgs,
                            "stream": true,
                        });
                        if let Some(ref tw) = tools {
                            payload["tools"] = serde_json::to_value(tw).unwrap_or_default();
                        }
                        let resp = http_client
                            .post(&url)
                            .bearer_auth(&cfg.apikey)
                            .json(&payload)
                            .send()
                            .await
                            .map_err(LlmError::RequestFailed)?;
                        let status = resp.status().as_u16();
                        if status >= 400 {
                            let body = resp.text().await.unwrap_or_default();
                            return Err(LlmError::HttpError { status, body });
                        }
                        parse_openai_sse(resp, "responses", None, None, &cfg.apibase).await
                    })
                },
                &cfg_responses,
            )
            .await
        } else {
            let url = format!(
                "{}/chat/completions",
                cfg_responses.apibase.trim_end_matches('/')
            );
            let model_lower = cfg_responses.model.to_lowercase();
            let cfg_chat = cfg_responses.clone();
            let cfg_for_chat = cfg_chat.clone();
            let http_client = self.http_client.clone();
            let tool_choice_relaxed = Arc::clone(&self.tool_choice_relaxed);

            retry_with_backoff(
                move || {
                    let mut oai_msgs = oai_msgs_base.clone();
                    let tools = tools.clone();
                    let cfg = cfg_for_chat.clone();
                    let url = url.clone();
                    let model_lower = model_lower.clone();
                    let system = system.clone();
                    let http_client = http_client.clone();
                    let tool_choice_relaxed = Arc::clone(&tool_choice_relaxed);
                    Box::pin(async move {
                        if let Some(ref sys) = system {
                            oai_msgs
                                .insert(0, serde_json::json!({"role": "system", "content": sys}));
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
                            if model_lower.starts_with("gpt-5")
                                || model_lower.starts_with("o1")
                                || model_lower.starts_with("o2")
                                || model_lower.starts_with("o3")
                                || model_lower.starts_with("o4")
                            {
                                payload["max_completion_tokens"] = serde_json::json!(maxt);
                            } else {
                                payload["max_tokens"] = serde_json::json!(maxt);
                            }
                        }
                        if let Some(ref tw) = tools {
                            payload["tools"] = serde_json::to_value(tw).unwrap_or_default();
                            payload["tool_choice"] = tool_choice_value(&tool_choice_relaxed);
                        }
                        let mut resp = http_client
                            .post(&url)
                            .bearer_auth(&cfg.apikey)
                            .json(&payload)
                            .send()
                            .await
                            .map_err(LlmError::RequestFailed)?;
                        let mut status = resp.status().as_u16();
                        if status >= 400 {
                            let body = resp.text().await.unwrap_or_default();
                            // Thinking-mode endpoints reject a forced tool
                            // choice; relax to "auto" and resend once.
                            if relax_tool_choice(status, &body, &mut payload, &tool_choice_relaxed)
                            {
                                resp = http_client
                                    .post(&url)
                                    .bearer_auth(&cfg.apikey)
                                    .json(&payload)
                                    .send()
                                    .await
                                    .map_err(LlmError::RequestFailed)?;
                                status = resp.status().as_u16();
                                if status >= 400 {
                                    let body = resp.text().await.unwrap_or_default();
                                    return Err(LlmError::HttpError { status, body });
                                }
                                return parse_openai_sse(
                                    resp,
                                    "chat_completions",
                                    None,
                                    None,
                                    &cfg.apibase,
                                )
                                .await;
                            }
                            return Err(LlmError::HttpError { status, body });
                        }
                        parse_openai_sse(resp, "chat_completions", None, None, &cfg.apibase).await
                    })
                },
                &cfg_chat,
            )
            .await
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
            let http_client = self.http_client.clone();
            // Retry only the send/status phase — a mid-stream failure is NOT
            // re-sent here: the agent loop owns turn-level retry, and
            // re-sending would duplicate TextDelta events already rendered.
            let resp = retry_with_backoff(
                move || {
                    let oai_msgs = oai_msgs_base.clone();
                    let tools = tools.clone();
                    let cfg = cfg_for_responses.clone();
                    let url = url.clone();
                    let http_client = http_client.clone();
                    Box::pin(async move {
                        let mut payload = serde_json::json!({
                            "model": cfg.model,
                            "input": oai_msgs,
                            "stream": true,
                        });
                        if let Some(ref tw) = tools {
                            payload["tools"] = serde_json::to_value(tw).unwrap_or_default();
                        }
                        // Send-phase timeout: headers may never arrive even
                        // after connect succeeds (wedged server). Without it,
                        // send() blocks for http_timeout (1h local) and the
                        // agent looks frozen — fail fast, retry instead.
                        let header_timeout = if is_local_apibase(&cfg.apibase) {
                            180
                        } else {
                            60
                        };
                        let resp = match tokio::time::timeout(
                            std::time::Duration::from_secs(header_timeout),
                            http_client
                                .post(&url)
                                .bearer_auth(&cfg.apikey)
                                .json(&payload)
                                .send(),
                        )
                        .await
                        {
                            Ok(Ok(r)) => r,
                            Ok(Err(e)) => return Err(LlmError::RequestFailed(e)),
                            Err(_) => {
                                return Err(LlmError::StreamError(format!(
                                    "no response headers within {header_timeout}s"
                                )))
                            }
                        };
                        let status = resp.status().as_u16();
                        if status >= 400 {
                            let body = resp.text().await.unwrap_or_default();
                            return Err(LlmError::HttpError { status, body });
                        }
                        Ok(resp)
                    })
                },
                &cfg_responses,
            )
            .await?;
            parse_openai_sse(
                resp,
                "responses",
                Some(event_tx),
                speculative_tx,
                &cfg_responses.apibase,
            )
            .await
        } else {
            let url = format!(
                "{}/chat/completions",
                cfg_responses.apibase.trim_end_matches('/')
            );
            let model_lower = cfg_responses.model.to_lowercase();
            let cfg_chat = cfg_responses.clone();
            let cfg_for_chat = cfg_chat.clone();
            let http_client = self.http_client.clone();
            let tool_choice_relaxed = Arc::clone(&self.tool_choice_relaxed);

            // Retry only the send/status phase — mid-stream failures are
            // surfaced to the agent loop, not re-sent (see responses branch).
            let resp = retry_with_backoff(
                move || {
                    let mut oai_msgs = oai_msgs_base.clone();
                    let tools = tools.clone();
                    let cfg = cfg_for_chat.clone();
                    let url = url.clone();
                    let model_lower = model_lower.clone();
                    let system = system.clone();
                    let http_client = http_client.clone();
                    let tool_choice_relaxed = Arc::clone(&tool_choice_relaxed);
                    Box::pin(async move {
                        if let Some(ref sys) = system {
                            oai_msgs
                                .insert(0, serde_json::json!({"role": "system", "content": sys}));
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
                            if model_lower.starts_with("gpt-5")
                                || model_lower.starts_with("o1")
                                || model_lower.starts_with("o2")
                                || model_lower.starts_with("o3")
                                || model_lower.starts_with("o4")
                            {
                                payload["max_completion_tokens"] = serde_json::json!(maxt);
                            } else {
                                payload["max_tokens"] = serde_json::json!(maxt);
                            }
                        }
                        if let Some(ref tw) = tools {
                            payload["tools"] = serde_json::to_value(tw).unwrap_or_default();
                            payload["tool_choice"] = tool_choice_value(&tool_choice_relaxed);
                        }
                        // Send-phase timeout (see responses branch).
                        let header_timeout = if is_local_apibase(&cfg.apibase) {
                            180
                        } else {
                            60
                        };
                        let mut resp = match tokio::time::timeout(
                            std::time::Duration::from_secs(header_timeout),
                            http_client
                                .post(&url)
                                .bearer_auth(&cfg.apikey)
                                .json(&payload)
                                .send(),
                        )
                        .await
                        {
                            Ok(Ok(r)) => r,
                            Ok(Err(e)) => return Err(LlmError::RequestFailed(e)),
                            Err(_) => {
                                return Err(LlmError::StreamError(format!(
                                    "no response headers within {header_timeout}s"
                                )))
                            }
                        };
                        let mut status = resp.status().as_u16();
                        if status >= 400 {
                            let body = resp.text().await.unwrap_or_default();
                            // Thinking-mode endpoints reject a forced tool
                            // choice; relax to "auto" and resend once.
                            if relax_tool_choice(status, &body, &mut payload, &tool_choice_relaxed)
                            {
                                resp = match tokio::time::timeout(
                                    std::time::Duration::from_secs(header_timeout),
                                    http_client
                                        .post(&url)
                                        .bearer_auth(&cfg.apikey)
                                        .json(&payload)
                                        .send(),
                                )
                                .await
                                {
                                    Ok(Ok(r)) => r,
                                    Ok(Err(e)) => return Err(LlmError::RequestFailed(e)),
                                    Err(_) => {
                                        return Err(LlmError::StreamError(format!(
                                            "no response headers within {header_timeout}s"
                                        )))
                                    }
                                };
                                status = resp.status().as_u16();
                                if status >= 400 {
                                    let body = resp.text().await.unwrap_or_default();
                                    return Err(LlmError::HttpError { status, body });
                                }
                                return Ok(resp);
                            }
                            return Err(LlmError::HttpError { status, body });
                        }
                        Ok(resp)
                    })
                },
                &cfg_chat,
            )
            .await?;
            parse_openai_sse(
                resp,
                "chat_completions",
                Some(event_tx),
                speculative_tx,
                &cfg_chat.apibase,
            )
            .await
        }
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
        msgs_claude2oai(messages, &self.config.model)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tool_choice_is_required_until_relaxed() {
        let relaxed = AtomicBool::new(false);
        assert_eq!(tool_choice_value(&relaxed), serde_json::json!("required"));
        relaxed.store(true, Ordering::Relaxed);
        assert_eq!(tool_choice_value(&relaxed), serde_json::json!("auto"));
    }

    #[test]
    fn thinking_mode_rejection_relaxes_to_auto() {
        let relaxed = AtomicBool::new(false);
        let mut payload = serde_json::json!({"tool_choice": "required"});
        let body = r#"{"error":{"message":"Error from provider (Console Go): Upstream request failed: [invalid_request_error] Thinking mode does not support this tool_choice"}}"#;
        assert!(relax_tool_choice(400, body, &mut payload, &relaxed));
        assert_eq!(payload["tool_choice"], serde_json::json!("auto"));
        assert!(relaxed.load(Ordering::Relaxed), "session must stay relaxed");
    }

    #[test]
    fn unrelated_400_keeps_forced_tool_choice() {
        let relaxed = AtomicBool::new(false);
        let mut payload = serde_json::json!({"tool_choice": "required"});
        assert!(!relax_tool_choice(
            400,
            "invalid_request_error: bad model",
            &mut payload,
            &relaxed
        ));
        assert_eq!(payload["tool_choice"], serde_json::json!("required"));
        assert!(!relaxed.load(Ordering::Relaxed));
    }

    #[test]
    fn rejection_without_tool_choice_field_is_not_relaxed() {
        // e.g. a 400 about tool *arguments* on a request that never forced a
        // choice — nothing to relax, so the error must surface unchanged.
        let relaxed = AtomicBool::new(false);
        let mut payload = serde_json::json!({"messages": []});
        assert!(!relax_tool_choice(
            400,
            "tool_choice is not supported",
            &mut payload,
            &relaxed
        ));
        assert!(!relaxed.load(Ordering::Relaxed));
    }
}
