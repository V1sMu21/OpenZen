mod client;
mod crypto;

    use std::collections::{HashMap, HashSet};

use async_trait::async_trait;
use oz_core_types::StreamEvent;
use oz_platform::{
    PlatformAdapter, PlatformConfig, PlatformContext,
    PlatformError, PlatformHealth,
};

use crate::client::WxBotClient;

pub struct WechatAdapter {
    default_model: Option<String>,
}

impl WechatAdapter {
    pub fn new(config: &PlatformConfig) -> Self {
        WechatAdapter {
            default_model: config.default_model.clone(),
        }
    }
}

#[async_trait]
impl PlatformAdapter for WechatAdapter {
    fn id(&self) -> &'static str {
        "wechat"
    }

    fn name(&self) -> &'static str {
        "WeChat"
    }

    async fn start(&self, ctx: PlatformContext) -> Result<(), PlatformError> {
        let agent = ctx.agent.clone();
        let default_model = self.default_model.clone();

        let mut bot = WxBotClient::new();
        if !bot.is_logged_in() {
            println!("[WeChat] Not logged in. Starting QR code login...");
            bot.qr_login()
                .await
                .map_err(|e| PlatformError::Connection(format!("QR login failed: {e}")))?;
        }

        tracing::info!(
            "[wechat] bot starting... bot_id={}",
            bot.bot_id.as_deref().unwrap_or("?")
        );

        let counter_path = ctx.working_dir.join("openzen").join("wechat_counters.json");
        let mut seen_ids: HashSet<String> = HashSet::new();
        let mut new_session_counter: HashMap<String, u32> =
            oz_platform::load_platform_counters(&counter_path);

        loop {
            match bot.get_updates(30).await {
                Ok(msgs) => {
                    for msg in &msgs {
                        if !WxBotClient::is_user_msg(msg) {
                            continue;
                        }
                        let msg_id = msg
                            .get("message_id")
                            .and_then(|v| v.as_i64())
                            .unwrap_or(0)
                            .to_string();
                        if seen_ids.contains(&msg_id) {
                            continue;
                        }
                        seen_ids.insert(msg_id);
                        if seen_ids.len() > 5000 {
                            seen_ids.clear();
                        }

                        let text = WxBotClient::extract_text(msg);
                        let uid = msg
                            .get("from_user_id")
                            .and_then(|v| v.as_str())
                            .unwrap_or("");
                        let ctx_token = msg
                            .get("context_token")
                            .and_then(|v| v.as_str())
                            .unwrap_or("");

                        if text.is_empty() {
                            continue;
                        }

                        // Safe UTF-8 slicing: find char boundary at or before byte 80
                        let preview: String = text.chars().take(80).collect();
                        tracing::info!("[wechat] message: {}", preview);

                        if text.starts_with('/') {
                            let new_sid = handle_wechat_command(
                                uid,
                                &text,
                                &mut new_session_counter,
                            );
                            oz_platform::save_platform_counters(&counter_path, &new_session_counter);
                            match new_sid {
                                Some((reply, sid)) => {
                                    let _ = bot.send_text(uid, &reply, ctx_token).await;
                                    if text.to_lowercase().starts_with("/new") {
                                        // /new just resets the session — don't run agent
                                        continue;
                                    }
                                    // Other commands may want to run agent after reply
                        let prompt = text.to_string();
                                    match agent.send_message(&sid, &prompt, "wechat", default_model.as_deref()).await {
                                        Ok(event_rx) => { stream_to_wechat(&bot, uid, ctx_token, event_rx).await; }
                                        Err(e) => { let _ = bot.send_text(uid, &format!("❌ {e}"), ctx_token).await; }
                                    }
                                }
                                None => continue,
                            }
                            continue;
                        }

                        let counter = new_session_counter.entry(uid.to_string()).or_insert(1);
                        let session_id = if *counter > 1 {
                            format!("wechat:{uid}:{counter}")
                        } else {
                            format!("wechat:{uid}")
                        };
                        let prompt = text.to_string();

                        match agent
                            .send_message(
                                &session_id,
                                &prompt,
                                "wechat",
                                default_model.as_deref(),
                            )
                            .await
                        {
                            Ok(event_rx) => {
                                stream_to_wechat(&bot, uid, ctx_token, event_rx).await;
                            }
                            Err(e) => {
                                let _ = bot
                                    .send_text(uid, &format!("❌ {e}"), ctx_token)
                                    .await;
                            }
                        }
                    }
                }
                Err(e) => {
                    if !e.contains("timeout") && !e.contains("Timeout") {
                        tracing::warn!("[wechat] get_updates error: {e}, retrying...");
                        tokio::time::sleep(std::time::Duration::from_secs(5)).await;
                    }
                }
            }
        }
    }

    async fn stop(&self) -> Result<(), PlatformError> {
        Ok(())
    }

    async fn health(&self) -> PlatformHealth {
        PlatformHealth::healthy()
    }
}

