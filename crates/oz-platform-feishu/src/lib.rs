mod card;
mod client;
mod frame;
mod media;

use std::sync::Arc;

use async_trait::async_trait;
use oz_core_types::StreamEvent;
use oz_platform::{
    AgentBridge, PlatformAdapter, PlatformConfig, PlatformContext,
    PlatformError, PlatformHealth,
};
use tokio::sync::Mutex;

use crate::card::TaskCard;
use crate::client::FeishuClient;

pub struct FeishuAdapter {
    instance_id: String,
    app_id: String,
    app_secret: String,
    allowed_users: Option<Vec<String>>,
    default_model: Option<String>,
    running_tasks: Arc<Mutex<std::collections::HashMap<String, bool>>>,
}

impl FeishuAdapter {
    pub fn new(config: &PlatformConfig) -> Result<Self, PlatformError> {
        let app_id = config
            .feishu_app_id()
            .ok_or_else(|| PlatformError::Config("feishu.app_id is required".into()))?
            .to_string();
        let app_secret = config
            .feishu_app_secret()
            .ok_or_else(|| PlatformError::Config("feishu.app_secret is required".into()))?
            .to_string();
        let instance_id = uuid::Uuid::new_v4().to_string();

        Ok(FeishuAdapter {
            instance_id,
            app_id,
            app_secret,
            allowed_users: config.allowed_users.clone(),
            default_model: config.default_model.clone(),
            running_tasks: Arc::new(Mutex::new(std::collections::HashMap::new())),
        })
    }
}

#[async_trait]
impl PlatformAdapter for FeishuAdapter {
    fn id(&self) -> &'static str {
        "feishu"
    }

    fn name(&self) -> &'static str {
        "飞书"
    }

    async fn start(&self, ctx: PlatformContext) -> Result<(), PlatformError> {
        let client = FeishuClient::new(self.app_id.clone(), self.app_secret.clone());
        let agent = ctx.agent.clone();
        let allowed = self.allowed_users.clone();
        let default_model = self.default_model.clone();
        let running_tasks = self.running_tasks.clone();

        let instance_id = &self.instance_id;
        eprintln!("[feishu:{instance_id}] starting bot... app_id={}", self.app_id);
        tracing::info!("[feishu:{instance_id}] starting bot... app_id={}", self.app_id);

        loop {
            eprintln!("[feishu:{instance_id}] connecting to WebSocket...");
            match Self::connect_websocket(
                &agent,
                &client,
                &allowed,
                &default_model,
                &running_tasks,
                instance_id,
                &ctx.working_dir,
            )
            .await
            {
                Ok(()) => {
                    eprintln!("[feishu:{instance_id}] WebSocket closed normally, reconnecting in 5s...");
                    tokio::time::sleep(std::time::Duration::from_secs(5)).await;
                }
                Err(e) => {
                    eprintln!("[feishu:{instance_id}] WebSocket error: {e}, reconnecting in 10s...");
                    tokio::time::sleep(std::time::Duration::from_secs(10)).await;
                }
            }
        }
    }

    async fn stop(&self) -> Result<(), PlatformError> {
        tracing::info!("[feishu] stopping");
        Ok(())
    }

    async fn health(&self) -> PlatformHealth {
        PlatformHealth::healthy()
    }
}

