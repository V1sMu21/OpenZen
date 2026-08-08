use std::sync::Mutex;
use std::time::{Duration, Instant};

use oz_core_types::{ContentBlock, LlmError, Message, TokenUsage, ToolDefinition};

use crate::session::Session;

pub struct MixinSession {
    sessions: Vec<Box<dyn Session>>,
    cur_idx: usize,
    switched_at: Instant,
    spring_back_secs: f64,
    retries: u32,
    base_delay: f64,
}

impl MixinSession {
    pub fn new(sessions: Vec<Box<dyn Session>>, _llm_nos: Option<Vec<usize>>,
               max_retries: Option<u32>, base_delay: Option<f64>, spring_back: Option<u64>) -> Self
    {
        MixinSession {
            sessions,
            cur_idx: 0,
            switched_at: Instant::now(),
            spring_back_secs: spring_back.unwrap_or(300) as f64,
            retries: max_retries.unwrap_or(3),
            base_delay: base_delay.unwrap_or(1.5),
        }
    }

    fn pick(&self) -> usize {
        if self.cur_idx != 0 && self.switched_at.elapsed().as_secs_f64() > self.spring_back_secs {
            0 // spring back to primary
        } else {
            self.cur_idx
        }
    }
}

#[async_trait::async_trait]
impl Session for MixinSession {
    fn config(&self) -> &oz_config::SessionConfig {
        self.sessions[self.pick()].config()
    }
    fn history(&self) -> &Mutex<Vec<Message>> {
        self.sessions[0].history()
    }
    fn history_mut(&self) -> &Mutex<Vec<Message>> {
        self.sessions[0].history_mut()
    }
    fn set_system(&mut self, system: String) {
        for s in &mut self.sessions {
            s.set_system(system.clone());
        }
    }
    fn set_tools(&mut self, tools: Vec<ToolDefinition>) {
        for s in &mut self.sessions {
            s.set_tools(tools.clone());
        }
    }

    async fn raw_ask(&self, messages: &[Message]) -> Result<(Vec<ContentBlock>, Option<TokenUsage>), LlmError> {
        let base = self.pick();
        let n = self.sessions.len();

        for attempt in 0..=self.retries {
            let idx = (base + attempt as usize) % n;
            tracing::info!("[MixinSession] Using session ({})", self.sessions[idx].model());

            match self.sessions[idx].raw_ask(messages).await {
                Ok(result) => {
                    if attempt > 0 {
                        tracing::info!("[MixinSession] Switched to session {}", idx);
                    }
                    return Ok(result);
                }
                Err(e) if attempt < self.retries => {
                    let delay = self.base_delay * (1.5f64).powi(attempt as i32);
                    let delay = delay.min(30.0);
                    tracing::warn!("[MixinSession] Session {} failed: {e}, retry {}/{}, delay {delay:.1}s",
                        idx, attempt + 1, self.retries);
                    tokio::time::sleep(Duration::from_secs_f64(delay)).await;
                }
                Err(e) => return Err(e),
            }
        }
        Err(LlmError::AllSessionsFailed)
    }

    async fn ask(&self, prompt: &str) -> Result<Vec<ContentBlock>, LlmError> {
        let raw_messages = {
            let mut history = self.history().lock().map_err(|e| LlmError::Custom(e.to_string()))?;
            history.push(Message::user(prompt));
            if history.len() > 5 {
                crate::retry::trim_history(&mut history, self.config().context_win);
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
                let mut history = self.history().lock().map_err(|e| LlmError::Custom(e.to_string()))?;
                history.push(Message::assistant_with_blocks(blocks.clone()));
            }
        }
        Ok(blocks)
    }

    fn format_messages(&self, messages: &[Message]) -> Vec<serde_json::Value> {
        self.sessions[self.pick()].format_messages(messages)
    }
}