fn handle_wechat_command(
    uid: &str,
    text: &str,
    counter: &mut HashMap<String, u32>,
) -> Option<(String, String)> {
    let parts: Vec<&str> = text.split_whitespace().collect();
    let op = parts.first().map(|s| s.to_lowercase()).unwrap_or_default();
    let c = counter.entry(uid.to_string()).or_insert(1);

    match op.as_str() {
        "/help" => {
            let sid = if *c > 1 { format!("wechat:{uid}:{c}") } else { format!("wechat:{uid}") };
            Some(("📖 /help /stop /new /status /llm".to_string(), sid))
        }
        "/stop" | "/abort" => {
            let sid = if *c > 1 { format!("wechat:{uid}:{c}") } else { format!("wechat:{uid}") };
            Some(("⏹️ 已停止".to_string(), sid))
        }
        "/new" => {
            *c += 1;
            let sid = if *c > 1 { format!("wechat:{uid}:{c}") } else { format!("wechat:{uid}") };
            Some((format!("✅ 新对话已开启（会话: {sid}）"), sid))
        }
        "/status" => {
            let sid = if *c > 1 { format!("wechat:{uid}:{c}") } else { format!("wechat:{uid}") };
            Some(("🟢 OpenZen 运行中".to_string(), sid))
        }
        _ => None,
    }
}

async fn stream_to_wechat(
    bot: &WxBotClient,
    uid: &str,
    ctx_token: &str,
    mut event_rx: tokio::sync::mpsc::UnboundedReceiver<StreamEvent>,
) {
    let mut buffer = String::new();
    let mut sent_parts: usize = 0;
    let mut last_send = std::time::Instant::now();

    while let Some(event) = event_rx.recv().await {
        match event {
            StreamEvent::TextDelta { text, .. } => {
                buffer.push_str(&text);
            }
            StreamEvent::FinishMessage { .. } => {
                let cleaned = clean_for_wechat(&buffer);
                let rest = &cleaned[sent_parts.min(cleaned.len())..];
                let final_text = if rest.len() > 2000 {
                    &rest[rest.len().saturating_sub(2000)..]
                } else {
                    rest
                };
                let _ = bot.send_text(uid, final_text, ctx_token).await;

                let files = oz_platform::extract_files(&buffer);
                for file_path in &files {
                    let path = std::path::Path::new(file_path);
                    if path.exists() {
                        let ext = path
                            .extension()
                            .and_then(|e| e.to_str())
                            .unwrap_or("")
                            .to_lowercase();
                        if matches!(ext.as_str(), "jpg" | "jpeg" | "png" | "gif" | "webp" | "bmp")
                        {
                            let _ = bot.send_image(uid, path, ctx_token).await;
                        } else {
                            let _ = bot.send_file(uid, path, ctx_token).await;
                        }
                    }
                }
                return;
            }
            StreamEvent::Error { message } => {
                let _ = bot.send_text(uid, &format!("❌ {message}"), ctx_token).await;
                return;
            }
            _ => {}
        }

        let now = std::time::Instant::now();
        if sent_parts >= 9 || (sent_parts > 0 && now.duration_since(last_send).as_secs() < 6 * sent_parts as u64) {
            continue;
        }

        let cleaned = clean_for_wechat(&buffer);
        if cleaned.len() > sent_parts {
            let new_part = &cleaned[sent_parts..];
            let chunk = &new_part[..new_part.len().min(2000)];
            let _ = bot.send_text(uid, chunk, ctx_token).await;
            sent_parts = cleaned.len();
            last_send = now;
        }
    }
}

fn clean_for_wechat(text: &str) -> String {
    let mut result = text.to_string();

    let tags = ["thinking", "summary", "tool_use", "file_content"];
    // First pass: remove well-formed <tag>…</tag> pairs.
    for tag in &tags {
        let open = format!("<{tag}>");
        let close = format!("</{tag}>");
        while let (Some(start), Some(end)) = (result.find(&open), result.rfind(&close)) {
            if end > start {
                result.replace_range(start..end + close.len(), "");
            } else {
                break;
            }
        }
    }
    // Second pass: strip any leftover standalone tags.
    for tag in &tags {
        result = result.replace(&format!("</{tag}>"), "").replace(&format!("<{tag}>"), "");
    }

    let re_turn = regex::Regex::new(r"(?m)^\**LLM Running \(Turn \d+\) \.\.\.\**\s*$").ok();
    let re_tool = regex::Regex::new(r"(?m)^\s*🛠️\s*[A-Za-z_][A-Za-z0-9_]*\(.*$").ok();
    let re_links = regex::Regex::new(r"\[([^\]]+)\]\([^\)]+\)").ok();
    let re_images = regex::Regex::new(r"!\[.*?\]\(.*?\)").ok();
    let re_multi_nl = regex::Regex::new(r"\n{3,}").ok();

    if let Some(re) = re_turn {
        result = re.replace_all(&result, "").to_string();
    }
    if let Some(re) = re_tool {
        result = re.replace_all(&result, "").to_string();
    }
    if let Some(re) = re_images {
        result = re.replace_all(&result, "").to_string();
    }
    if let Some(re) = re_links {
        result = re.replace_all(&result, "$1").to_string();
    }
    if let Some(re) = re_multi_nl {
        result = re.replace_all(&result, "\n\n").to_string();
    }

    result.trim().to_string()
}