impl FeishuAdapter {
    async fn connect_websocket(
        agent: &Arc<AgentBridge>,
        client: &FeishuClient,
        allowed: &Option<Vec<String>>,
        default_model: &Option<String>,
        running_tasks: &Arc<Mutex<std::collections::HashMap<String, bool>>>,
        instance_id: &str,
        working_dir: &std::path::Path,
    ) -> Result<(), String> {
        use futures_util::StreamExt;
        use tokio_tungstenite::connect_async;

        eprintln!("[feishu:{instance_id}] discovering WebSocket endpoint...");
        let endpoint = client.get_ws_endpoint().await.map_err(|e| format!("get ws endpoint: {e}"))?;
        let ws_url = endpoint.url.ok_or_else(|| "no ws url in endpoint response".to_string())?;
        eprintln!("[feishu:{instance_id}] connecting to WebSocket: {ws_url}");

        let bot_open_id = client.get_bot_open_id().await.unwrap_or_default();
        eprintln!("[feishu:{instance_id}] bot open_id: {bot_open_id}");

        let (ws_stream, _) = connect_async(&ws_url)
            .await
            .map_err(|e| format!("WebSocket connect error: {e}"))?;

        let (_, mut read) = ws_stream.split();
        eprintln!("[feishu:{instance_id}] WebSocket connected, waiting for events...");

        // Persistent dedup: survive restarts so old messages replayed
        // by the Feishu server on reconnect don't trigger agent runs.
        let dedup_path = working_dir.join("openzen").join("feishu_seen_msg_ids.json");
        let mut seen_msg_ids: std::collections::VecDeque<String> =
            oz_platform::load_seen_msg_ids(&dedup_path);

        // Per-chat /new counters: persisted so restart doesn't lose session state.
        let counter_path = working_dir.join("openzen").join("feishu_counters.json");
        let mut new_session_counter: std::collections::HashMap<String, u32> =
            oz_platform::load_platform_counters(&counter_path);

        while let Some(msg) = read.next().await {
            let msg = msg.map_err(|e| format!("WebSocket read error: {e}"))?;
            if msg.is_ping() || msg.is_pong() {
                continue;
            }
            if msg.is_close() {
                eprintln!("[feishu:{instance_id}] WebSocket closed by server");
                return Ok(());
            }

            let (event_type, event_json) = if msg.is_binary() {
                let frame = crate::frame::decode_frame(msg.into_data().as_slice())
                    .map_err(|e| format!("protobuf decode error: {e}"))?;
                let payload_bytes = frame.payload.as_deref().unwrap_or(&[]);
                let event: serde_json::Value = serde_json::from_slice(payload_bytes)
                    .unwrap_or(serde_json::Value::Null);
                let hdr_type = frame.headers.get("type").map(|s| s.as_str()).unwrap_or("");
                let payload_type = frame.payload_type.as_str();
                if hdr_type.to_lowercase() != "event" {
                    eprintln!("[feishu:{instance_id}] non-event frame: type={hdr_type}, headers={:?}, payload_type={payload_type}", frame.headers);
                    continue;
                }
                let event_type = payload_type.to_string();
                let event_key = if event_type.is_empty() {
                    event.get("header").and_then(|h| h.get("event_type"))
                        .and_then(|v| v.as_str()).unwrap_or("").to_string()
                } else {
                    event_type
                };
                eprintln!("[feishu:{instance_id}] decoded event: type={event_key}, payload_len={}", payload_bytes.len());
                (event_key, event)
            } else if msg.is_text() {
                let text = msg.to_text().map_err(|e| format!("WebSocket text error: {e}"))?;
                eprintln!("[feishu:{instance_id}] received text event: {}", &text[..text.len().min(200)]);
                let event: serde_json::Value = serde_json::from_str(text)
                    .map_err(|e| format!("WebSocket JSON error: {e}"))?;
                let event_type = event
                    .get("header").and_then(|h| h.get("event_type"))
                    .and_then(|v| v.as_str()).unwrap_or("").to_string();
                (event_type, event)
            } else {
                continue;
            };

            // Deduplicate: skip if we've already processed this event.
            // Feishu may deliver the same message multiple times via WebSocket.
            let msg_id = event_json.get("event")
                .and_then(|e| e.get("message"))
                .and_then(|m| m.get("message_id"))
                .and_then(|v| v.as_str())
                .unwrap_or("");
            if !msg_id.is_empty() {
                if seen_msg_ids.contains(&msg_id.to_string()) {
                    eprintln!("[feishu:{instance_id}] skipping duplicate event: msg_id={msg_id}");
                    continue;
                }
                seen_msg_ids.push_back(msg_id.to_string());
                if seen_msg_ids.len() > 64 {
                    seen_msg_ids.pop_front();
                }
                oz_platform::save_seen_msg_ids(&dedup_path, &seen_msg_ids);
            }

            let event_type_str = event_type.as_str();

            if event_type_str.contains("message.receive") {
                let sender_id = if event_json.is_null() { "" } else {
                    event_json.get("event")
                        .and_then(|e| e.get("sender"))
                        .and_then(|s| s.get("sender_id"))
                        .and_then(|s| s.get("open_id"))
                        .and_then(|v| v.as_str()).unwrap_or("")
                };
                let sender_type = if event_json.is_null() { "" } else {
                    event_json.get("event")
                        .and_then(|e| e.get("sender"))
                        .and_then(|s| s.get("sender_type"))
                        .and_then(|v| v.as_str()).unwrap_or("")
                };

                if sender_type.is_empty() || sender_type != "user" {
                    if !sender_type.is_empty() {
                        eprintln!("[feishu:{instance_id}] skip non-user message: sender_type={sender_type}, sender_id={sender_id}");
                        continue;
                    }
                }

                if !bot_open_id.is_empty() && sender_id == bot_open_id {
                    continue;
                }

                if let Some(ref allowed) = allowed {
                    if !allowed.is_empty()
                        && !allowed.contains(&"*".to_string())
                        && !allowed.contains(&sender_id.to_string())
                    {
                        continue;
                    }
                }

                let message = if event_json.is_null() { None } else { event_json.get("event").and_then(|e| e.get("message")) };
                if let Some(message) = message {
                    let msg_type = message
                        .get("message_type")
                        .and_then(|v| v.as_str())
                        .unwrap_or("");
                    let chat_id = message
                        .get("chat_id")
                        .and_then(|v| v.as_str())
                        .unwrap_or(sender_id);
                    let content = message.get("content").and_then(|v| v.as_str()).unwrap_or("");

                    if msg_type == "text" {
                        let text = serde_json::from_str::<serde_json::Value>(content)
                            .ok()
                            .and_then(|c| c.get("text").and_then(|t| t.as_str()).map(|s| s.to_string()))
                            .unwrap_or_default();

                        let receive_id = message
                            .get("chat_id")
                            .and_then(|v| v.as_str())
                            .unwrap_or(sender_id);
                        let rid_type = if message.get("chat_id").is_some() {
                            "chat_id"
                        } else {
                            "open_id"
                        };

                        // Each chat_id gets its own OpenZen session.
                        // /new increments a per-chat counter to start a fresh session.
                        let base_sid = format!("feishu:{chat_id}");
                        let sid: Option<String> = if text.starts_with('/') {
                            let counter = new_session_counter
                                .entry(chat_id.to_string())
                                .or_insert(1);
                            let cmd_sid = if *counter > 1 {
                                format!("{base_sid}:{counter}")
                            } else {
                                base_sid.clone()
                            };
                            let result = handle_feishu_command(
                                agent, client, chat_id, sender_id, &text, &cmd_sid, &mut *counter, &base_sid,
                            )
                            .await;
                            oz_platform::save_platform_counters(&counter_path, &new_session_counter);
                            result
                        } else {
                            // Regular message: use the current session for this chat.
                            let counter = new_session_counter.entry(chat_id.to_string()).or_insert(1);
                            if *counter > 1 {
                                Some(format!("{base_sid}:{counter}"))
                            } else {
                                Some(base_sid.clone())
                            }
                        };
                        // If command handler returned None, skip agent start.
                        let sid = match sid {
                            Some(s) => s,
                            None => continue,
                        };

                        let prompt = text.to_string();
                        let model = default_model.as_deref();

                        eprintln!("[feishu:{instance_id}] starting agent for session={sid}, text_len={}", text.len());

                        {
                            let mut tasks = running_tasks.lock().await;
                            tasks.insert(sender_id.to_string(), true);
                        }

                        match agent.send_message(&sid, &prompt, "feishu", model).await {
                            Ok(event_rx) => {
                                stream_to_feishu_card(
                                    agent, client, event_rx,
                                    receive_id, rid_type, sender_id,
                                    running_tasks,
                                )
                                .await;
                            }
                            Err(e) => {
                                let _ = client
                                    .send_text(receive_id, &format!("❌ {e}"), rid_type)
                                    .await;
                            }
                        }

                        {
                            let mut tasks = running_tasks.lock().await;
                            tasks.remove(sender_id);
                        }
                    }
                }
            }
        }

        Ok(())
    }
}

