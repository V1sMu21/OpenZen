mod client;
mod crypto;

use std::collections::HashMap;
use std::sync::Arc;

use async_trait::async_trait;
use oz_core_types::StreamEvent;
use oz_platform::{
    AgentBridge, PlatformAdapter, PlatformConfig, PlatformContext, PlatformError, PlatformHealth,
};

use crate::client::WxBotClient;

pub struct WechatAdapter {
    default_model: Option<String>,
    /// Long-poll state reported by the loop and read by health().
    conn: Arc<oz_platform::ConnectionHealth>,
}

impl WechatAdapter {
    pub fn new(config: &PlatformConfig) -> Self {
        WechatAdapter {
            default_model: config.default_model.clone(),
            conn: oz_platform::ConnectionHealth::shared(),
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
        let conn = self.conn.clone();

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
        // Persistent dedup: messages replayed by the WeChat server after a
        // crash/restart must not trigger duplicate agent runs.
        let dedup_path = ctx
            .working_dir
            .join("openzen")
            .join("wechat_seen_msg_ids.json");
        let mut seen_ids: std::collections::VecDeque<String> =
            oz_platform::load_seen_msg_ids(&dedup_path);
        // Shared with spawned agent tasks: per-user counters and the
        // per-user "task running" gate must be consistent across them.
        let counters: Arc<tokio::sync::Mutex<HashMap<String, u32>>> = Arc::new(
            tokio::sync::Mutex::new(oz_platform::load_platform_counters(&counter_path)),
        );
        let running: Arc<tokio::sync::Mutex<HashMap<String, bool>>> =
            Arc::new(tokio::sync::Mutex::new(HashMap::new()));

        conn.report_connected();
        loop {
            match bot.get_updates(30).await {
                Ok(msgs) => {
                    // Every completed long-poll proves the loop is alive.
                    conn.report_activity();
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
                        seen_ids.push_back(msg_id);
                        if seen_ids.len() > 5000 {
                            seen_ids.pop_front();
                        }
                        oz_platform::save_seen_msg_ids(&dedup_path, &seen_ids);

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

                        // Commands are handled before the running-task gate so
                        // /stop and /status stay usable while an agent runs.
                        if text.starts_with('/') {
                            let result = {
                                let mut c = counters.lock().await;
                                let r = handle_wechat_command(uid, &text, &mut c);
                                oz_platform::save_platform_counters(&counter_path, &c);
                                r
                            };
                            let Some((reply, sid)) = result else { continue };
                            let _ = bot.send_text(uid, &reply, ctx_token).await;
                            let op = text
                                .split_whitespace()
                                .next()
                                .map(|s| s.to_lowercase())
                                .unwrap_or_default();
                            if op == "/stop" || op == "/abort" {
                                // /stop must actually stop the running session —
                                // never start a new agent run with the command
                                // text as its prompt.
                                agent.stop_session(&sid);
                                continue;
                            }
                            if op == "/new" {
                                // /new just resets the session — don't run agent
                                continue;
                            }
                            // Other commands may want to run agent after reply
                            spawn_wechat_agent(
                                &agent,
                                bot.clone(),
                                &running,
                                uid,
                                ctx_token,
                                &sid,
                                &text,
                                &default_model,
                            );
                            continue;
                        }

                        let sid = {
                            let mut c = counters.lock().await;
                            let counter = c.entry(uid.to_string()).or_insert(1);
                            if *counter > 1 {
                                format!("wechat:{uid}:{counter}")
                            } else {
                                format!("wechat:{uid}")
                            }
                        };

                        {
                            let mut r = running.lock().await;
                            if r.contains_key(uid) {
                                let _ = bot
                                    .send_text(uid, "⏳ 上一个任务进行中，请稍候…", ctx_token)
                                    .await;
                                continue;
                            }
                            // Mark running so a second message from this user
                            // during the run gets the busy reply instead of
                            // queueing behind it.
                            r.insert(uid.to_string(), true);
                        }

                        spawn_wechat_agent(
                            &agent,
                            bot.clone(),
                            &running,
                            uid,
                            ctx_token,
                            &sid,
                            &text,
                            &default_model,
                        );
                    }
                }
                Err(e) => {
                    conn.report_disconnected();
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
        // Stale window: the long-poll cycle is ~30s (+ processing); no
        // completed poll for 5 minutes means the loop is wedged.
        let (connected, last) = self.conn.snapshot(300);
        if connected {
            PlatformHealth::healthy()
        } else {
            PlatformHealth::disconnected(format!(
                "long-poll loop not progressing (last activity unix-secs: {last:?})"
            ))
        }
    }
}

/// Clears the per-user running flag when the stream task ends — including
/// during a panic unwind, so a mid-task panic cannot leave the user locked
/// into "previous task still running" until process restart.
struct RunningGuard {
    running: Arc<tokio::sync::Mutex<HashMap<String, bool>>>,
    uid: String,
}

impl Drop for RunningGuard {
    fn drop(&mut self) {
        let running = self.running.clone();
        let uid = std::mem::take(&mut self.uid);
        tokio::spawn(async move {
            running.lock().await.remove(&uid);
        });
    }
}

/// Spawn an agent run for one WeChat message and stream the result back.
/// Returns immediately — the long-poll loop must never block on a full
/// agent run, otherwise one user's long task stalls every other user.
#[allow(clippy::too_many_arguments)]
fn spawn_wechat_agent(
    agent: &Arc<AgentBridge>,
    bot: WxBotClient,
    running: &Arc<tokio::sync::Mutex<HashMap<String, bool>>>,
    uid: &str,
    ctx_token: &str,
    sid: &str,
    prompt: &str,
    default_model: &Option<String>,
) {
    let agent = agent.clone();
    let running = running.clone();
    let uid = uid.to_string();
    let ctx_token = ctx_token.to_string();
    let sid = sid.to_string();
    let prompt = prompt.to_string();
    let model = default_model.clone();

    tokio::spawn(async move {
        let _running_guard = RunningGuard {
            running,
            uid: uid.clone(),
        };
        match agent
            .send_message(&sid, &prompt, "wechat", model.as_deref())
            .await
        {
            Ok(event_rx) => stream_to_wechat(&bot, &uid, &ctx_token, event_rx).await,
            Err(e) => {
                let _ = bot.send_text(&uid, &format!("❌ {e}"), &ctx_token).await;
            }
        }
    });
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
            let sid = if *c > 1 {
                format!("wechat:{uid}:{c}")
            } else {
                format!("wechat:{uid}")
            };
            Some(("📖 /help /stop /new /status /llm".to_string(), sid))
        }
        "/stop" | "/abort" => {
            let sid = if *c > 1 {
                format!("wechat:{uid}:{c}")
            } else {
                format!("wechat:{uid}")
            };
            Some(("⏹️ 已停止".to_string(), sid))
        }
        "/new" => {
            *c += 1;
            let sid = if *c > 1 {
                format!("wechat:{uid}:{c}")
            } else {
                format!("wechat:{uid}")
            };
            Some((format!("✅ 新对话已开启（会话: {sid}）"), sid))
        }
        "/status" => {
            let sid = if *c > 1 {
                format!("wechat:{uid}:{c}")
            } else {
                format!("wechat:{uid}")
            };
            Some(("🟢 OpenZen 运行中".to_string(), sid))
        }
        _ => None,
    }
}

/// Byte index snapped down to a UTF-8 char boundary. Raw byte slicing of
/// CJK text panics when the index lands mid-char; every stream offset in
/// this module goes through this snap.
fn floor_boundary(s: &str, i: usize) -> usize {
    let mut i = i.min(s.len());
    while i > 0 && !s.is_char_boundary(i) {
        i -= 1;
    }
    i
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
                let rest = &cleaned[floor_boundary(&cleaned, sent_parts)..];
                let final_text = if rest.len() > 2000 {
                    &rest[floor_boundary(rest, rest.len().saturating_sub(2000))..]
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
                        if matches!(
                            ext.as_str(),
                            "jpg" | "jpeg" | "png" | "gif" | "webp" | "bmp"
                        ) {
                            let _ = bot.send_image(uid, path, ctx_token).await;
                        } else {
                            let _ = bot.send_file(uid, path, ctx_token).await;
                        }
                    }
                }
                return;
            }
            StreamEvent::Error { message } => {
                let _ = bot
                    .send_text(uid, &format!("❌ {message}"), ctx_token)
                    .await;
                return;
            }
            _ => {}
        }

        let now = std::time::Instant::now();
        if sent_parts >= 9
            || (sent_parts > 0 && now.duration_since(last_send).as_secs() < 6 * sent_parts as u64)
        {
            continue;
        }

        let cleaned = clean_for_wechat(&buffer);
        if cleaned.len() > sent_parts {
            let new_part = &cleaned[floor_boundary(&cleaned, sent_parts)..];
            let chunk = &new_part[..floor_boundary(new_part, 2000)];
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
        result = result
            .replace(&format!("</{tag}>"), "")
            .replace(&format!("<{tag}>"), "");
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
