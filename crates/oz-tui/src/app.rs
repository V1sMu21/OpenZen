//! App state and domain types for the TUI.
//!
//! `App` is the single source of UI state. `ChatItem` is the unified
//! stream of renderable items that the chat pane draws — every kind
//! of incoming event (user message, agent text delta, tool call,
//! thinking chunk, system notice) eventually becomes a `ChatItem`.

use std::path::PathBuf;
use std::sync::atomic::AtomicBool;
use std::sync::Arc;

use tokio::sync::mpsc;

use oz_core_types::Message;
use oz_server::webui::sessions::SessionStore;
use oz_server::webui::sse_bus::SseBus;

/// Length threshold above which a long assistant message is folded
/// into a 5-line preview with "··· N more lines ···".
pub const LONG_MSG_THRESHOLD: usize = 30;
pub const LONG_MSG_PREVIEW: usize = 5;

// ── Input mode ──

#[derive(Debug, PartialEq, Eq, Clone, Copy)]
pub enum InputMode {
    /// Typing into the input box. This is the *only* normal mode —
    /// the user can always type, no keypress required.
    Editing,
    /// `ask_user` dialog visible — arrow keys to pick, Enter to confirm.
    AskUser,
}

// ── Role classification (drives separator line logic) ──

#[derive(Debug, PartialEq, Eq, Clone, Copy)]
pub enum MsgRole {
    User,
    Agent,
    System,
    None,
}

pub fn chat_item_role(item: &ChatItem) -> MsgRole {
    match item {
        ChatItem::UserMessage { .. } => MsgRole::User,
        ChatItem::AssistantText { .. }
        | ChatItem::SummaryHeader { .. }
        | ChatItem::SummaryBody { .. }
        | ChatItem::ToolCall { .. }
        | ChatItem::ThinkingHeader { .. }
        | ChatItem::ThinkingBody { .. } => MsgRole::Agent,
        ChatItem::SystemMessage { .. } | ChatItem::AskUserItem { .. } => MsgRole::System,
    }
}

// ── ChatItem variants ──

#[derive(Debug, Clone)]
pub enum ChatItem {
    UserMessage {
        content: String,
        ts: String,
    },
    ThinkingHeader {
        duration: String,
        words: usize,
        expanded: bool,
    },
    ThinkingBody {
        content: String,
    },
    AssistantText {
        content: String,
        ts: String,
        expanded: bool,
    },
    /// One-line summary the system prompt asks the model to emit
    /// every turn. Shown as a single highlighted line; expand shows
    /// the full content.
    SummaryHeader {
        content: String,
        expanded: bool,
    },
    SummaryBody {
        content: String,
    },
    ToolCall {
        name: String,
        args: String,
        status: ToolStatus,
        result: String,
        ts: String,
        expanded: bool,
    },
    AskUserItem {
        question: String,
        candidates: Vec<String>,
        status: AskUserStatus,
        response: Option<String>,
    },
    SystemMessage {
        content: String,
        ts: String,
    },
}

#[derive(Debug, PartialEq, Eq, Clone, Copy)]
pub enum ToolStatus {
    Running,
    Done,
    Error,
}

#[derive(Debug, PartialEq, Eq, Clone, Copy)]
pub enum AskUserStatus {
    Pending,
    Answered,
}

// ── App state ──

pub struct App {
    // Input
    pub input: String,
    pub input_mode: InputMode,
    pub cmd_mode: bool,
    pub cmd_buffer: String,
    pub cmd_suggestions: Vec<&'static str>,
    pub cmd_selected: usize,
    pub confirm_quit: bool,
    pub confirm_delete: bool,
    /// Set by `/exit` command. Main loop checks this to know when
    /// to terminate the TUI.
    pub should_quit: bool,

    // Chat state
    pub items: Vec<ChatItem>,
    pub chat_scroll: usize,
    /// Auto-scroll to the tail of the chat. Cleared when the user
    /// scrolls up manually; restored by `G`.
    pub follow_tail: bool,

    // Session
    pub current_id: Option<String>,
    pub session_id_for_run: String,
    pub session_store: SessionStore,

    // Status
    pub status: String,
    pub is_processing: bool,
    pub started_at: Option<std::time::Instant>,
    pub frame_count: u64,
    pub model_name: String,
    pub model_provider: String,
    pub current_tool_name: String,
    pub last_error: Option<String>,
    pub current_agent: Option<String>,

