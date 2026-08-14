use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use oz_core::handler::LoopConfig;
use oz_core_types::StreamEvent;
use oz_server::webui::sessions::{SessionStatus, SessionStore};
use tokio::sync::mpsc;
use tokio::task::JoinHandle;

use crate::{PlatformError, FILE_HINT};

pub struct AgentBridge {
    pub sessions: Arc<Mutex<SessionStore>>,
    pub running_agents: Arc<Mutex<HashMap<String, JoinHandle<()>>>>,
    pub stop_signals: Arc<Mutex<HashMap<String, Arc<AtomicBool>>>>,
    pub config_path: String,
    pub working_dir: String,
    pub assets_dir: String,
    pub script_dir: String,
    pub locale: Arc<Mutex<String>>,
    pub approval_handler: Arc<Mutex<Option<Arc<dyn oz_safety::ApprovalHandler>>>>,
    pub ask_user_rxs: Arc<Mutex<HashMap<String, Arc<Mutex<Option<String>>>>>>,
    pub skill_mcp_dir: Option<String>,
}

impl AgentBridge {
    pub async fn send_message(
        &self,
        session_id: &str,
        message: &str,
        source: &str,
        model_name: Option<&str>,
    ) -> Result<mpsc::UnboundedReceiver<StreamEvent>, PlatformError> {
        {
            let mut store = self.sessions.lock().unwrap();
            if !store.has_session(session_id) {
                let name = format!("[{}] {}", source, chrono::Local::now().format("%H:%M"));
                store.create_with_id(session_id, &name);
            }
            if let Some(s) = store.get_mut(session_id) {
                s.status = SessionStatus::Running;
                s.messages.push(serde_json::json!({
                    "role": "user",
                    "content": message,
                    "source": source,
                    "timestamp": chrono::Utc::now().to_rfc3339(),
                }));
            }
            store.save();
        }

        {
            let mut agents = self.running_agents.lock().unwrap();
            if let Some(handle) = agents.remove(session_id) {
                if let Some(stop_signal) = {
                    let signals = self.stop_signals.lock().unwrap();
                    signals.get(session_id).cloned()
                } {
                    stop_signal.store(true, Ordering::Relaxed);
                }
                handle.abort();
            }
        }

        let config_path = self.resolve_config_path();
        let cfg = oz_config::mykey::MyKeyConfig::from_file(Path::new(&config_path))
            .map_err(|e| PlatformError::Config(format!("Config error: {e}")))?;

        // Resolve session: try model_name first, then default_session,
        // then the first available session in config. This handles platforms
        // whose default_model config points to a non-existent session name.
        let session_name = if let Some(name) = model_name {
            if cfg.get(name).is_some() {
                name
            } else {
                tracing::warn!("[platform] model '{name}' not found in config, falling back");
                cfg.default_session
                    .as_deref()
                    .or_else(|| cfg.iter_sessions().next().map(|(k, _)| k.as_str()))
                    .unwrap_or("claude_sonnet")
            }
        } else {
            cfg.default_session
                .as_deref()
                .or_else(|| cfg.iter_sessions().next().map(|(k, _)| k.as_str()))
                .unwrap_or("claude_sonnet")
        };
        let sess_config = cfg
            .get(session_name)
            .ok_or_else(|| PlatformError::Config(format!("Session '{session_name}' not found")))?
            .clone();
        let sess_type = cfg.session_type(session_name);

        let ctx = oz_core_types::ToolContext {
            working_dir: self.working_dir.clone(),
            assets_dir: self.assets_dir.clone(),
            script_dir: self.script_dir.clone(),
            lang: self.locale.lock().unwrap().clone(),
            skill_mcp_dir: self.skill_mcp_dir.clone(),
            harness_dir: None,
            session_id: String::new(),
        };

        let backend: Box<dyn oz_llm::Session> = match sess_type {
            oz_config::mykey::SessionType::Claude => {
                Box::new(oz_llm::ClaudeSession::new(sess_config.clone()))
            }
            oz_config::mykey::SessionType::Oai => {
                Box::new(oz_llm::OaiSession::new(sess_config.clone()))
            }
            oz_config::mykey::SessionType::NativeClaude => {
                Box::new(oz_llm::NativeClaudeSession::new(sess_config.clone()))
            }
            oz_config::mykey::SessionType::NativeOai => {
                Box::new(oz_llm::NativeOAISession::new(sess_config.clone()))
            }
            oz_config::mykey::SessionType::Mixin => {
                return Err(PlatformError::Config(
                    "Mixin session not supported in platform adapters".into(),
                ));
            }
        };
        let mut client = oz_llm::NativeToolClient::new(backend);

        let memory = oz_memory::MemorySystem::new(Path::new(&self.working_dir), &ctx.lang);
        let memory_context = memory.get_global_memory().await.unwrap_or_default();

        let registry = oz_tools::registry::ToolRegistry::build_default();
        let definitions = registry.to_schema("en");
        let mut handler = oz_tools::handler::ToolRegistryHandler::new(registry);

        let mut system_prompt = load_system_prompt_from(Path::new(&self.assets_dir), &ctx.lang);
        system_prompt.push('\n');
        system_prompt.push_str(FILE_HINT);
        if !memory_context.is_empty() {
            system_prompt.push_str("\n\n## Persistent Memory Context\n\n");
            system_prompt.push_str(&memory_context);
        }

        let mut loop_config = LoopConfig::default();
        loop_config.verbose = false;
        loop_config.context_win = sess_config.context_win;
        loop_config.session_id = session_id.to_string();
        loop_config.working_dir = self.working_dir.clone();
        loop_config.skill_mcp_dir = self.skill_mcp_dir.clone();

        let trust_path = Path::new(&self.working_dir).join("openzen/trust.json");
        let trust_store = oz_safety::TrustStore::new(Some(trust_path));
        loop_config.safety_guard = Some(Arc::new(oz_safety::SafetyGuard::new(trust_store)));
        loop_config.approval_handler = self.approval_handler.lock().unwrap().clone();

        {
            let mut ask_rxs = self.ask_user_rxs.lock().unwrap();
            let slot = ask_rxs
                .entry(session_id.to_string())
                .or_insert_with(|| Arc::new(Mutex::new(None)));
            *slot.lock().unwrap() = None;
            loop_config.ask_user_rx = Some(slot.clone());
        }

        let (event_tx, event_rx) = mpsc::unbounded_channel::<StreamEvent>();
        loop_config.event_tx = Some(event_tx);

        let stop_signal = Arc::new(AtomicBool::new(false));
        {
            let mut map = self.stop_signals.lock().unwrap();
            map.insert(session_id.to_string(), stop_signal.clone());
        }

        let session_id_owned = session_id.to_string();
        let message_owned = message.to_string();
        let stop_signals_clone = self.stop_signals.clone();
        let sessions_clone = self.sessions.clone();

        let handle = tokio::spawn(async move {
            let outcome = oz_core::agent_loop::run_agent_loop(
                &mut client,
                system_prompt,
                message_owned,
                Vec::new(),
                &mut handler,
                &definitions,
                &ctx,
                &loop_config,
                &stop_signal,
            )
            .await;

            eprintln!("[platform] agent loop finished: exit_reason={}, turn={}", outcome.exit_reason, outcome.turn);

            // Send FinishMessage so platform adapters (Feishu, Telegram, etc.)
            // can finalize their streaming cards. The TUI does this in its own
            // event loop; the platform bridge must also send it before dropping
            // the event_tx, otherwise the card stays at "thinking" forever.
            if let Some(ref tx) = loop_config.event_tx {
                let _ = tx.send(StreamEvent::FinishMessage {
                    stop_reason: outcome.exit_reason.clone(),
                });
            }

            stop_signals_clone.lock().unwrap().remove(&session_id_owned);

            let mut store = sessions_clone.lock().unwrap();
            if let Some(s) = store.get_mut(&session_id_owned) {
                s.status = SessionStatus::Idle;

                // Save assistant's final response to the session store.
                // The desktop app reads from this store to display chat history.
                if let Some(ref data) = outcome.data {
                    let full_response = data
                        .get("full_response")
                        .and_then(|v| v.as_str())
                        .unwrap_or("");
                    let cleaned = crate::clean_agent_output(full_response);
                    if !cleaned.is_empty() {
                        s.messages.push(serde_json::json!({
                            "role": "assistant",
                            "content": cleaned,
                            "timestamp": chrono::Utc::now().to_rfc3339(),
                        }));
                        store.save();
                    }
                }
            }
            drop(store);
        });

        self.running_agents
            .lock()
            .unwrap()
            .insert(session_id.to_string(), handle);

        Ok(event_rx)
    }

