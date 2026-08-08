use std::collections::{HashMap, VecDeque};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use async_trait::async_trait;
use futures_util::{SinkExt, StreamExt};
use oz_core_types::StreamEvent;
use oz_platform::{
    AgentBridge, PlatformAdapter, PlatformConfig, PlatformContext,
    PlatformError, PlatformHealth,
};
use serde::{Deserialize, Serialize};
use tokio::sync::Mutex;
use tokio_tungstenite::connect_async;

const QQ_API_BASE: &str = "https://api.sgroup.qq.com";
const QQ_SANDBOX_API_BASE: &str = "https://sandbox.api.sgroup.qq.com";
const QQ_TOKEN_URL: &str = "https://bots.qq.com/app/getAppAccessToken";
const SPLIT_LIMIT: usize = 1500;
const MAX_PROCESSED_IDS: usize = 1000;

static MSG_SEQ: AtomicU64 = AtomicU64::new(1);

fn next_msg_seq() -> u64 {
    MSG_SEQ.fetch_add(1, Ordering::Relaxed)
}

#[derive(Debug, Deserialize)]
struct TokenResponse {
    access_token: Option<String>,
    expires_in: Option<i64>,
}

#[derive(Debug, Deserialize)]
struct GatewayResponse {
    url: Option<String>,
}

#[derive(Debug, Deserialize)]
struct WsPayload {
    op: i64,
    d: Option<serde_json::Value>,
    s: Option<i64>,
    t: Option<String>,
}

#[derive(Debug, Deserialize)]
struct WsHello {
    heartbeat_interval: Option<i64>,
}

#[derive(Debug, Deserialize)]
struct C2cEvent {
    id: Option<String>,
    content: Option<String>,
    author: Option<QqAuthor>,
}

#[derive(Debug, Deserialize)]
struct GroupEvent {
    id: Option<String>,
    content: Option<String>,
    author: Option<QqAuthor>,
    group_openid: Option<String>,
}

#[derive(Debug, Deserialize)]
struct QqAuthor {
    id: Option<String>,
    user_openid: Option<String>,
    member_openid: Option<String>,
}

pub struct QQAdapter {
    app_id: String,
    app_secret: String,
    allowed_users: Option<Vec<String>>,
    default_model: Option<String>,
    sandbox: bool,
}

impl QQAdapter {
    pub fn new(config: &PlatformConfig) -> Result<Self, PlatformError> {
        let app_id = config
            .qq_app_id()
            .ok_or_else(|| PlatformError::Config("qq.app_id is required".into()))?
            .to_string();
        let app_secret = config
            .qq_app_secret()
            .ok_or_else(|| PlatformError::Config("qq.app_secret is required".into()))?
            .to_string();
        let sandbox = config.extra.get("sandbox").and_then(|v| v.as_bool()).unwrap_or(true);
        Ok(QQAdapter {
            app_id,
            app_secret,
            allowed_users: config.allowed_users.clone(),
            default_model: config.default_model.clone(),
            sandbox,
        })
    }

    fn api_base(&self) -> &str {
        if self.sandbox { QQ_SANDBOX_API_BASE } else { QQ_API_BASE }
    }

    async fn get_access_token(&self) -> Result<String, String> {
        let client = reqwest::Client::new();
        let resp = client
            .post(QQ_TOKEN_URL)
            .json(&serde_json::json!({
                "appId": self.app_id,
                "clientSecret": self.app_secret,
            }))
            .send()
            .await
            .map_err(|e| format!("token request error: {e}"))?;

        let status = resp.status();
        let body_text = resp.text().await
            .map_err(|e| format!("token read error: {e}"))?;
        tracing::info!("[qq] token response status={}, body={}", status, body_text);

        let body: TokenResponse = serde_json::from_str(&body_text)
            .map_err(|e| format!("token parse error: {e} (body={body_text})"))?;

        body.access_token
            .ok_or_else(|| format!("no access_token in response (body={body_text})"))
    }

    async fn get_gateway_url(&self, token: &str) -> Result<String, String> {
        let client = reqwest::Client::new();
        let resp = client
            .get(format!("{}/gateway", self.api_base()))
            .header("Authorization", format!("QQBot {token}"))
            .send()
            .await
            .map_err(|e| format!("gateway request error: {e}"))?;

        let body: GatewayResponse = resp
            .json()
            .await
            .map_err(|e| format!("gateway parse error: {e}"))?;

        body.url
            .ok_or_else(|| "no gateway url in response".into())
    }