    // Async
    pub stop_signal: Option<Arc<AtomicBool>>,
    pub pending_ask_user: Option<(String, Vec<String>)>,
    pub sse_bus: SseBus,
    pub event_rx: Option<mpsc::UnboundedReceiver<oz_core_types::StreamEvent>>,

    // Filesystem
    pub working_dir: String,
    pub assets_dir: String,
    pub config_path: String,
    pub system_prompt: String,

            // Prompt template — loaded from `[tui] left_prompt` /
            // `right_prompt` in mykey.toml. See `template` module.
    pub left_prompt: Option<crate::template::PromptTemplate>,
    pub right_prompt: Option<crate::template::PromptTemplate>,
    pub tokens_total: std::sync::atomic::AtomicU64,
    pub theme: crate::theme::Theme,
}

impl App {
    pub fn new(working_dir: String, assets_dir: String, config_path: String) -> Self {
        let sessions_path = PathBuf::from(&working_dir).join("openzen").join("tui-sessions.json");
        let _ = std::fs::create_dir_all(sessions_path.parent().unwrap());
        let session_store = SessionStore::persisted(sessions_path);
        let sessions = session_store.list();
        let current_id = sessions.first().map(|s| s.id.clone());
        let sse_bus = SseBus::new(1_000);
        App {
            input: String::new(),
            input_mode: InputMode::Editing,
            cmd_mode: false,
            cmd_buffer: String::new(),
            cmd_suggestions: Vec::new(),
            cmd_selected: 0,
            confirm_quit: false,
            confirm_delete: false,
            should_quit: false,
            items: Vec::new(),
            chat_scroll: 0,
            follow_tail: true,
            current_id,
            session_id_for_run: String::new(),
            session_store,
            status: "type to chat · / for commands · PgUp/PgDn scroll history · G bottom · g top".into(),
            is_processing: false,
            started_at: None,
            frame_count: 0,
            model_name: String::new(),
            model_provider: String::new(),
            current_tool_name: String::new(),
            last_error: None,
            current_agent: None,
            stop_signal: None,
            pending_ask_user: None,
            sse_bus,
            event_rx: None,
            working_dir,
            assets_dir,
            config_path,
            system_prompt: String::new(),
            left_prompt: None,
            right_prompt: None,
            tokens_total: std::sync::atomic::AtomicU64::new(0),
            theme: crate::theme::Theme::default(),
        }
    }

    /// Build a `Vars` map from the current app state for prompt
    /// template rendering. Variables for the future Phase 3
    /// (`agent`, `role`, `rag`) are present but empty until those
    /// subsystems land.
    pub fn template_vars(&self) -> crate::template::Vars {
        let mut vars = crate::template::Vars::new();
        let model = if self.model_name.is_empty() {
            "—".to_string()
        } else {
            self.model_name.clone()
        };
        vars.insert("model", model);
        let session = self
            .current_id
            .as_ref()
            .and_then(|id| self.session_store.get(id))
            .map(|s| s.info.name.clone())
            .unwrap_or_default();
        vars.insert("session", session);
        vars.insert("agent", self.current_agent.clone().unwrap_or_default());
        vars.insert("role", "");
        vars.insert("rag", "");
        let tokens = self
            .tokens_total
            .load(std::sync::atomic::Ordering::Relaxed);
        vars.insert("consume_tokens", tokens.to_string());
        vars.insert("consume_percent", "0".to_string());
        vars
    }

    pub fn now_ts(&self) -> String {
        chrono::Local::now().format("%H:%M:%S").to_string()
    }

    pub fn add_user_message(&mut self, text: &str) {
        self.items.push(ChatItem::UserMessage {
            content: text.to_string(),
            ts: self.now_ts(),
        });
    }

    pub fn add_assistant_text(&mut self, text: &str) {
        self.items.push(ChatItem::AssistantText {
            content: text.to_string(),
            ts: self.now_ts(),
            expanded: true,
        });
    }

    pub fn add_thinking_header(&mut self, dur_ms: u64, words: usize) {
        self.items.push(ChatItem::ThinkingHeader {
            duration: format!("{:.1}s", dur_ms as f64 / 1000.0),
            words,
            expanded: false,
        });
    }

    pub fn add_thinking_body(&mut self, content: &str) {
        self.items.push(ChatItem::ThinkingBody {
            content: content.to_string(),
        });
    }

