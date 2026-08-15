//! Input bar — the single line at the bottom of the screen where
//! the user types. The bar is always editable; there is no "press
//! i to type" gate. Slash commands use a gold prefix-less style.

use ratatui::layout::Rect;
use ratatui::style::{Color, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;
use ratatui::Frame;

use crate::app::{App, InputMode};
use crate::theme::*;

pub fn draw(f: &mut Frame, area: Rect, app: &App) {
    let (prefix, text, style): (String, &str, Style) = match app.input_mode {
        InputMode::Editing => {
            if app.cmd_mode {
                (
                    String::new(),
                    app.input.as_str(),
                    Style::default().fg(HIGHLIGHT_FG),
                )
            } else {
                let rendered = app
                    .left_prompt
                    .as_ref()
                    .map(|p| p.render(&app.template_vars()))
                    .unwrap_or_else(|| "▸ ".to_string());
                (rendered, app.input.as_str(), Style::default().fg(USER_FG))
            }
        }
        InputMode::AskUser => (
            "? ".to_string(),
            app.input.as_str(),
            Style::default().fg(HIGHLIGHT_FG),
        ),
    };
    let display = if text.is_empty()
        && !app.cmd_mode
        && matches!(app.input_mode, InputMode::Editing)
        && !app.is_processing
    {
        Line::from(Span::styled(
            format!("{}type to chat · / for commands · /exit to quit", prefix),
            Style::default().fg(Color::DarkGray),
        ))
    } else {
        Line::from(Span::styled(format!("{}{}", prefix, text), style))
    };
    f.render_widget(Paragraph::new(display), area);
}