    async fn send_message(
        &self,
        token: &str,
        chat_id: &str,
        content: &str,
        is_group: bool,
        msg_id: Option<&str>,
    ) -> Result<(), String> {
        let client = reqwest::Client::new();
        let key = if is_group {
            "group_openid"
        } else {
            "openid"
        };
        let endpoint = if is_group {
            format!("{}/v2/groups/{chat_id}/messages", self.api_base())
        } else {
            format!("{}/v2/users/{chat_id}/messages", self.api_base())
        };

        let mut body = serde_json::json!({
            "msg_type": 0,
            "content": content,
            "msg_seq": next_msg_seq(),
        });
        if let Some(mid) = msg_id {
            body["msg_id"] = serde_json::Value::String(mid.to_string());
        }

        let resp = client
            .post(&endpoint)
            .header("Authorization", format!("QQBot {token}"))
            .json(&body)
            .send()
            .await
            .map_err(|e| format!("send message error: {e}"))?;

        if !resp.status().is_success() {
            let status = resp.status();
            let text = resp.text().await.unwrap_or_default();
            return Err(format!("send message HTTP {status}: {text}"));
        }
        Ok(())
    }

    async fn send_text_parts(
        &self,
        token: &str,
        chat_id: &str,
        text: &str,
        is_group: bool,
        msg_id: Option<&str>,
    ) -> Result<(), String> {
        let parts = oz_platform::split_text(text, SPLIT_LIMIT);
        for part in parts {
            self.send_message(token, chat_id, &part, is_group, msg_id)
                .await?;
        }
        Ok(())
    }
}

#[async_trait]
impl PlatformAdapter for QQAdapter {
    fn id(&self) -> &'static str {
        "qq"
    }

    fn name(&self) -> &'static str {
        "QQ"
    }

    async fn start(&self, ctx: PlatformContext) -> Result<(), PlatformError> {
        let agent = ctx.agent.clone();
        let allowed = self.allowed_users.clone();
        let default_model = self.default_model.clone();
        let app_id = self.app_id.clone();
        let app_secret = self.app_secret.clone();
        let sandbox = self.sandbox;

        tracing::info!("[qq] bot starting (sandbox={})...", sandbox);

        let mut delay: u64 = 5;
        let max_delay: u64 = 300;

        loop {
            let started_at = std::time::Instant::now();
            match Self::run_gateway_loop(
                &app_id,
                &app_secret,
                &agent,
                &allowed,
                &default_model,
                &ctx.working_dir,
                sandbox,
            )
            .await
            {
                Ok(()) => {
                    tracing::info!("[qq] gateway closed normally, reconnecting...");
                }
                Err(e) => {
                    tracing::warn!("[qq] gateway error: {e}, reconnecting in {delay}s...");
                }
            }

            if started_at.elapsed().as_secs() >= 60 {
                delay = 5;
            }
            tokio::time::sleep(std::time::Duration::from_secs(delay)).await;
            delay = std::cmp::min(delay * 2, max_delay);
        }
    }

    async fn stop(&self) -> Result<(), PlatformError> {
        tracing::info!("[qq] stopping");
        Ok(())
    }

    async fn health(&self) -> PlatformHealth {
        PlatformHealth::healthy()
    }
}