    pub fn add_summary(&mut self, content: &str) {
        let first_line = content.lines().next().unwrap_or("").to_string();
        if first_line.is_empty() {
            return;
        }
        self.items.push(ChatItem::SummaryHeader {
            content: first_line,
            expanded: false,
        });
        if content.lines().count() > 1 {
            self.items.push(ChatItem::SummaryBody {
                content: content.to_string(),
            });
        }
    }

    pub fn add_tool_call(&mut self, name: &str, args: &str) {
        self.items.push(ChatItem::ToolCall {
            name: name.to_string(),
            args: args.to_string(),
            status: ToolStatus::Running,
            result: String::new(),
            ts: self.now_ts(),
            expanded: false,
        });
    }

    pub fn mark_tool_done(&mut self, name: &str, ok: bool) {
        for item in self.items.iter_mut().rev() {
            if let ChatItem::ToolCall { name: n, status, .. } = item {
                if n == name && *status == ToolStatus::Running {
                    *status = if ok { ToolStatus::Done } else { ToolStatus::Error };
                    break;
                }
            }
        }
    }

    pub fn find_last_tool_call_mut(&mut self, name: &str) -> Option<&mut ChatItem> {
        self.items.iter_mut().rev().find(|i| {
            matches!(i, ChatItem::ToolCall { name: n, .. } if n == name)
        })
    }

    pub fn toggle_last_expandable(&mut self) {
        for item in self.items.iter_mut().rev() {
            match item {
                ChatItem::ThinkingHeader { expanded, .. } => {
                    *expanded = !*expanded;
                    return;
                }
                ChatItem::SummaryHeader { expanded, .. } => {
                    *expanded = !*expanded;
                    return;
                }
                ChatItem::ToolCall { expanded, .. } => {
                    *expanded = !*expanded;
                    return;
                }
                ChatItem::AssistantText { expanded, content, .. } => {
                    if content.lines().count() > LONG_MSG_THRESHOLD {
                        *expanded = !*expanded;
                        return;
                    }
                }
                _ => continue,
            }
        }
    }

    pub fn add_ask_user(&mut self, question: &str, candidates: Vec<String>) {
        self.items.push(ChatItem::AskUserItem {
            question: question.to_string(),
            candidates,
            status: AskUserStatus::Pending,
            response: None,
        });
    }

    pub fn add_system(&mut self, text: &str) {
        self.items.push(ChatItem::SystemMessage {
            content: text.to_string(),
            ts: self.now_ts(),
        });
    }

    pub fn load_session_history(&mut self, session_id: &str) {
        let Some(entry) = self.session_store.get(session_id) else {
            return;
        };
        if entry.messages.is_empty() {
            return;
        }
        let count = entry.messages.len();
        self.items.push(ChatItem::SystemMessage {
            content: format!(
                "↻ Replayed {} message{} from previous session",
                count,
                if count == 1 { "" } else { "s" }
            ),
            ts: self.now_ts(),
        });
        for msg in &entry.messages {
            let role = msg.get("role").and_then(|v| v.as_str()).unwrap_or("");
            let raw_content = msg.get("content").and_then(|v| v.as_str()).unwrap_or("");
            let ts = msg
                .get("timestamp")
                .and_then(|v| v.as_str())
                .map(|s| {
                    chrono::DateTime::parse_from_rfc3339(s)
                        .map(|dt| dt.with_timezone(&chrono::Local).format("%H:%M:%S").to_string())
                        .unwrap_or_else(|_| s.to_string())
                })
                .unwrap_or_default();
            if raw_content.is_empty() {
                continue;
            }
            match role {
                "user" => {
                    self.items.push(ChatItem::UserMessage {
                        content: raw_content.to_string(),
                        ts,
                    });
                }
                "assistant" => {
                    self.items.push(ChatItem::AssistantText {
                        content: raw_content.to_string(),
                        ts,
                        expanded: true,
                    });
                }
                _ => {}
            }
        }
    }

    /// Build a chat history for the agent loop. The trailing
    /// `UserMessage` (if any) is excluded — the caller passes the
    /// current user prompt separately to `run_agent_loop`.
    pub fn to_message_history(&self) -> Vec<Message> {
        let items: &[ChatItem] = match self.items.last() {
            Some(ChatItem::UserMessage { .. }) => {
                &self.items[..self.items.len().saturating_sub(1)]
            }
            _ => &self.items[..],
        };
        items
            .iter()
            .filter_map(chat_item_to_message)
            .collect()
    }
}

