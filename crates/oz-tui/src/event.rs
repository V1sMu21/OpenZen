//! Stream event ingestion, key handling, and the agent-loop bridge.
//!
//! The agent loop produces `oz_core_types::StreamEvent`s. We plumb
//! those through an unbounded mpsc and apply them to `App` state
//! here, on the UI task. Keeping this in one place means the
//! renderer doesn't need to know about the agent loop at all.
//!
//! This consumer speaks only the typed start-delta-end protocol
//! events (`TextStart`/`TextDelta`/`TextEnd`, `ReasoningStart`/...,
//! `ToolInputStart`/.../`ToolInputAvailable`/`ToolOutputAvailable`,
//! `StartStep`/`FinishStep`/`FinishMessage`). Tag-stripping
//! heuristics are no longer needed.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use crossterm::event::{KeyCode, KeyModifiers};
use oz_core::handler::LoopConfig;
use oz_core_types::{Message, StreamEvent};
use oz_server::webui::sessions::SessionStatus;
use oz_tools::handler::ToolRegistryHandler;
use oz_tools::registry::ToolRegistry;
use tokio::sync::mpsc;

use crate::app::{App, AskUserStatus, ChatItem, InputMode, ToolStatus};

/// Channel rx half: drives the agent loop's output into the UI.
pub type StreamRx = mpsc::UnboundedReceiver<StreamEvent>;

/// Spawn the agent loop on a background task.
///
/// Returns the receiver the UI should poll, and the stop signal
/// used to cancel the loop if the user presses 's'.
#[allow(clippy::field_reassign_with_default)]
pub fn spawn_agent_loop(
    app: &App,
    prompt: String,
    additional_messages: Vec<Message>,
) -> (StreamRx, Arc<AtomicBool>) {
    use oz_config::mykey::SessionType;

    let (tx, rx) = mpsc::unbounded_channel::<StreamEvent>();

    // Resolve session + tool registry
    let cfg = match oz_config::mykey::MyKeyConfig::from_file(
        std::path::PathBuf::from(&app.config_path).as_path(),
    ) {
        Ok(c) => c,
        Err(e) => {
            tracing::error!("config error: {e}");
            // Return a closed channel so the UI sees no events.
            let (_tx, rx) = mpsc::unbounded_channel::<StreamEvent>();
            return (rx, Arc::new(AtomicBool::new(false)));
        }
    };
    let session_name = cfg.default_session.as_deref().unwrap_or("claude_sonnet");
    let sess_config = match cfg.get(session_name) {
        Some(c) => c.clone(),
        None => {
            tracing::error!("session '{session_name}' not found");
            let (_tx, rx) = mpsc::unbounded_channel::<StreamEvent>();
            return (rx, Arc::new(AtomicBool::new(false)));
        }
    };
    let sess_type = cfg.session_type(session_name);

    let ctx = oz_core_types::ToolContext {
        working_dir: app.working_dir.clone(),
        assets_dir: app.assets_dir.clone(),
        script_dir: app.assets_dir.clone(),
        lang: std::env::var("OZ_LANG").unwrap_or_default(),
        skill_mcp_dir: None,
        harness_dir: None,
        session_id: String::new(),
    };

    let backend: Box<dyn oz_llm::Session> = match sess_type {
        SessionType::Claude => Box::new(oz_llm::ClaudeSession::new(sess_config.clone())),
        SessionType::Oai => Box::new(oz_llm::OaiSession::new(sess_config.clone())),
        SessionType::NativeClaude => {
            Box::new(oz_llm::NativeClaudeSession::new(sess_config.clone()))
        }
        SessionType::NativeOai => Box::new(oz_llm::NativeOAISession::new(sess_config.clone())),
        SessionType::Mixin => {
            tracing::error!("Mixin session not supported in TUI");
            let (_tx, rx) = mpsc::unbounded_channel::<StreamEvent>();
            return (rx, Arc::new(AtomicBool::new(false)));
        }
    };
    let mut client = oz_llm::NativeToolClient::new(backend);

    let registry = ToolRegistry::build_default();
    let definitions = registry.to_schema("en");
    let mut handler = ToolRegistryHandler::new(registry);

    let stop_signal = Arc::new(AtomicBool::new(false));
    let stop_signal_clone = stop_signal.clone();

    let mut loop_config = LoopConfig::default();
    loop_config.max_turns = 30;
    loop_config.event_tx = Some(tx.clone());
    loop_config.session_id = app.session_id_for_run.clone();
    loop_config.working_dir = app.working_dir.clone();

    let system_prompt = app.system_prompt.clone();

    tokio::spawn(async move {
        let outcome = oz_core::agent_loop::run_agent_loop(
            &mut client,
            system_prompt,
            prompt,
            additional_messages,
            &mut handler,
            &definitions,
            &ctx,
            &loop_config,
            &stop_signal_clone,
        )
        .await;

        let _ = tx.send(StreamEvent::FinishMessage {
            stop_reason: outcome.exit_reason.clone(),
        });
    });

    (rx, stop_signal)
}

