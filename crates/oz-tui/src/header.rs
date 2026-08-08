//! Top header — OpenZen FIGlet logo + "Work Less, Imagine More" slogan.

use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;
use ratatui::Frame;

use crate::theme::*;

pub fn draw(f: &mut Frame, area: Rect, _app: &crate::app::App) {
    if area.height == 0 {
        return;
    }
    let logo_width = OA_LOGO.iter().map(|l| l.len()).max().unwrap_or(0);
    let mut lines: Vec<Line> = Vec::new();
    for (i, line) in OA_LOGO.iter().enumerate() {
        let padded = format!("{:<width$}", line, width = logo_width);
        if i == 2 {
            lines.push(Line::from(vec![
                Span::styled(
                    padded,
                    Style::default().fg(ACCENT_FG).add_modifier(Modifier::BOLD),
                ),
                Span::styled("  ", Style::default()),
                Span::styled(
                    TAGLINE,
                    Style::default().fg(MUTED_FG).add_modifier(Modifier::ITALIC),
                ),
            ]));
        } else {
            lines.push(Line::from(Span::styled(
                padded,
                Style::default().fg(ACCENT_FG).add_modifier(Modifier::BOLD),
            )));
        }
    }
    let para = Paragraph::new(lines);
    f.render_widget(para, area);
}