fn chat_item_to_message(item: &ChatItem) -> Option<Message> {
    match item {
        ChatItem::UserMessage { content, .. } => Some(Message::user(content.clone())),
        ChatItem::AssistantText { content, .. } => {
            if content.trim().is_empty() {
                None
            } else {
                Some(Message::assistant(content.clone()))
            }
        }
        ChatItem::ToolCall { name, args, status, result, .. } => {
            let status_label = match status {
                ToolStatus::Running => "running",
                ToolStatus::Done => "ok",
                ToolStatus::Error => "failed",
            };
            let mut line = format!("[tool_call: {} ({}) → {}", name, status_label, args);
            if !result.is_empty() {
                line.push_str(&format!("\nresult: {}", result));
            }
            line.push(']');
            Some(Message::assistant(line))
        }
        _ => None,
    }
}

pub fn format_session_date(iso: &str) -> String {
    chrono::DateTime::parse_from_rfc3339(iso)
        .map(|dt| {
            let local = dt.with_timezone(&chrono::Local);
            local.format("%Y-%m-%d %H:%M").to_string()
        })
        .unwrap_or_else(|_| iso.to_string())
}

pub fn truncate_chars(s: &str, max_chars: usize) -> String {
    let n = s.chars().count();
    if n <= max_chars {
        return s.to_string();
    }
    let keep = max_chars.saturating_sub(1);
    let mut out: String = s.chars().take(keep).collect();
    out.push('…');
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_app() -> App {
        App::new(
            "/tmp".to_string(),
            "/tmp".to_string(),
            "config/mykey.toml".to_string(),
        )
    }

    #[test]
    fn to_message_history_excludes_trailing_user_message() {
        let mut app = make_app();
        app.items.push(ChatItem::UserMessage {
            content: "请写一个 HTML 俄罗斯方块".to_string(),
            ts: "10:00:00".to_string(),
        });
        app.items.push(ChatItem::ToolCall {
            name: "write".to_string(),
            args: r#"{"path":"/home/user/tetris.html"}"#.to_string(),
            status: ToolStatus::Error,
            result: r#"{"error":"write failed: No such file or directory"}"#.to_string(),
            ts: "10:00:01".to_string(),
            expanded: false,
        });
        app.items.push(ChatItem::AssistantText {
            content: "我在。请问有什么需要帮忙的？".to_string(),
            ts: "10:00:02".to_string(),
            expanded: true,
        });
        app.items.push(ChatItem::UserMessage {
            content: "上面那个文件，我打不开".to_string(),
            ts: "10:00:03".to_string(),
        });

        let history = app.to_message_history();

        assert_eq!(history.len(), 3);
        assert_eq!(history[0].role, oz_core_types::Role::User);
        assert_eq!(history[0].content_text(), "请写一个 HTML 俄罗斯方块");
        assert_eq!(history[1].role, oz_core_types::Role::Assistant);
        assert!(history[1].content_text().contains("write"));
        assert!(history[1].content_text().contains("failed"));
        assert_eq!(history[2].role, oz_core_types::Role::Assistant);
        assert_eq!(history[2].content_text(), "我在。请问有什么需要帮忙的？");
    }

    #[test]
    fn to_message_history_skips_system_and_thinking_items() {
        let mut app = make_app();
        app.items.push(ChatItem::SystemMessage {
            content: "agent starting".to_string(),
            ts: "10:00:00".to_string(),
        });
        app.items.push(ChatItem::UserMessage {
            content: "hi".to_string(),
            ts: "10:00:01".to_string(),
        });
        app.items.push(ChatItem::ThinkingBody {
            content: "secret reasoning".to_string(),
        });
        app.items.push(ChatItem::SummaryHeader {
            content: "plan".to_string(),
            expanded: false,
        });
        app.items.push(ChatItem::AssistantText {
            content: "hello back".to_string(),
            ts: "10:00:02".to_string(),
            expanded: true,
        });

        let history = app.to_message_history();

        assert_eq!(history.len(), 2);
        assert_eq!(history[0].content_text(), "hi");
        assert_eq!(history[1].content_text(), "hello back");
    }

    #[test]
    fn to_message_history_empty_when_no_items() {
        let app = make_app();
        assert!(app.to_message_history().is_empty());
    }
}