// ── Stream event handling ──

/// Apply one stream event to App state. This is the *only* place
/// that mutates `App.items` in response to the agent.
pub fn handle_stream_event(app: &mut App, evt: StreamEvent) {
    match evt {
        StreamEvent::TextStart { .. } => {
            app.items.push(ChatItem::AssistantText {
                content: String::new(),
                ts: app.now_ts(),
                expanded: true,
            });
        }
        StreamEvent::TextDelta { text, .. } => {
            if !text.is_empty() {
                append_assistant_text(app, &text);
            }
        }
        StreamEvent::TextEnd { .. } => {}

        StreamEvent::ReasoningStart { .. } => {
            app.items.push(ChatItem::ThinkingHeader {
                duration: String::new(),
                words: 0,
                expanded: false,
            });
            app.items.push(ChatItem::ThinkingBody {
                content: String::new(),
            });
        }
        StreamEvent::ReasoningDelta { text, .. } => {
            if text.is_empty() {
                return;
            }
            if let Some(ChatItem::ThinkingBody { content, .. }) = app.items.last_mut() {
                content.push_str(&text);
            } else {
                app.items.push(ChatItem::ThinkingBody { content: text });
            }
        }
        StreamEvent::ReasoningEnd { .. } => {}

        StreamEvent::ToolInputStart { name, .. } => {
            if name == "respond" || name == "ask_user" {
                return;
            }
            app.current_tool_name = name.clone();
            app.add_tool_call(&name, "");
        }
        StreamEvent::ToolInputDelta { delta, .. } => {
            if delta.is_empty() {
                return;
            }
            if let Some(ChatItem::ToolCall { args, .. }) = app.items.iter_mut().rev().find(
                |i| matches!(i, ChatItem::ToolCall { status: s, .. } if *s == ToolStatus::Running),
            ) {
                args.push_str(&delta);
            }
        }
        StreamEvent::ToolInputAvailable { name, args, .. } => {
            if name == "respond" || name == "ask_user" {
                return;
            }
            if let Some(ChatItem::ToolCall { args: a, .. }) = app.items.iter_mut().rev().find(|i| {
                matches!(i, ChatItem::ToolCall { name: n, status: ToolStatus::Running, .. } if *n == name)
            }) {
                *a = args.clone();
            }
        }
        StreamEvent::ToolOutputAvailable { name, output, .. } => {
            if name == "ask_user" {
                if let Ok(parsed) = serde_json::from_str::<serde_json::Value>(&output) {
                    let inner = parsed.get("data").unwrap_or(&parsed);
                    if let Some(q) = inner.get("question").and_then(|v| v.as_str()) {
                        let cands: Vec<String> = inner
                            .get("candidates")
                            .and_then(|v| v.as_array())
                            .map(|arr| {
                                arr.iter()
                                    .filter_map(|v| v.as_str().map(String::from))
                                    .collect()
                            })
                            .unwrap_or_default();
                        app.add_ask_user(q, cands.clone());
                        app.pending_ask_user = Some((q.to_string(), cands));
                        app.input_mode = InputMode::AskUser;
                        app.status =
                            "Ask user: ↑/↓ to pick candidate, Enter to send, Esc to dismiss".into();
                        app.mark_tool_done("ask_user", true);
                        return;
                    }
                }
            }
            // For write/edit, surface a short summary line into the
            // ToolCall args preview so the user sees what was changed.
            if (name == "write" || name == "edit" || name == "patch") && output.len() < 200 {
                if let Some(ChatItem::ToolCall { args, .. }) = app.items.iter_mut().rev().find(|i| {
                    matches!(i, ChatItem::ToolCall { name: n, status: ToolStatus::Running, .. } if n == &name)
                }) {
                    let first_line = output.lines().next().unwrap_or("").to_string();
                    if first_line.len() < 100 {
                        args.push_str(&format!("  ⟶ {}", first_line));
                    }
                }
            }
            if let Some(ChatItem::ToolCall { result: r, .. }) = app.find_last_tool_call_mut(&name) {
                if r.is_empty() {
                    *r = output;
                }
            }
            app.mark_tool_done(&name, true);
            if app.current_tool_name == name {
                app.current_tool_name.clear();
            }
        }

        StreamEvent::StartStep {} | StreamEvent::FinishStep {} => {}
        StreamEvent::FinishMessage { stop_reason } => {
            app.is_processing = false;
            app.stop_signal = None;
            app.started_at = None;
            app.input_mode = InputMode::Editing;
            app.current_tool_name.clear();
            let reason_display = match stop_reason.as_str() {
                "stopped_by_user" => {
                    app.add_system(
                        "Task stopped by user. Checkpoint saved — you can resume later.",
                    );
                    "Stopped by user".to_string()
                }
                r => r.to_string(),
            };
            app.status = format!("Done ({reason_display}) · type to continue, /exit to quit");
            app.session_store.save();
        }

        StreamEvent::Error { message } => {
            app.is_processing = false;
            app.stop_signal = None;
            app.started_at = None;
            app.input_mode = InputMode::Editing;
            app.add_system(&format!("ERROR: {}", message));
            app.last_error = Some(message);
            app.status = "Error — type to retry, /exit to quit".into();
        }
        StreamEvent::ToolCallReady { .. } => {
            // Internal event for speculative execution; not rendered in TUI.
        }
        StreamEvent::AskUserPending { data } => {
            if let Ok(parsed) = serde_json::from_str::<serde_json::Value>(&data) {
                let q = parsed["payload"]["data"]["question"].as_str().unwrap_or("");
                if !q.is_empty() {
                    app.add_system(&format!("[ask_user] {}", q));
                }
            }
        }
        StreamEvent::DataCompressingContext { .. } => {
            // Transient frontend notification; not rendered in TUI.
        }
        StreamEvent::DataTodoUpdate { .. } => {
            // Todo tracking handled by frontend UI; not rendered in TUI.
        }
        StreamEvent::OpenArtifact { .. } => {}
        StreamEvent::UserIntervention { .. } => {}
        StreamEvent::DataContextUsage { .. } => {
            // Context bar usage update; handled by frontend, not rendered in TUI.
        }
    }
}