async fn stream_to_feishu_card(
    _agent: &Arc<AgentBridge>,
    client: &FeishuClient,
    mut event_rx: tokio::sync::mpsc::UnboundedReceiver<StreamEvent>,
    receive_id: &str,
    rid_type: &str,
    sender_id: &str,
    running_tasks: &Arc<Mutex<std::collections::HashMap<String, bool>>>,
) {
    let mut card = TaskCard::new(receive_id.to_string(), rid_type.to_string());
    if let Err(e) = card.start(client).await {
        tracing::warn!("[feishu] card start error: {e}");
        return;
    }

    let mut buffer = String::new();

    while let Some(event) = event_rx.recv().await {
        eprintln!("[feishu:card] event: {:?}", std::mem::discriminant(&event));
        match event {
            StreamEvent::TextDelta { text, .. } => {
                buffer.push_str(&text);
            }
            StreamEvent::ToolInputAvailable { name, args, .. } => {
                if name == "respond" {
                    if let Some(resp) = serde_json::from_str::<serde_json::Value>(&args)
                        .ok()
                        .and_then(|v| v.get("response").and_then(|v| v.as_str()).map(|s| s.to_string()))
                    {
                        buffer = resp;
                    }
                }
                let summary = format!("调用工具: {name}");
                let detail = format!("**工具**: `{name}`\n**参数**:\n```json\n{args}\n```");
                if let Err(e) = card.step(client, &summary, &detail).await {
                    tracing::warn!("[feishu] card step error: {e}");
                }
            }
            StreamEvent::FinishMessage { .. } => {
                let cleaned = oz_platform::clean_agent_output(&buffer);
                let display = if cleaned.is_empty() {
                    "_(无文本输出)_".into()
                } else {
                    cleaned
                };
                let _ = card.done(client, &display).await;

                let files = oz_platform::extract_files(&buffer);
                for file_path in &files {
                    let _ = media::send_local_file(client, receive_id, file_path, rid_type).await;
                }
                return;
            }
            StreamEvent::Error { message } => {
                let _ = card.fail(client, &message).await;
                return;
            }
            _ => {}
        }

        {
            let tasks = running_tasks.lock().await;
            if !tasks.get(sender_id).copied().unwrap_or(true) {
                let _ = card.fail(client, "已停止").await;
                return;
            }
        }
    }

    // Safety fallback: if the channel closed without a FinishMessage event
    // (e.g. a bug or unexpected exit path), finalize the card with whatever
    // text was accumulated so the user doesn't see "thinking" forever.
    let cleaned = oz_platform::clean_agent_output(&buffer);
    let display = if cleaned.is_empty() {
        "_(无文本输出)_".into()
    } else {
        cleaned
    };
    let _ = card.done(client, &display).await;
}

