//! Slash command parser + handler.
//!
//! When the user types `/`, we enter command mode. They type a
//! command (e.g. `/help`), press Enter, and we route to a handler
//! here. The list of known commands is the source of truth for
//! the autocompletion popup as well.

use crate::app::App;

const COMMANDS: &[&str] = &[
    "/help", "/h",
    "/sessions", "/s",
    "/session",
    "/rename",
    "/delete",
    "/model",
    "/theme",
    "/agent",
    "/clear",
    "/export",
    "/exit", "/quit",
];

/// Update `app.cmd_suggestions` based on what the user has typed
/// so far. The matching is intentionally loose (substring) so
/// `/h` matches `/help`.
pub fn update_suggestions(app: &mut App) {
    let typed = app.input.trim_start_matches('/').to_lowercase();
    if typed.is_empty() {
        app.cmd_suggestions = COMMANDS.to_vec();
    } else {
        app.cmd_suggestions = COMMANDS
            .iter()
            .filter(|cmd| {
                cmd.contains(&typed) || cmd.trim_start_matches('/').contains(&typed)
            })
            .copied()
            .collect();
    }
    app.cmd_selected = 0;
}

/// Handle a complete `/command arg` line. The leading `/` is
/// optional in the input.
pub async fn handle(app: &mut App, input: &str) {
    let parts: Vec<&str> = input.trim().splitn(2, ' ').collect();
    let cmd = parts[0].to_lowercase();
    let arg = parts.get(1).map(|s| s.trim()).unwrap_or("");

    match cmd.as_str() {
        "/exit" | "/quit" => {
            if app.is_processing {
                app.confirm_quit = true;
                app.status = "Agent is running — confirm quit? (y/N)".into();
            } else {
                app.should_quit = true;
            }
        }
        "/help" | "/h" => {
            app.add_system(
                "Commands:\n\
                 /help, /h          — Show this help\n\
                 /sessions, /s      — List all sessions\n\
                 /session <name>    — Switch to a session\n\
                 /session new       — Create new session\n\
                 /rename <name>     — Rename current session\n\
                 /delete            — Delete current session\n\
                 /model <name>      — Switch model\n\
                 /theme <light|dark> — Toggle TUI theme\n\
                 /agent <name>      — Select agent\n\
                 /clear             — Clear chat history\n\
                 /export            — Export session as JSON\n\
                 /exit, /quit       — Quit OpenZen",
            );
            app.status = "type to chat · / for commands · /exit to quit".into();
        }
        "/sessions" | "/s" => {
            let sessions = app.session_store.list();
            if sessions.is_empty() {
                app.add_system("No sessions.");
            } else {
                let mut msg = "Sessions:".to_string();
                for (i, s) in sessions.iter().enumerate() {
                    let marker = if Some(&s.id) == app.current_id.as_ref() {
                        "  ● "
                    } else {
                        "    "
                    };
                    let date = crate::app::format_session_date(&s.created_at);
                    msg.push_str(&format!(
                        "\n{}{}. {}  {}  {} msgs",
                        marker,
                        i + 1,
                        s.name,
                        date,
                        s.message_count
                    ));
                }
                msg.push_str("\n\nUse /session <number or name> to switch, /session new to create.");
                app.add_system(&msg);
            }
            app.status = "type to chat · / for commands · /exit to quit".into();
        }
        "/session" => {
            if arg.is_empty() {
                app.add_system("Usage: /session <name or number> or /session new");
                return;
            }
            if arg == "new" {
                let info = app.session_store.create("New chat");
                app.current_id = Some(info.id.clone());
                app.session_id_for_run = info.id.clone();
                app.items.clear();
                app.input.clear();
                app.add_system(&format!("Created new session: {}", info.name));
            } else {
                let sessions = app.session_store.list();
                let target = if let Ok(num) = arg.parse::<usize>() {
                    if num > 0 && num <= sessions.len() {
                        Some(sessions[num - 1].clone())
                    } else {
                        None
                    }
                } else {
                    sessions
                        .into_iter()
                        .find(|s| s.name.contains(arg) || s.id.contains(arg))
                };
                match target {
                    Some(info) => {
                        app.current_id = Some(info.id.clone());
                        app.session_id_for_run = info.id.clone();
                        app.items.clear();
                        app.add_system(&format!("Switched to session: {}", info.name));
                    }
                    None => app.add_system(&format!("Session '{}' not found.", arg)),
                }
            }
            app.status = "type to chat · / for commands · /exit to quit".into();
        }
        "/rename" => {
            if arg.is_empty() {
                app.add_system("Usage: /rename <new name>");
                return;
            }
            if let Some(id) = app.current_id.clone() {
                app.session_store.rename(&id, arg);
                app.add_system(&format!("Session renamed to: {}", arg));
            }
            app.status = "type to chat · / for commands · /exit to quit".into();
        }
        "/delete" => {
            if app.is_processing {
                app.add_system("Cannot delete while agent is running. Stop it first.");
                return;
            }
            app.confirm_delete = true;
            app.status = "Delete current session? (y/n)".into();
        }
        "/model" => {
            if arg.is_empty() {
                app.add_system(&format!("Current model: {}", app.model_name));
            } else {
                app.model_name = arg.to_string();
                app.add_system(&format!("Model set to: {}", arg));
            }
            app.status = "type to chat · / for commands · /exit to quit".into();
        }
        "/theme" => {
            if arg == "light" {
                app.theme = crate::theme::Theme::light();
                app.add_system("Theme: light (high contrast)");
            } else {
                app.theme = crate::theme::Theme::dark();
                app.add_system("Theme: dark (Song Dynasty)");
            }
            app.status = "type to chat · / for commands · /exit to quit".into();
        }
        "/agent" => {
            if arg.is_empty() {
                let dir = oz_agent::agents_dir();
                match oz_agent::Agent::list(&dir) {
                    Ok(names) => {
                        if names.is_empty() {
                            app.add_system("No agents found. Create one at ~/.openzen/agents/<name>/config.yaml");
                        } else {
                            let mut msg = "Available agents:".to_string();
                            for name in &names {
                                let marker = if Some(name) == app.current_agent.as_ref() { "  ● " } else { "    " };
                                msg.push_str(&format!("\n{}{}", marker, name));
                            }
                            msg.push_str("\n\nUse /agent <name> to select an agent.");
                            app.add_system(&msg);
                        }
                    }
                    Err(_) => app.add_system("Failed to list agents."),
                }
            } else {
                let dir = oz_agent::agents_dir();
                match oz_agent::Agent::load(arg, &dir) {
                    Ok(agent) => {
                        app.current_agent = Some(agent.name.clone());
                        let inst = agent.config.instructions.clone().unwrap_or_default();
                        if !inst.is_empty() {
                            app.system_prompt = inst.clone();
                        }
                        app.add_system(&format!("Agent set to: {}.", agent.name));
                        if !agent.config.use_tools.as_ref().is_none_or(|s| s.is_empty()) {
                            app.add_system(&format!("  Tools: {}", agent.config.use_tools.unwrap_or_default()));
                        }
                        if !agent.config.documents.is_empty() {
                            app.add_system(&format!("  Documents: {}", agent.config.documents.join(", ")));
                        }
                    }
                    Err(e) => app.add_system(&format!("Failed to load agent '{}': {}", arg, e)),
                }
            }
            app.status = "type to chat · / for commands · /exit to quit".into();
        }
        "/clear" => {
            app.items.clear();
            app.chat_scroll = 0;
            app.add_system("Chat history cleared.");
            app.status = "type to chat · / for commands · /exit to quit".into();
        }
        "/export" => {
            app.add_system("Export not implemented yet.");
            app.status = "type to chat · / for commands · /exit to quit".into();
        }
        _ => {
            app.add_system(&format!(
                "Unknown command: {}. Type /help for available commands.",
                cmd
            ));
            app.status = "type to chat · / for commands · /exit to quit".into();
        }
    }
}
