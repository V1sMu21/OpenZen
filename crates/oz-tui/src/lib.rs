//! OpenZen TUI — ratatui-based terminal UI for the agent loop.
//!
//! This crate is split into focused modules:
//!
//! - `app`     — `App` state, `ChatItem` enum, helpers
//! - `event`   — StreamEvent ingestion, key handling, agent-loop spawn
//! - `chat`    — chat pane rendering (the tool-call overflow fix lives here)
//! - `input`   — input bar
//! - `header`  — top header (logo + tagline)
//! - `command` — slash command parser + handlers
//! - `ask_user`— ask_user dialog modal
//! - `theme`   — colour palette + tool emoji mapping
//!
//! `lib.rs` itself stays a thin entry: it sets up the terminal,
//! loads config + system prompt, runs the main event loop, and
//! dispatches to the per-module renderers.
//!
//! Note: the legacy `content` and `narration` modules (heuristic
//! `<thinking>` / `<summary>` tag stripping and tool-call
//! narration-block filtering) were removed after the stream-protocol
//! migration (Phase 0.3). The TUI now consumes typed start-delta-end
//! events directly from the LLM layer.

use std::path::PathBuf;
use std::time::Duration;

use crossterm::event::{
    self as ctm_event, DisableMouseCapture, EnableMouseCapture, Event, KeyEventKind, MouseEventKind,
};
use crossterm::execute;
use crossterm::terminal::{
    disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen,
};
use oz_core_types::StreamEvent;
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, Paragraph};
use ratatui::{Frame, Terminal};

pub mod app;
pub mod ask_user;
pub mod chat;
pub mod command;
pub mod editor;
pub mod event;
pub mod header;
pub mod input;
pub mod markdown;
pub mod template;
pub mod theme;

use crate::app::{App, InputMode};
use crate::theme::*;

// ── Public entry point ──