/// Handle a slash-command. Returns the session_id to use for this message,
/// or None if the command was fully handled and no agent should be started.
async fn handle_feishu_command(
    agent: &Arc<AgentBridge>,
    client: &FeishuClient,
    chat_id: &str,
    _sender_id: &str,
    command: &str,
    current_sid: &str,
    counter: &mut u32,
    base_sid: &str,
) -> Option<String> {
    let parts: Vec<&str> = command.split_whitespace().collect();
    let op = parts.first().map(|s| s.to_lowercase()).unwrap_or_default();
    let receive_id_type = if chat_id.is_empty() { "open_id" } else { "chat_id" };

    match op.as_str() {
        "/help" => {
            let help = "命令列表:\n/stop - 停止\n/status - 状态\n/new - 新对话\n/help - 帮助";
            let _ = client.send_text(chat_id, help, receive_id_type).await;
            None
        }
        "/stop" => {
            agent.stop_session(current_sid);
            let _ = client.send_text(chat_id, "⏹️ 正在停止...", receive_id_type).await;
            None
        }
        "/status" => {
            let _ = client.send_text(chat_id, "🟢 OpenZen 运行中", receive_id_type).await;
            None
        }
        "/new" => {
            *counter += 1;
            let new_sid = if *counter > 1 {
                format!("{base_sid}:{counter}")
            } else {
                base_sid.to_string()
            };
            let _ = client.send_text(
                chat_id,
                &format!("✅ 新对话已开启（会话: {new_sid}）"),
                receive_id_type,
            ).await;
            // Don't start agent — just tell user the new session is ready.
            None
        }
        _ => {
            // Unknown command → treat as message for the agent to handle.
            Some(current_sid.to_string())
        }
    }
}

fn base64_url_encode(input: &str) -> String {
    use base64::Engine;
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(input.as_bytes())
}
