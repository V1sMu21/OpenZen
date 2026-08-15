mod commands;
mod markdown;
mod stream;

use async_trait::async_trait;
use oz_platform::{
    PlatformAdapter, PlatformConfig, PlatformContext, PlatformError, PlatformHealth, FILE_HINT,
};
use teloxide::prelude::*;

use crate::stream::StreamSession;

pub struct TelegramAdapter {
    token: String,
    allowed_users: Option<Vec<i64>>,
    default_model: Option<String>,
}

impl TelegramAdapter {
    pub fn new(config: &PlatformConfig) -> Result<Self, PlatformError> {
        let token = config
            .telegram_token()
            .ok_or_else(|| PlatformError::Config("telegram.bot_token is required".into()))?
            .to_string();
        let allowed_users = config
            .allowed_users
            .as_ref()
            .map(|ids| ids.iter().filter_map(|s| s.parse::<i64>().ok()).collect());
        Ok(TelegramAdapter {
            token,
            allowed_users,
            default_model: config.default_model.clone(),
        })
    }
}

#[async_trait]
impl PlatformAdapter for TelegramAdapter {
    fn id(&self) -> &'static str {
        "telegram"
    }

    fn name(&self) -> &'static str {
        "Telegram"
    }

    async fn start(&self, ctx: PlatformContext) -> Result<(), PlatformError> {
        let mut retry_delay = std::time::Duration::from_secs(10);
        loop {
            let agent = ctx.agent.clone();
            let allowed = self.allowed_users.clone();
            let default_model = self.default_model.clone();
            let bot = Bot::new(&self.token);

            tracing::info!("[telegram] bot starting (retry_delay={:?})...", retry_delay);

            teloxide::repl(bot, move |bot: Bot, msg: Message| {
                let agent = agent.clone();
                let allowed = allowed.clone();
                let default_model = default_model.clone();
                async move {
                    let user_id = msg.from.as_ref().map(|u| u.id.0 as i64).unwrap_or(0);
                    if let Some(ref allowed) = allowed {
                        if !allowed.is_empty() && !allowed.contains(&user_id) {
                            let _ = bot.send_message(msg.chat.id, "no").await;
                            return Ok(());
                        }
                    }

                    if let Some(text) = msg.text() {
                        let text = text.trim().to_string();
                        if text.is_empty() {
                            return Ok(());
                        }

                        if text.starts_with('/') {
                            commands::handle_command(&bot, &msg, &text).await;
                            return Ok(());
                        }

                        let chat_id = msg.chat.id.0;
                        let session_id = format!("tg:{chat_id}");
                        let prompt = format!("{FILE_HINT}\n\n{text}");
                        let model = default_model.as_deref();

                        match agent
                            .send_message(&session_id, &prompt, "telegram", model)
                            .await
                        {
                            Ok(event_rx) => {
                                stream_agent_output(&bot, &msg, event_rx).await;
                            }
                            Err(e) => {
                                let _ = bot.send_message(msg.chat.id, format!("❌ {e}")).await;
                            }
                        }
                    } else if msg.photo().is_some() {
                        let caption = msg.caption().unwrap_or("").to_string();
                        let chat_id = msg.chat.id.0;
                        let session_id = format!("tg:{chat_id}");
                        let prompt = if caption.is_empty() {
                            "[用户发送了图片]".to_string()
                        } else {
                            format!("[用户发送了图片]\n{caption}")
                        };

                        match agent
                            .send_message(&session_id, &prompt, "telegram", None)
                            .await
                        {
                            Ok(event_rx) => {
                                stream_agent_output(&bot, &msg, event_rx).await;
                            }
                            Err(e) => {
                                let _ = bot.send_message(msg.chat.id, format!("❌ {e}")).await;
                            }
                        }
                    }

                    Ok(())
                }
            })
            .await;

            tracing::warn!(
                "[telegram] disconnected, reconnecting in {:?}...",
                retry_delay
            );
            tokio::time::sleep(retry_delay).await;
            retry_delay = std::cmp::min(retry_delay * 2, std::time::Duration::from_secs(60));
        }
    }

    async fn stop(&self) -> Result<(), PlatformError> {
        tracing::info!("[telegram] stopping");
        Ok(())
    }

    async fn health(&self) -> PlatformHealth {
        PlatformHealth::healthy()
    }
}

async fn stream_agent_output(
    bot: &Bot,
    msg: &Message,
    mut event_rx: tokio::sync::mpsc::UnboundedReceiver<oz_core_types::StreamEvent>,
) {
    let mut session = StreamSession::new(bot.clone(), msg.clone());

    if let Err(e) = session.start().await {
        tracing::warn!("[telegram] stream start error: {e}");
        return;
    }

    while let Some(event) = event_rx.recv().await {
        match event {
            oz_core_types::StreamEvent::TextDelta { text, .. } => {
                session.add_chunk(&text).await;
            }
            oz_core_types::StreamEvent::FinishMessage { .. } => {
                session.finalize().await;
                return;
            }
            oz_core_types::StreamEvent::Error { message } => {
                session.error(&message).await;
                return;
            }
            _ => {}
        }
    }
}