/// Run the TUI until the user quits.
///
/// `sess_config` and `sess_type` are the *initial* config; the
/// actual model and provider used for each run are reloaded from
/// `config_path` at run time (so `/model` and config edits take
/// effect without restarting the TUI).
pub async fn run_tui(
    sess_config: oz_config::SessionConfig,
    sess_type: oz_config::mykey::SessionType,
    assets_dir: &str,
    working_dir: &str,
    config_path: &str,
) -> anyhow::Result<()> {
    // Eagerly validate the requested session type by constructing
    // the backend. We don't keep the instance around — the agent
    // loop constructs its own client on each run.
    let _backend: Box<dyn oz_llm::Session> = match sess_type {
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
            anyhow::bail!("Mixin session not supported in TUI")
        }
    };

    enable_raw_mode()?;
    let mut stdout = std::io::stdout();
    execute!(stdout, EnterAlternateScreen, EnableMouseCapture)?;
    let backend_term = ratatui::backend::CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend_term)?;
    terminal.clear()?;

    // ── App state ──
    let mut app = App::new(
        working_dir.to_string(),
        assets_dir.to_string(),
        config_path.to_string(),
    );

    // Load `[tui] left_prompt` / `right_prompt` from mykey.toml.
    if let Ok(cfg) = oz_config::mykey::MyKeyConfig::from_file(std::path::Path::new(config_path)) {
        if let Some(raw) = cfg.tui.left_prompt {
            app.left_prompt = Some(crate::template::PromptTemplate::new(raw));
        }
        if let Some(raw) = cfg.tui.right_prompt {
            app.right_prompt = Some(crate::template::PromptTemplate::new(raw));
        }
        if let Some("light") = cfg.tui.theme.as_deref() {
            app.theme = crate::theme::Theme::light()
        }
        let cfg_ref = crate::theme::ThemeConfig {
            user_fg: cfg.tui.theme_overrides.user_fg,
            agent_fg: cfg.tui.theme_overrides.agent_fg,
            muted_fg: cfg.tui.theme_overrides.muted_fg,
            accent_fg: cfg.tui.theme_overrides.accent_fg,
            highlight_fg: cfg.tui.theme_overrides.highlight_fg,
        };
        app.theme = crate::theme::Theme::from_config(&cfg_ref);
    }

    // Load system prompt (Chinese by default, English if GA_LANG=en)
    let lang = std::env::var("OZ_LANG").unwrap_or_default();
    let sys_prompt_filename = if lang == "en" {
        "sys_prompt_en.txt"
    } else {
        "sys_prompt.txt"
    };
    let sys_prompt_path = PathBuf::from(assets_dir).join(sys_prompt_filename);
    app.system_prompt = if sys_prompt_path.exists() {
        std::fs::read_to_string(&sys_prompt_path).unwrap_or_default()
    } else {
        String::new()
    };
    if app.system_prompt.is_empty() {
        app.add_system("(system prompt not found — agent will run without it)");
    }

    // Always start a fresh empty session on launch — never auto-replay
    // the previous session's messages into the chat pane. The session
    // store still persists across launches (so /sessions works), but
    // the in-memory chat starts blank.
    let info = app.session_store.create("New chat");
    app.current_id = Some(info.id.clone());
    app.session_id_for_run = info.id.clone();
    app.items.clear();

    app.add_system(
        "OpenZen TUI ready. Type to chat — '/exit' or Ctrl+C to quit. ↑/↓ recall history.",
    );

    let mut history = crate::editor::History::default();

    // ── Main loop ──
    while !app.should_quit {
        app.frame_count = app.frame_count.wrapping_add(1);

        if let Err(e) = terminal.draw(|f| ui(f, &mut app)) {
            tracing::error!("TUI draw error: {e}");
            tokio::time::sleep(Duration::from_millis(100)).await;
            continue;
        }

        // Drain stream events from the agent loop
        let mut events: Vec<StreamEvent> = Vec::new();
        if let Some(ref mut rx) = app.event_rx {
            while let Ok(evt) = rx.try_recv() {
                events.push(evt);
            }
        }
        for evt in events {
            event::handle_stream_event(&mut app, evt);
        }

        // Poll keyboard + mouse
        match ctm_event::poll(Duration::from_millis(100)) {
            Ok(true) => {
                if let Ok(ev) = ctm_event::read() {
                    match ev {
                        Event::Key(key) if key.kind == KeyEventKind::Press => {
                            event::handle_key(&mut app, &mut history, key).await;
                        }
                        Event::Mouse(m) => {
                            match m.kind {
                                MouseEventKind::ScrollUp => {
                                    // Scroll chat up by 3 lines
                                    app.chat_scroll = app.chat_scroll.saturating_add(3);
                                    app.follow_tail = false;
                                }
                                MouseEventKind::ScrollDown => {
                                    if app.follow_tail {
                                        // already at bottom; no-op
                                    } else {
                                        app.chat_scroll = app.chat_scroll.saturating_sub(3);
                                        if app.chat_scroll == 0 {
                                            app.follow_tail = true;
                                        }
                                    }
                                }
                                _ => {}
                            }
                        }
                        _ => {}
                    }
                }
            }
            Ok(false) => {}
            Err(e) => {
                tracing::error!("TUI event poll error: {e}");
            }
        }
    }

    history.save();

    // ── Teardown ──
    disable_raw_mode()?;
    execute!(std::io::stdout(), DisableMouseCapture, LeaveAlternateScreen)?;
    Ok(())
}

// ── Top-level layout ──

