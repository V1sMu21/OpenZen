//! AskUser dialog — a centred modal that appears when the agent
//! calls the `ask_user` tool. The user picks a candidate with
//! arrow keys (or types a free-form answer) and confirms with
//! Enter. Esc dismisses.

use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, Paragraph, Wrap};
use ratatui::Frame;

use crate::app::App;
use crate::theme::*;

pub fn draw(f: &mut Frame, area: Rect, app: &App) {
    let (question, cands) = match &app.pending_ask_user {
        Some((q, c)) => (q.clone(), c.clone()),
        None => return,
    };

    let mut lines: Vec<Line> = vec![
        Line::from(Span::styled(
            "Agent needs your input",
            Style::default().fg(HIGHLIGHT_FG).add_modifier(Modifier::BOLD),
        )),
        Line::from(""),
        Line::from(Span::styled(question, Style::default().fg(AGENT_FG))),
        Line::from(""),
    ];
    if !cands.is_empty() {
        for (i, c) in cands.iter().enumerate() {
            let sel = i == app.cmd_selected;
            let prefix = if sel { "●" } else { "○" };
            let style = if sel {
                Style::default().fg(HIGHLIGHT_FG).add_modifier(Modifier::BOLD)
            } else {
                Style::default().fg(MUTED_FG)
            };
            lines.push(Line::from(Span::styled(
                format!("  {} {}", prefix, c),
                style,
            )));
        }
    } else {
        // Free-form answer: show what the user has typed so far.
        lines.push(Line::from(Span::styled(
            format!("  ▸ {}", app.input),
            Style::default().fg(USER_FG),
        )));
    }
    lines.push(Line::from(""));
    lines.push(Line::from(Span::styled(
        "↑/↓ navigate  Enter confirm  Esc skip",
        MUTED_FG,
    )));

    let popup = Rect::new(
        area.x + (area.width / 6),
        area.y + (area.height / 4),
        (area.width * 2 / 3).max(40),
        ((lines.len() as u16) + 4).min(area.height / 2),
    );
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(HIGHLIGHT_FG))
        .title("Ask User");
    let para = Paragraph::new(lines).block(block).wrap(Wrap { trim: false });
    f.render_widget(Clear, popup);
    f.render_widget(para, popup);
}