impl QQAdapter {
    async fn run_gateway_loop(
        app_id: &str,
        app_secret: &str,
        agent: &Arc<AgentBridge>,
        allowed: &Option<Vec<String>>,
        default_model: &Option<String>,
        working_dir: &std::path::Path,
        sandbox: bool,
    ) -> Result<(), String> {
        let adapter = QQAdapter {
            app_id: app_id.to_string(),
            app_secret: app_secret.to_string(),
            allowed_users: allowed.clone(),
            default_model: default_model.clone(),
            sandbox,
        };

        let token = adapter.get_access_token().await?;
        let ws_url = adapter.get_gateway_url(&token).await?;

        let (ws_stream, _) = connect_async(&ws_url)
            .await
            .map_err(|e| format!("WebSocket connect error: {e}"))?;

        let (write, mut read) = ws_stream.split();
        let write = Arc::new(Mutex::new(write));
        let mut heartbeat_interval: u64 = 30;
        let mut processed_ids: VecDeque<String> = VecDeque::new();
        let counter_path = working_dir.join("openzen").join("qq_counters.json");
        let dedup_path = working_dir.join("openzen").join("qq_seen_msg_ids.json");
        let mut processed_ids: VecDeque<String> =
            oz_platform::load_seen_msg_ids(&dedup_path);
        let mut new_session_counter: HashMap<String, u32> =
            oz_platform::load_platform_counters(&counter_path);

        // Identify payload
        {
            let identify = serde_json::json!({
                "op": 2,
                "d": {
                    "token": format!("QQBot {token}"),
                    "intents": 1 | 512,  // C2C + GROUP_AT
                    "shard": [0, 1],
                    "properties": {}
                }
            });
            let msg = tokio_tungstenite::tungstenite::Message::Text(identify.to_string());
            write.lock().await.send(msg).await
                .map_err(|e| format!("ws identify send error: {e}"))?;
        }

        // Heartbeat task
        let write_heartbeat = write.clone();
        let heartbeat_handle = tokio::spawn(async move {
            loop {
                tokio::time::sleep(std::time::Duration::from_secs(heartbeat_interval)).await;
                let hb = serde_json::json!({ "op": 1, "d": {} });
                let msg = tokio_tungstenite::tungstenite::Message::Text(hb.to_string());
                if write_heartbeat.lock().await.send(msg).await.is_err() {
                    break;
                }
            }
        });

        // Read loop
        while let Some(msg) = read.next().await {
            let msg = msg.map_err(|e| format!("ws read error: {e}"))?;
            let text = msg.to_text().map_err(|e| format!("ws text error: {e}"))?;

            let payload: WsPayload = serde_json::from_str(text)
                .map_err(|e| format!("ws json error: {e}"))?;

            match payload.op {
                10 => {
                    if let Some(d) = payload.d {
                        if let Ok(hello) = serde_json::from_value::<WsHello>(d) {
                            if let Some(interval) = hello.heartbeat_interval {
                                heartbeat_interval = (interval as u64).max(1);
                            }
                        }
                    }
                }
                0 => {
                    let event_type = payload.t.as_deref().unwrap_or("");
                    let data = payload.d.unwrap_or_default();

                    match event_type {
                        "C2C_MESSAGE_CREATE" => {
                            if let Ok(evt) = serde_json::from_value::<C2cEvent>(data) {
                                Self::handle_message(
                                    agent,
                                    allowed,
                                    default_model,
                                    &adapter,
                                    &token,
                                    &evt.content.unwrap_or_default(),
                                    &evt.author
                                        .as_ref()
                                        .and_then(|a| a.user_openid.as_deref())
                                        .unwrap_or(""),
                                    &evt.author
                                        .as_ref()
                                        .and_then(|a| a.user_openid.as_deref())
                                        .unwrap_or(""),
                                    evt.id.as_deref(),
                                    &mut processed_ids,
                                &mut new_session_counter,
                                &counter_path,
                                &dedup_path,
                                false,
                                )
                                .await;
                            }
                        }
                        "GROUP_AT_MESSAGE_CREATE" => {
                            if let Ok(evt) = serde_json::from_value::<GroupEvent>(data) {
                                let group_id = evt.group_openid.unwrap_or_default();
                                Self::handle_message(
                                    agent,
                                    allowed,
                                    default_model,
                                    &adapter,
                                    &token,
                                    &evt.content.unwrap_or_default(),
                                    &evt.author
                                        .as_ref()
                                        .and_then(|a| a.member_openid.as_deref())
                                        .unwrap_or(""),
                                    &group_id,
                                    evt.id.as_deref(),
                                    &mut processed_ids,
                                &mut new_session_counter,
                                &counter_path,
                                &dedup_path,
                                true,
                                )
                                .await;
                            }
                        }
                        _ => {}
                    }
                }
                _ => {}
            }
        }

        heartbeat_handle.abort();
        Ok(())
    }

    async fn handle_message(
        agent: &Arc<AgentBridge>,
        allowed: &Option<Vec<String>>,
        default_model: &Option<String>,
        adapter: &QQAdapter,
        token: &str,
        content: &str,
        user_id: &str,
        chat_id: &str,
        msg_id: Option<&str>,
        processed_ids: &mut VecDeque<String>,
        new_session_counter: &mut HashMap<String, u32>,
        counter_path: &std::path::Path,
        dedup_path: &std::path::Path,
        is_group: bool,
    ) {
        if content.is_empty() {
            return;
        }

        if let Some(mid) = msg_id {
            if processed_ids.contains(&mid.to_string()) {
                return;
            }
            processed_ids.push_back(mid.to_string());
            if processed_ids.len() > MAX_PROCESSED_IDS {
                processed_ids.pop_front();
            }
            oz_platform::save_seen_msg_ids(dedup_path, processed_ids);
        }

        let is_public = allowed.is_none()
            || allowed.as_ref().map(|v| v.is_empty() || v.contains(&"*".to_string()))
                .unwrap_or(true);
        if !is_public
            && !allowed
                .as_ref()
                .map(|v| v.contains(&user_id.to_string()))
                .unwrap_or(false)
        {
            tracing::warn!("[qq] unauthorized user: {user_id}");
            return;
        }

        tracing::info!(
            "[qq] message from {user_id} ({}): {content}",
            if is_group { "group" } else { "c2c" }
        );

        if content.starts_with('/') {
            Self::handle_command(agent, adapter, token, chat_id, content, is_group, msg_id, new_session_counter).await;
            oz_platform::save_platform_counters(counter_path, new_session_counter);
            return;
        }

        let counter = new_session_counter.entry(chat_id.to_string()).or_insert(1);
        let session_id = if *counter > 1 {
            format!("qq:{chat_id}:{counter}")
        } else {
            format!("qq:{chat_id}")
        };
        let prompt = content.to_string();

        match agent
            .send_message(&session_id, &prompt, "qq", default_model.as_deref())
            .await
        {
            Ok(event_rx) => {
                let _ = adapter
                    .send_text_parts(token, chat_id, "思考中...", is_group, msg_id)
                    .await;
                Self::stream_response(adapter, token, chat_id, is_group, msg_id, event_rx).await;
            }
            Err(e) => {
                let _ = adapter
                    .send_text_parts(token, chat_id, &format!("❌ {e}"), is_group, msg_id)
                    .await;
            }
        }
    }