/// Top-level UI composition. Splits the screen into:
///   header  (5 lines: FIGlet "OpenZen" + "Work Less, Imagine More")
///   chat    (fills the middle)
///   ────────────  full-width horizontal frame line
///   input   (1 line)
///   ────────────  full-width horizontal frame line
///   status  (1 line, bottom)
fn ui(f: &mut Frame, app: &mut App) {
    let area = f.area();

    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(5), // OA logo (FIGlet "small")
            Constraint::Min(3),    // chat
            Constraint::Length(1), // input top frame
            Constraint::Length(1), // input
            Constraint::Length(1), // input bottom frame
            Constraint::Length(1), // status (bottom)
        ])
        .split(area);

    header::draw(f, chunks[0], app);
    chat::draw(f, chunks[1], app);

    // Horizontal frame lines above + below the input bar
    let frame = Paragraph::new(Line::from(Span::styled(
        "─".repeat(area.width as usize),
        Style::default().fg(ACCENT_FG),
    )));
    f.render_widget(frame, chunks[2]);
    input::draw(f, chunks[3], app);
    let frame2 = Paragraph::new(Line::from(Span::styled(
        "─".repeat(area.width as usize),
        Style::default().fg(ACCENT_FG),
    )));
    f.render_widget(frame2, chunks[4]);

    draw_status(f, chunks[5], app);

    // Overlays
    if app.input_mode == InputMode::AskUser {
        ask_user::draw(f, area, app);
    }
    if app.cmd_mode && !app.cmd_suggestions.is_empty() {
        draw_cmd_popup(f, area, app);
    }
    if let Some(ref err) = app.last_error {
        let err_p = Paragraph::new(err.as_str())
            .style(Style::default().fg(Color::Red))
            .block(Block::default().borders(Borders::ALL).title("Error"));
        let err_area = Rect::new(
            area.x,
            area.y + area.height.saturating_sub(6),
            area.width,
            3,
        );
        f.render_widget(Clear, err_area);
        f.render_widget(err_p, err_area);
    }
}

fn draw_status(f: &mut Frame, area: Rect, app: &App) {
    let status_style = if app.is_processing {
        Style::default().fg(HIGHLIGHT_FG)
    } else {
        Style::default().fg(MUTED_FG)
    };

    let dots = if app.is_processing {
        let frame = (app.frame_count as usize) % LOADING_FRAMES.len();
        Span::styled(LOADING_FRAMES[frame], Style::default().fg(HIGHLIGHT_FG))
    } else {
        Span::styled(IDLE_DOTS, Style::default().fg(MUTED_FG))
    };

    let status_text = if app.confirm_quit {
        Span::styled(" Quit? (y/N)", status_style)
    } else if app.confirm_delete {
        Span::styled(
            " Delete current session? (Y/n)",
            Style::default().fg(Color::Red),
        )
    } else {
        Span::styled(format!(" {}", app.status), status_style)
    };

    let model_info: Vec<Span> = if !app.model_name.is_empty() {
        let info = if !app.current_tool_name.is_empty() {
            format!(" · {}", app.current_tool_name)
        } else {
            format!(" · {}", app.model_name)
        };
        vec![Span::styled("  ", MUTED_FG), Span::styled(info, MUTED_FG)]
    } else {
        vec![]
    };

    let footer = Line::from({
        let mut spans: Vec<Span> = vec![dots, status_text];
        spans.extend(model_info);
        spans
    });
    let p = Paragraph::new(footer);
    f.render_widget(p, area);
}

fn draw_cmd_popup(f: &mut Frame, area: Rect, app: &App) {
    let suggestions: Vec<Line> = app
        .cmd_suggestions
        .iter()
        .enumerate()
        .map(|(i, cmd)| {
            let sel = i == app.cmd_selected;
            let prefix = if sel { "▸ " } else { "  " };
            let style = if sel {
                Style::default()
                    .fg(HIGHLIGHT_FG)
                    .add_modifier(Modifier::BOLD)
            } else {
                Style::default().fg(MUTED_FG)
            };
            Line::from(Span::styled(format!("{}{}", prefix, cmd), style))
        })
        .collect();

    if suggestions.is_empty() {
        return;
    }

    let popup_h = (suggestions.len() as u16 + 2).min(area.height / 2);
    let popup_w = 42.min(area.width.saturating_sub(4));
    let popup = Rect::new(
        area.x + 1,
        area.y + area.height.saturating_sub(popup_h).saturating_sub(2),
        popup_w,
        popup_h,
    );

    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(HIGHLIGHT_FG))
        .title("Commands");
    let para = Paragraph::new(suggestions).block(block);
    f.render_widget(Clear, popup);
    f.render_widget(para, popup);
}