fn append_assistant_text(app: &mut App, chunk: &str) {
    if chunk.is_empty() {
        return;
    }
    // Only merge into the *trailing* AssistantText — never across a
    // tool call, user message, or other intermediate item. Each
    // agent segment (between tool calls or across turns) gets its
    // own block.
    if let Some(ChatItem::AssistantText { content, .. }) = app.items.iter_mut().last() {
        content.push_str(chunk);
    } else {
        app.items.push(ChatItem::AssistantText {
            content: chunk.to_string(),
            ts: app.now_ts(),
            expanded: true,
        });
    }
}

// ── Key handling ──

pub async fn handle_key(
    app: &mut App,
    history: &mut crate::editor::History,
    key: crossterm::event::KeyEvent,
) {
    use crate::command;

    let code = key.code;
    let mods = key.modifiers;

    // Global: Ctrl+C quits
    if code == KeyCode::Char('c') && mods.contains(KeyModifiers::CONTROL) {
        app.should_quit = true;
        return;
    }

    // Global: PageUp / PageDown scroll the chat history in any input mode.
    const SCROLL_PAGE: usize = 10;
    match code {
        KeyCode::PageUp => {
            app.chat_scroll = app.chat_scroll.saturating_sub(SCROLL_PAGE);
            app.follow_tail = false;
            return;
        }
        KeyCode::PageDown => {
            app.chat_scroll = app.chat_scroll.saturating_add(SCROLL_PAGE);
            app.follow_tail = false;
            return;
        }
        KeyCode::Home => {
            app.chat_scroll = 0;
            app.follow_tail = false;
            return;
        }
        KeyCode::End => {
            app.follow_tail = true;
            return;
        }
        _ => {}
    }

    match app.input_mode {
        InputMode::AskUser => match code {
            KeyCode::Esc => {
                app.input_mode = InputMode::Editing;
                app.pending_ask_user = None;
                app.status = "ask_user dismissed".into();
            }
            KeyCode::Up if app.cmd_selected > 0 => {
                app.cmd_selected -= 1;
            }
            KeyCode::Down => {
                if let Some((_, cands)) = app.pending_ask_user.clone() {
                    if app.cmd_selected + 1 < cands.len() {
                        app.cmd_selected += 1;
                    }
                    let _ = cands;
                }
            }
            KeyCode::Enter => {
                if let Some((_, cands)) = app.pending_ask_user.clone() {
                    let response = if cands.is_empty() {
                        app.input.trim().to_string()
                    } else {
                        let idx = app.cmd_selected.min(cands.len().saturating_sub(1));
                        cands[idx].clone()
                    };
                    if !response.is_empty() {
                        if let Some(ChatItem::AskUserItem {
                            status,
                            response: r,
                            ..
                        }) = app.items.iter_mut().rev().find(|i| {
                            matches!(
                                i,
                                ChatItem::AskUserItem {
                                    status: AskUserStatus::Pending,
                                    ..
                                }
                            )
                        }) {
                            *status = AskUserStatus::Answered;
                            *r = Some(response.clone());
                        }
                        app.pending_ask_user = None;
                        app.input_mode = InputMode::Editing;
                        app.input.clear();
                        app.cmd_selected = 0;
                        app.add_user_message(&format!("[ask_user answer] {}", response));
                        history.append(&format!("[ask_user answer] {}", response));
                        start_run(app, response).await;
                    }
                }
            }
            KeyCode::Char(c) => {
                app.input.push(c);
            }
            KeyCode::Backspace => {
                app.input.pop();
            }
            _ => {}
        },
        InputMode::Editing => {
            // Confirmation prompts take precedence over everything
            // else, including typing.
            if app.confirm_quit {
                match code {
                    KeyCode::Char('y') | KeyCode::Char('Y') => {
                        app.confirm_quit = false;
                        app.should_quit = true;
                        return;
                    }
                    KeyCode::Char('n') | KeyCode::Char('N') | KeyCode::Esc => {
                        app.confirm_quit = false;
                        app.status = "Resume typing...".into();
                        return;
                    }
                    _ => {}
                }
            }
            if app.confirm_delete {
                match code {
                    KeyCode::Char('y') | KeyCode::Char('Y') => {
                        app.confirm_delete = false;
                        if let Some(id) = app.current_id.clone() {
                            app.session_store.delete(&id);
                            if let Some(next) = app.session_store.list().first().cloned() {
                                app.current_id = Some(next.id.clone());
                                app.session_id_for_run = next.id.clone();
                            } else {
                                let info = app.session_store.create("New chat");
                                app.current_id = Some(info.id.clone());
                                app.session_id_for_run = info.id.clone();
                            }
                            app.items.clear();
                            app.add_system("Session deleted.");
                        }
                        return;
                    }
                    KeyCode::Char('n') | KeyCode::Char('N') | KeyCode::Esc => {
                        app.confirm_delete = false;
                        app.status = "Delete cancelled.".into();
                        return;
                    }
                    _ => {}
                }
            }

            // Navigation keybinds only fire when the input box is
            // empty — otherwise the keypress is treated as text.
            if app.input.is_empty() && !app.cmd_mode {
                match code {
                    KeyCode::Char('s') => {
                        if app.is_processing {
                            if let Some(sig) = &app.stop_signal {
                                sig.store(true, Ordering::SeqCst);
                                app.status =
                                    "Stop signal sent. Waiting for current tool to finish..."
                                        .into();
                            }
                        }
                        return;
                    }
                    KeyCode::Char('n') => {
                        if !app.is_processing {
                            let info = app.session_store.create("New chat");
                            app.current_id = Some(info.id.clone());
                            app.session_id_for_run = info.id.clone();
                            app.items.clear();
                            app.input.clear();
                            app.add_system(&format!("Created new session: {}", info.name));
                        }
                        return;
                    }
                    KeyCode::Char('G') => {
                        app.follow_tail = true;
                        return;
                    }
                    KeyCode::Char('g') => {
                        app.chat_scroll = 0;
                        app.follow_tail = false;
                        return;
                    }
                    KeyCode::Char(' ') => {
                        app.toggle_last_expandable();
                        return;
                    }
                    KeyCode::Char('/') => {
                        app.input.clear();
                        app.input.push('/');
                        app.cmd_mode = true;
                        app.cmd_buffer.clear();
                        app.cmd_selected = 0;
                        command::update_suggestions(app);
                        app.status = "Type a command...".into();
                        return;
                    }
                    _ => {}
                }
            }

            match code {
                KeyCode::Esc => {
                    app.cmd_mode = false;
                    app.cmd_buffer.clear();
                    app.cmd_suggestions.clear();
                    app.status = "Resume typing...".into();
                }
                KeyCode::Tab if app.cmd_mode && !app.cmd_suggestions.is_empty() => {
                    let sel = app
                        .cmd_selected
                        .min(app.cmd_suggestions.len().saturating_sub(1));
                    let completion = app.cmd_suggestions[sel].trim_start_matches('/');
                    app.input = format!("/{} ", completion);
                    app.cmd_suggestions.clear();
                }
                KeyCode::Up
                    if app.cmd_mode && !app.cmd_suggestions.is_empty() && app.cmd_selected > 0 =>
                {
                    app.cmd_selected -= 1;
                }
                KeyCode::Down
                    if app.cmd_mode
                        && !app.cmd_suggestions.is_empty()
                        && app.cmd_selected + 1 < app.cmd_suggestions.len() =>
                {
                    app.cmd_selected += 1;
                }
                KeyCode::Up => {
                    recall_history(app, history, true);
                }
                KeyCode::Down => {
                    recall_history(app, history, false);
                }
                KeyCode::Enter => {
                    let text = app.input.trim().to_string();
                    if text.is_empty() {
                        return;
                    }
                    if app.is_processing {
                        app.input = text;
                        app.status =
                            "Agent is still running — press 's' + Enter to stop, or wait for it to finish."
                                .into();
                        return;
                    }
                    app.input.clear();
                    *history.cursor_mut() = None;

                    if app.cmd_mode {
                        app.cmd_mode = false;
                        app.cmd_suggestions.clear();
                        command::handle(app, &text).await;
                    } else {
                        history.append(&text);
                        app.add_user_message(&text);
                        start_run(app, text).await;
                    }
                }
                KeyCode::Backspace => {
                    app.input.pop();
                    if app.cmd_mode {
                        command::update_suggestions(app);
                    } else {
                        *history.cursor_mut() = None;
                    }
                }
                KeyCode::Char(c) => {
                    app.input.push(c);
                    if app.cmd_mode {
                        command::update_suggestions(app);
                    } else {
                        *history.cursor_mut() = None;
                    }
                }
                _ => {}
            }
        }
    }
}