    pub fn stop_session(&self, session_id: &str) {
        if let Some(signal) = self.stop_signals.lock().unwrap().get(session_id) {
            signal.store(true, std::sync::atomic::Ordering::SeqCst);
        }
        let mut store = self.sessions.lock().unwrap();
        if let Some(entry) = store.get_mut(session_id) {
            entry.status = SessionStatus::Stopped;
        }
        store.save();
    }

    pub fn session_status(&self, session_id: &str) -> SessionStatus {
        self.sessions
            .lock()
            .unwrap()
            .get(session_id)
            .map(|s| s.status.clone())
            .unwrap_or(SessionStatus::Idle)
    }

    pub fn ask_user_response(&self, session_id: &str, response: &str) {
        if let Some(slot) = self.ask_user_rxs.lock().unwrap().get(session_id) {
            *slot.lock().unwrap() = Some(response.to_string());
        }
    }

    pub fn is_running(&self, session_id: &str) -> bool {
        self.running_agents.lock().unwrap().contains_key(session_id)
    }

    fn resolve_config_path(&self) -> String {
        let home = std::env::var("HOME").unwrap_or_else(|_| "/tmp".into());
        let candidates = [
            PathBuf::from(&self.config_path),
            PathBuf::from(&home).join("mykey.toml"),
            PathBuf::from("config/mykey.toml"),
            PathBuf::from("mykey.toml"),
        ];
        for c in &candidates {
            if c.exists() {
                return c.to_string_lossy().to_string();
            }
        }
        self.config_path.clone()
    }
}

fn load_system_prompt_from(assets_dir: &Path, lang: &str) -> String {
    let suffix = if lang == "en" { "_en" } else { "" };
    let path = assets_dir.join(format!("sys_prompt{}.txt", suffix));
    std::fs::read_to_string(&path).unwrap_or_else(|_| {
        format!(
            "You are OpenZen, an autonomous AI agent. Today is {}.",
            chrono::Local::now().format("%Y-%m-%d %a")
        )
    })
}