    async fn handle_command(
        agent: &Arc<AgentBridge>,
        adapter: &QQAdapter,
        token: &str,
        chat_id: &str,
        command: &str,
        is_group: bool,
        msg_id: Option<&str>,
        new_session_counter: &mut HashMap<String, u32>,
    ) {
        let parts: Vec<&str> = command.split_whitespace().collect();
        let op = parts.first().map(|s| s.to_lowercase()).unwrap_or_default();

        match op.as_str() {
            "/new" => {
                let c = new_session_counter.entry(chat_id.to_string()).or_insert(1);
                *c += 1;
                let sid = if *c > 1 {
                    format!("qq:{chat_id}:{c}")
                } else {
                    format!("qq:{chat_id}")
                };
                let reply = format!("✅ 新对话已开启（会话: {sid}）");
                let _ = adapter
                    .send_text_parts(token, chat_id, &reply, is_group, msg_id)
                    .await;
            }
            "/help" => {
                let reply = "📖 命令列表:\n/help - 帮助\n/stop - 停止\n/new - 新对话\n/status - 状态";
                let _ = adapter.send_text_parts(token, chat_id, reply, is_group, msg_id).await;
            }
            "/stop" => {
                let c = new_session_counter.entry(chat_id.to_string()).or_insert(1);
                let sid = if *c > 1 { format!("qq:{chat_id}:{c}") } else { format!("qq:{chat_id}") };
                agent.stop_session(&sid);
                let _ = adapter.send_text_parts(token, chat_id, "⏹️ 正在停止...", is_group, msg_id).await;
            }
            "/status" => {
                let _ = adapter.send_text_parts(token, chat_id, "🟢 OpenZen 运行中（通过平台适配器）", is_group, msg_id).await;
            }
            _ => {}
        }
    }

    async fn stream_response(
        adapter: &QQAdapter,
        token: &str,
        chat_id: &str,
        is_group: bool,
        msg_id: Option<&str>,
        mut event_rx: tokio::sync::mpsc::UnboundedReceiver<StreamEvent>,
    ) {
        let mut buffer = String::new();
        let mut last_send = std::time::Instant::now();
        let mut send_count: u32 = 0;

        while let Some(event) = event_rx.recv().await {
            match event {
                StreamEvent::TextDelta { text, .. } => {
                    buffer.push_str(&text);
                }
                StreamEvent::FinishMessage { .. } => {
                    let cleaned = oz_platform::clean_agent_output(&buffer);
                    let display = if cleaned.is_empty() {
                        "（无文本输出）".to_string()
                    } else {
                        cleaned
                    };
                    let _ = adapter
                        .send_text_parts(token, chat_id, &display, is_group, msg_id)
                        .await;

                    let files = oz_platform::extract_files(&buffer);
                    for file_path in &files {
                        let _ = adapter
                            .send_text_parts(
                                token,
                                chat_id,
                                &format!("📎 生成文件: {file_path}"),
                                is_group,
                                msg_id,
                            )
                            .await;
                    }
                    return;
                }
                StreamEvent::Error { message } => {
                    let _ = adapter
                        .send_text_parts(token, chat_id, &format!("❌ {message}"), is_group, msg_id)
                        .await;
                    return;
                }
                StreamEvent::ToolInputAvailable { name, args, .. } => {
                    let summary = format!(
                        "🔧 调用工具: {name}\n参数: {}",
                        &args[..std::cmp::min(args.len(), 200)]
                    );
                    let cleaned = oz_platform::clean_agent_output(&buffer);
                    let display = if cleaned.is_empty() {
                        summary
                    } else {
                        format!("{}\n\n{}", summary, &cleaned[..std::cmp::min(cleaned.len(), 1000)])
                    };
                    let _ = adapter
                        .send_text_parts(token, chat_id, &display, is_group, msg_id)
                        .await;
                    buffer.clear();
                    send_count += 1;
                    last_send = std::time::Instant::now();
                }
                _ => {}
            }

            if send_count >= 9 {
                break;
            }
            let now = std::time::Instant::now();
            if send_count > 0 && now.duration_since(last_send).as_secs() < 6 * send_count as u64 {
                continue;
            }
        }
    }
}