fn recall_history(app: &mut App, history: &mut crate::editor::History, up: bool) {
    if app.is_processing {
        return;
    }
    let cur = history.cursor();
    let next = match (cur, up) {
        (None, true) => 0,
        (None, false) => {
            // Not in recall mode; DOWN is a no-op.
            return;
        }
        (Some(0), false) => {
            // At the most-recent recalled line; DOWN clears the input
            // and exits recall mode (standard readline/bash behaviour).
            app.input.clear();
            *history.cursor_mut() = None;
            return;
        }
        (Some(n), true) => n + 1,
        (Some(n), false) => n - 1,
    };
    if let Some(line) = history.lookup(next) {
        app.input = line;
        *history.cursor_mut() = Some(next);
    }
}

pub async fn start_run(app: &mut App, prompt: String) {
    // Persist user message
    let session_id = if let Some(id) = &app.current_id {
        id.clone()
    } else {
        let info = app.session_store.create("New chat");
        app.current_id = Some(info.id.clone());
        info.id
    };
    app.session_id_for_run = session_id.clone();

    if let Some(s) = app.session_store.get_mut(&session_id) {
        s.messages.push(serde_json::json!({
            "role": "user",
            "content": prompt,
            "timestamp": chrono::Utc::now().to_rfc3339(),
        }));
        s.status = SessionStatus::Running;
    }
    app.session_store.save();

    // Resolve config to update model display
    if let Ok(cfg) = oz_config::mykey::MyKeyConfig::from_file(
        std::path::PathBuf::from(&app.config_path).as_path(),
    ) {
        let session_name = cfg.default_session.as_deref().unwrap_or("claude_sonnet");
        if let Some(sess_config) = cfg.get(session_name) {
            app.model_name = sess_config.model.clone();
            let sess_type = cfg.session_type(session_name);
            app.model_provider = match sess_type {
                oz_config::mykey::SessionType::Claude
                | oz_config::mykey::SessionType::NativeClaude => "claude",
                oz_config::mykey::SessionType::Oai | oz_config::mykey::SessionType::NativeOai => {
                    "openai"
                }
                oz_config::mykey::SessionType::Mixin => "mixin",
            }
            .to_string();
        }
    }

    let history = app.to_message_history();

    let (rx, stop_signal) = spawn_agent_loop(app, prompt, history);
    app.event_rx = Some(rx);
    app.is_processing = true;
    app.started_at = Some(std::time::Instant::now());
    // Stay in Editing mode so the user can type the next message
    // or /exit while the agent runs.
    app.input_mode = InputMode::Editing;
    app.status = "Agent is running... (type 's' + Enter to stop, or /exit to quit)".into();
    app.current_tool_name.clear();
    app.stop_signal = Some(stop_signal);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::InputMode;
    use crossterm::event::{KeyCode, KeyModifiers};

    fn make_app() -> App {
        App::new(
            "/tmp".to_string(),
            "/tmp".to_string(),
            "config/mykey.toml".to_string(),
        )
    }

    #[tokio::test]
    async fn enter_while_processing_restores_input_and_warns() {
        let mut app = make_app();
        let mut hist = crate::editor::History::default();
        app.input = "上面那个文件，我打不开".to_string();
        app.is_processing = true;
        app.input_mode = InputMode::Editing;
        let prior_status = app.status.clone();

        let key = crossterm::event::KeyEvent::new(KeyCode::Enter, KeyModifiers::empty());
        handle_key(&mut app, &mut hist, key).await;

        assert_eq!(app.input, "上面那个文件，我打不开");
        assert_ne!(app.status, prior_status);
        assert!(
            app.status.contains("running") || app.status.contains("wait"),
            "status should explain the agent is still busy, got: {}",
            app.status
        );
        assert!(app.is_processing, "processing flag must remain true");
    }

    #[tokio::test]
    async fn enter_with_empty_input_does_nothing() {
        let mut app = make_app();
        let mut hist = crate::editor::History::default();
        app.is_processing = false;
        app.input_mode = InputMode::Editing;
        app.input = "   ".to_string();
        let items_before = app.items.len();

        let key = crossterm::event::KeyEvent::new(KeyCode::Enter, KeyModifiers::empty());
        handle_key(&mut app, &mut hist, key).await;

        assert_eq!(app.items.len(), items_before);
        assert!(!app.is_processing);
    }
}
