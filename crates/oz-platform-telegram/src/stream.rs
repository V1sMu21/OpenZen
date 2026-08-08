use std::time::Instant;

use teloxide::prelude::*;
use teloxide::types::{ParseMode, ReplyParameters};

use crate::markdown;

const STREAM_SUFFIX: &str = " ⏳";
const MAX_MESSAGE_LENGTH: usize = 3800;
const UPDATE_INTERVAL_MS: u64 = 2000;
const MIN_UPDATE_CHARS: usize = 400;

pub struct StreamSession {
    bot: Bot,
    msg: Message,
    live_msg_id: Option<teloxide::types::MessageId>,
    buffer: String,
    last_update: Instant,
    last_update_len: usize,
    active_display: String,
    sent_segments: usize,
}

impl StreamSession {
    pub fn new(bot: Bot, msg: Message) -> Self {
        StreamSession {
            bot,
            msg,
            live_msg_id: None,
            buffer: String::new(),
            last_update: Instant::now(),
            last_update_len: 0,
            active_display: String::new(),
            sent_segments: 0,
        }
    }

    pub async fn start(&mut self) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        let msg = self
            .bot
            .send_message(self.msg.chat.id, "thinking...")
            .reply_parameters(ReplyParameters::new(self.msg.id))
            .await?;
        self.live_msg_id = Some(msg.id);
        self.active_display = "thinking...".into();
        self.last_update = Instant::now();
        Ok(())
    }

    pub async fn add_chunk(&mut self, chunk: &str) {
        self.buffer.push_str(chunk);

        let elapsed = self.last_update.elapsed().as_millis() as u64;
        let char_delta = self.buffer.len().saturating_sub(self.last_update_len);

        if elapsed >= UPDATE_INTERVAL_MS || char_delta >= MIN_UPDATE_CHARS {
            self.refresh(false).await;
        }
    }

    pub async fn finalize(&mut self) {
        self.refresh(true).await;

        let files = oz_platform::extract_files(&self.buffer);

        for file_path in &files {
            let path = std::path::Path::new(file_path);
            if !path.exists() {
                continue;
            }
            let ext = path
                .extension()
                .and_then(|e| e.to_str())
                .unwrap_or("")
                .to_lowercase();
            if matches!(ext.as_str(), "png" | "jpg" | "jpeg" | "gif" | "webp") {
                let input = teloxide::types::InputFile::file(path);
                let _ = self
                    .bot
                    .send_photo(self.msg.chat.id, input)
                    .reply_parameters(ReplyParameters::new(self.msg.id))
                    .await;
            } else {
                let input = teloxide::types::InputFile::file(path);
                let _ = self
                    .bot
                    .send_document(self.msg.chat.id, input)
                    .reply_parameters(ReplyParameters::new(self.msg.id))
                    .await;
            }
        }
    }

    pub async fn error(&mut self, message: &str) {
        if let Some(msg_id) = self.live_msg_id {
            let text = format!("❌ {message}");
            let _ = self.bot.edit_message_text(self.msg.chat.id, msg_id, text).await;
            self.live_msg_id = None;
        } else {
            let _ = self
                .bot
                .send_message(self.msg.chat.id, format!("❌ {message}"))
                .await;
        }
    }

    async fn refresh(&mut self, done: bool) {
        let cleaned = oz_platform::clean_agent_output(&self.buffer);
        if cleaned.is_empty() && !done {
            return;
        }

        let segments = markdown::split_into_segments(&cleaned, MAX_MESSAGE_LENGTH);
        let finalize_target = if done {
            segments.len()
        } else {
            segments.len().saturating_sub(1)
        };

        while self.sent_segments < finalize_target {
            let segment = &segments[self.sent_segments];
            let rendered = markdown::to_telegram_markdown_v2(segment);

            if let Some(msg_id) = self.live_msg_id.take() {
                let _ = self
                    .bot
                    .edit_message_text(self.msg.chat.id, msg_id, &rendered)
                    .parse_mode(ParseMode::MarkdownV2)
                    .await;
            } else {
                let sent = self
                    .bot
                    .send_message(self.msg.chat.id, &rendered)
                    .parse_mode(ParseMode::MarkdownV2)
                    .reply_parameters(ReplyParameters::new(self.msg.id))
                    .await;
                if let Ok(sent) = sent {
                    self.live_msg_id = Some(sent.id);
                }
            }
            self.sent_segments += 1;
        }

        if done {
            self.live_msg_id = None;
            self.active_display.clear();
            return;
        }

        if let Some(active) = segments.last() {
            let display = format!(
                "{}{STREAM_SUFFIX}",
                markdown::trim_to_fit(active, MAX_MESSAGE_LENGTH - STREAM_SUFFIX.len())
            );

            if display == self.active_display {
                return;
            }

            let rendered = markdown::to_telegram_markdown_v2(&display);

            if let Some(msg_id) = self.live_msg_id {
                let _ = self
                    .bot
                    .edit_message_text(self.msg.chat.id, msg_id, &rendered)
                    .parse_mode(ParseMode::MarkdownV2)
                    .await;
            } else {
                let sent = self
                    .bot
                    .send_message(self.msg.chat.id, &rendered)
                    .parse_mode(ParseMode::MarkdownV2)
                    .reply_parameters(ReplyParameters::new(self.msg.id))
                    .await;
                if let Ok(sent) = sent {
                    self.live_msg_id = Some(sent.id);
                }
            }

            self.active_display = display;
            self.last_update = Instant::now();
            self.last_update_len = self.buffer.len();
        }
    }
}
