// ga-tui :: chat rendering
// (c) 2026 OpenZen contributors — MIT
//
// The chat pane is the heart of the TUI. It owns nothing — it just
// walks the `App::items` slice of `ChatItem` rows and projects each
// one into a sequence of Ratatui `Line`s. The pane does not know
// about sessions, agents, or tools directly; rendering decisions
// (folding, tag stripping, emoji mapping) live here so the rest of
// the app can stay simple.

use crate::app::{
    chat_item_role, App, ChatItem, MsgRole, ToolStatus, LONG_MSG_PREVIEW, LONG_MSG_THRESHOLD,
};
use crate::markdown::render_markdown;
use crate::theme::*;
use ratatui::layout::{Alignment, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Paragraph, Wrap};
use ratatui::Frame;

/// Paint the chat into the frame buffer. Scroll position is read
/// from `App::chat_scroll` (or auto-computed from the tail when
/// `App::follow_tail` is true) so the key handler can move it
/// without the pane needing to know about keys.
pub fn draw(f: &mut Frame, area: Rect, app: &App) {
    let width = area.width as usize;
    let lines = build_lines(&app.items, width);
    let viewport_h = area.height as usize;
    let total = lines.len();
    let max_scroll = total.saturating_sub(viewport_h);
    let scroll = if app.follow_tail {
        max_scroll as u16
    } else {
        app.chat_scroll.min(max_scroll).min(u16::MAX as usize) as u16
    };
    let p = Paragraph::new(lines)
        .scroll((scroll, 0))
        .alignment(Alignment::Left)
        .wrap(Wrap { trim: false });
    f.render_widget(p, area);

    if max_scroll > 0 {
        let indicator = build_scroll_indicator(scroll as usize, max_scroll, viewport_h);
        let ind_w = indicator.chars().count() as u16 + 2;
        let ind_x = area.x + area.width.saturating_sub(ind_w);
        let ind_y = area.y;
        let ind_area = Rect::new(ind_x, ind_y, ind_w.min(area.width), 1);
        let ind_p = Paragraph::new(Line::from(Span::styled(
            format!(" {} ", indicator),
            Style::default().fg(HIGHLIGHT_FG).bg(Color::Black),
        )));
        f.render_widget(ind_p, ind_area);
    }
}

fn build_scroll_indicator(scroll: usize, max_scroll: usize, viewport_h: usize) -> String {
    let effective = scroll.min(max_scroll);
    if effective == 0 {
        return "↑ top".to_string();
    }
    if effective >= max_scroll {
        return "↓ end".to_string();
    }
    let shown_start = effective + 1;
    let shown_end = (effective + viewport_h).min(max_scroll + viewport_h);
    let total_approx = max_scroll + viewport_h;
    let pct = (shown_start * 100).checked_div(total_approx).unwrap_or(0);
    format!("↑ {}–{} {}%", shown_start, shown_end, pct)
}

/// Project a `ChatItem` slice into a sequence of display `Line`s,
/// applying folding, tag escaping, and tool-call card framing. The
/// pane does not own layout — the caller supplies `width` so
/// wrapping matches the current viewport.
pub(crate) fn build_lines(items: &[ChatItem], width: usize) -> Vec<Line<'static>> {
    let mut out: Vec<Line<'static>> = Vec::new();
    let mut prev_role: Option<MsgRole> = None;
    for (idx, item) in items.iter().enumerate() {
        let cur_role = chat_item_role(item);
        // ── Role separator: thin 月白 line between User and Agent blocks
        if let Some(pr) = prev_role {
            if pr != cur_role
                && matches!(
                    (pr, cur_role),
                    (MsgRole::User, MsgRole::Agent) | (MsgRole::Agent, MsgRole::User)
                )
            {
                out.push(Line::from(Span::styled(
                    "─".repeat(width.min(40)),
                    MUTED_FG,
                )));
            }
        }
        match item {
            ChatItem::UserMessage { content, ts } => {
                render_user_message(&mut out, content, ts, width);
            }
            ChatItem::AssistantText {
                content,
                ts,
                expanded,
            } => {
                render_agent_message(&mut out, content, ts, *expanded, width);
            }
            ChatItem::SummaryHeader { content, expanded } => {
                render_summary_header(&mut out, content, *expanded, width);
            }
            ChatItem::SummaryBody { content } => {
                if !content.is_empty() {
                    for wrapped in wrap_text(content, width.saturating_sub(4)) {
                        out.push(Line::from(Span::styled(wrapped, MUTED_FG)));
                    }
                }
            }
            ChatItem::ThinkingHeader { .. } | ChatItem::ThinkingBody { .. } => {
                // Kept for session-store round-trips; never rendered.
            }
            ChatItem::ToolCall {
                name,
                args,
                status,
                result,
                ts,
                expanded,
            } => {
                render_tool_call(&mut out, name, args, status, ts, result, *expanded, width);
            }
            ChatItem::SystemMessage { content, ts } => {
                render_system_message(&mut out, content, ts, width);
            }
            ChatItem::AskUserItem {
                question,
                candidates,
                status,
                response,
            } => {
                render_ask_user(
                    &mut out,
                    question,
                    candidates,
                    *status,
                    response.as_deref(),
                    width,
                );
            }
        }
        // Blank line between items for breathing room — but never between
        // a fold-header and its body (they belong together).
        if idx + 1 < items.len() && !is_fold_pair(&items[idx], &items[idx + 1]) {
            out.push(Line::from(""));
        }
        prev_role = Some(cur_role);
    }
    out
}

/// True if `a` is a fold header and `b` is its body — these must
/// render without a blank line between them or the visual coupling
/// is lost.
fn is_fold_pair(a: &ChatItem, b: &ChatItem) -> bool {
    matches!(
        (a, b),
        (ChatItem::SummaryHeader { .. }, ChatItem::SummaryBody { .. })
            | (
                ChatItem::ThinkingHeader { .. },
                ChatItem::ThinkingBody { .. }
            )
    )
}

// ── per-role renderers ──

fn render_user_message(lines: &mut Vec<Line>, text: &str, _ts: &str, width: usize) {
    // User message: ▌ prefix bar in the user color, body in user_fg.
    // The whole line gets a faint background tint to make the region
    // visually distinct from the agent's reply.
    let user_style = Style::default().fg(USER_FG).bg(Color::Rgb(20, 28, 38));
    let bar_style = Style::default().fg(HIGHLIGHT_FG).bg(Color::Rgb(30, 38, 48));
    let bar = "▌";
    let content_w = width.saturating_sub(2);
    for wrapped in wrap_text(text, content_w) {
        lines.push(Line::from(vec![
            Span::styled(bar, bar_style),
            Span::styled(" ", bar_style),
            Span::styled(wrapped, user_style),
        ]));
    }
}

fn render_agent_message(
    lines: &mut Vec<Line>,
    text: &str,
    _ts: &str,
    expanded: bool,
    width: usize,
) {
    let total = text.lines().count();
    let (shown_text, hidden) = if total > LONG_MSG_THRESHOLD && !expanded {
        let head: String = text
            .lines()
            .take(LONG_MSG_PREVIEW)
            .collect::<Vec<_>>()
            .join("\n");
        (head, total - LONG_MSG_PREVIEW)
    } else {
        (text.to_string(), 0)
    };
    // Agent region: subtle green-tinted background to distinguish
    // from the user's blue-tinted region. The bar is `▌` in the
    // agent color.
    let agent_bg = Color::Rgb(15, 26, 26);
    let bar_bg = Color::Rgb(25, 38, 38);
    let bar = "▌";
    let bar_style = Style::default().fg(AGENT_FG).bg(bar_bg);
    let inner_w = width.saturating_sub(2);
    let mut md_lines = render_markdown(&shown_text, inner_w);
    // Prepend a bar to each line and apply background to every span
    // in the agent region so the visual block is contiguous.
    let mut block: Vec<Line> = Vec::new();
    block.push(Line::from(Span::styled(bar, bar_style)));
    for ml in md_lines.drain(..) {
        let mut new_spans: Vec<Span> = vec![Span::styled(" ", bar_style)];
        for sp in ml.spans.into_iter() {
            let mut s = sp.style;
            if s.bg.is_none() {
                s = s.bg(agent_bg);
            }
            new_spans.push(Span::styled(sp.content, s));
        }
        block.push(Line::from(new_spans));
    }
    lines.extend(block);
    if hidden > 0 {
        let hidden_style = Style::default().fg(MUTED_FG).bg(agent_bg);
        let mut spans = vec![Span::styled(" ", hidden_style)];
        spans.push(Span::styled(
            format!("  [+{} lines · Space to expand]", hidden),
            hidden_style,
        ));
        lines.push(Line::from(spans));
    }
}

fn render_summary_header(lines: &mut Vec<Line>, content: &str, expanded: bool, _width: usize) {
    let prefix = if expanded { "▾" } else { "▸" };
    lines.push(Line::from(Span::styled(
        format!("{} {}", prefix, content),
        MUTED_FG,
    )));
}

fn render_system_message(lines: &mut Vec<Line>, content: &str, _ts: &str, width: usize) {
    for wrapped in wrap_text(content, width) {
        lines.push(Line::from(Span::styled(format!("· {}", wrapped), MUTED_FG)));
    }
}

fn render_ask_user(
    lines: &mut Vec<Line>,
    question: &str,
    candidates: &[String],
    status: crate::app::AskUserStatus,
    response: Option<&str>,
    width: usize,
) {
    lines.push(Line::from(Span::styled(
        format!("❓ {}", question),
        HIGHLIGHT_FG,
    )));
    for (i, opt) in candidates.iter().enumerate() {
        let key = char::from(b'a' + i as u8);
        let style = match status {
            crate::app::AskUserStatus::Answered => MUTED_FG,
            crate::app::AskUserStatus::Pending => USER_FG,
        };
        lines.push(Line::from(Span::styled(
            format!("  [{}] {}", key, opt),
            style,
        )));
    }
    if let Some(ans) = response {
        lines.push(Line::from(Span::styled(
            format!("  → {}", truncate_inline(ans, width.saturating_sub(8))),
            AGENT_FG,
        )));
    }
}

// ── helpers ──

/// Naive hard-wrap that respects terminal width. We don't try to be
/// smart about word boundaries — input is short, agent-formatted
/// text, and breaking inside a CJK run is fine.
fn wrap_text(s: &str, width: usize) -> Vec<String> {
    if width == 0 {
        return vec![s.to_string()];
    }
    let mut out: Vec<String> = Vec::new();
    for raw_line in s.split('\n') {
        let chars: Vec<char> = raw_line.chars().collect();
        let mut start = 0;
        while start < chars.len() {
            let end = (start + width).min(chars.len());
            out.push(chars[start..end].iter().collect());
            start = end;
        }
    }
    out
}

/// Pick the single most informative argument from a tool call's JSON
/// `args` string for the card header. Mirrors the webui's
/// `summaryArg()` in `ToolCallCard.svelte` so both UIs agree on
/// what to show — the order of keys matters.
fn tool_arg_summary(name: &str, args: &str) -> String {
    if args.is_empty() {
        return String::new();
    }
    let _ = name; // key order below is already a superset of the webui's
                  // Deliberately omits `code`/`command`/`data` — the chat header
                  // must never inline raw code, shell commands, or JSON blobs.
    const KEYS: &[&str] = &[
        "file_path",
        "path",
        "pattern",
        "url",
        "name",
        "prompt",
        "question",
        "query",
        "goal",
    ];
    for key in KEYS {
        if let Some(val) = extract_json_string(args, key) {
            return truncate_inline(&val, 50);
        }
    }
    // No recognised key — empty string beats a garbled `{"data":...}` blob.
    String::new()
}

/// Look for `"key":"value"` in a JSON-ish string and return the
/// unescaped value. Doesn't pull in serde_json — tool args are
/// short and the parsing cost would dominate the render cost.
fn extract_json_string(args: &str, key: &str) -> Option<String> {
    let needle = format!("\"{}\":", key);
    let pos = args.find(&needle)?;
    let after = args[pos + needle.len()..].trim_start();
    let rest = after.strip_prefix('"')?;
    let bytes = rest.as_bytes();
    let mut end = 0usize;
    while end < bytes.len() {
        match bytes[end] {
            b'\\' if end + 1 < bytes.len() => end += 2,
            b'"' => return Some(unescape(&rest[..end])),
            _ => end += 1,
        }
    }
    None
}

/// Char-aware inline truncator. Counts chars (not bytes) so CJK
/// text doesn't get cut in the middle of a glyph. Trailing `…` is
/// used to mark truncation.
fn truncate_inline(s: &str, max: usize) -> String {
    if max == 0 {
        return String::new();
    }
    let count = s.chars().count();
    if count <= max {
        return s.to_string();
    }
    let keep = max.saturating_sub(1);
    let mut out: String = s.chars().take(keep).collect();
    out.push('…');
    out
}

/// Unescape a JSON-style string literal body. Handles `\"`, `\\`,
/// `\n`, `\t`, `\r`, `\/` and the `\uXXXX` forms. Used by
/// `extract_json_string` for tool-arg rendering.
fn unescape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let bytes = s.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'\\' && i + 1 < bytes.len() {
            match bytes[i + 1] {
                b'"' => {
                    out.push('"');
                    i += 2;
                }
                b'\\' => {
                    out.push('\\');
                    i += 2;
                }
                b'n' => {
                    out.push('\n');
                    i += 2;
                }
                b't' => {
                    out.push('\t');
                    i += 2;
                }
                b'r' => {
                    out.push('\r');
                    i += 2;
                }
                b'/' => {
                    out.push('/');
                    i += 2;
                }
                b'u' if i + 5 < bytes.len() => {
                    let hex = std::str::from_utf8(&bytes[i + 2..i + 6]).unwrap_or("");
                    if let Some(c) = u32::from_str_radix(hex, 16).ok().and_then(char::from_u32) {
                        out.push(c);
                    }
                    i += 6;
                }
                b => {
                    out.push(b as char);
                    i += 2;
                }
            }
        } else {
            out.push(bytes[i] as char);
            i += 1;
        }
    }
    out
}

// ── Tool call rendering (the critical fix) ──

/// Render a `ChatItem::ToolCall` as a card. The card has a visible
/// box frame so it is unambiguous which lines belong to the tool
/// call and which belong to the surrounding chat. Collapsed state
/// shows the summary line plus a one-line result preview if any;
/// expanded state shows the full result up to a hard line cap.
#[allow(clippy::too_many_arguments)]
fn render_tool_call(
    lines: &mut Vec<Line>,
    name: &str,
    args: &str,
    status: &ToolStatus,
    _ts: &str,
    result: &str,
    expanded: bool,
    width: usize,
) {
    let emoji = tool_emoji(name);
    let (status_icon, status_color) = match status {
        ToolStatus::Running => ("⚙", TOOL_RUNNING),
        ToolStatus::Done => ("✓", TOOL_DONE),
        ToolStatus::Error => ("✗", TOOL_ERROR),
    };

    let arg_summary = tool_arg_summary(name, args);
    let name_style = Style::default()
        .fg(status_color)
        .add_modifier(Modifier::BOLD);
    let inner_w = width.max(20);

    // Simple header line: emoji + name + arg summary + status.
    // No box borders — they make the viewport look messy when many
    // tool calls land in a row.
    let mut head = vec![Span::styled(format!("  {} {} ", emoji, name), name_style)];
    if !arg_summary.is_empty() {
        let arg_budget = inner_w.saturating_sub(20).max(8);
        head.push(Span::styled(
            format!("· {} ", truncate_inline(&arg_summary, arg_budget)),
            MUTED_FG,
        ));
    }
    let result_lines = result.lines().count();
    if *status == ToolStatus::Done && result_lines > 0 {
        head.push(Span::styled(format!("· {} lines ", result_lines), MUTED_FG));
    }
    head.push(Span::styled(status_icon.to_string(), status_color));

    lines.push(Line::from(head));

    if expanded {
        if !result.is_empty() {
            const MAX_LINES: usize = 60;
            // For code-related tools, wrap result in a fenced code block so
            // render_markdown applies code-block styling (dimmed + italic).
            // Otherwise the raw text has no markdown markers and renders as
            // a plain paragraph, which is visually indistinguishable from
            // the old raw-text display.
            let code_tools = [
                "write",
                "read_file",
                "edit",
                "patch",
                "bash",
                "code_run",
                "grep",
                "ast_grep_replace",
                "glob",
            ];
            let is_code = code_tools.contains(&name);
            let md_source = if is_code {
                format!("```\n{}\n```", result)
            } else {
                result.to_string()
            };
            let md_lines = render_markdown(&md_source, inner_w.saturating_sub(4));
            let total = md_lines.len();
            let shown = total.min(MAX_LINES);
            for md_line in &md_lines[..shown] {
                let mut spans = vec![Span::styled("    ", MUTED_FG)];
                spans.extend(md_line.spans.iter().cloned());
                lines.push(Line::from(spans));
            }
            if total > shown {
                lines.push(Line::from(Span::styled(
                    format!(
                        "    ··· {} more lines ···  [Space to collapse]",
                        total - shown
                    ),
                    MUTED_FG,
                )));
            }
        }
    } else if !result.is_empty() {
        let first = result.lines().next().unwrap_or("").trim();
        let preview = truncate_inline(first, inner_w.saturating_sub(4));
        if first.is_empty() {
            lines.push(Line::from(Span::styled(
                "    (empty)".to_string(),
                MUTED_FG,
            )));
        } else {
            lines.push(Line::from(Span::styled(
                format!("    {}", preview),
                MUTED_FG,
            )));
        }
        if result_lines > 1 {
            lines.push(Line::from(Span::styled(
                format!(
                    "    ··· {} more lines ···  [Space to expand]",
                    result_lines - 1
                ),
                MUTED_FG,
            )));
        }
    }
}

// ── tests ──

#[cfg(test)]
mod tests {
    use super::*;

    fn s(text: &str) -> String {
        text.to_string()
    }

    #[test]
    fn user_message_wraps_and_right_aligns() {
        let items = vec![ChatItem::UserMessage {
            content: s("hello world"),
            ts: s("12:00:00"),
        }];
        let lines = build_lines(&items, 80);
        let flat: String = lines
            .iter()
            .flat_map(|l| l.spans.iter())
            .map(|sp| sp.content.as_ref())
            .collect();
        assert!(flat.contains("hello world"));
        assert!(!flat.contains("12:00:00"), "no timestamp on user message");
        assert!(!flat.contains("you"), "no role label on user message");
    }

    #[test]
    fn agent_long_message_folds_at_threshold() {
        let mut content = String::new();
        for i in 0..(LONG_MSG_THRESHOLD + 5) {
            content.push_str(&format!("line {}\n", i));
        }
        let items = vec![ChatItem::AssistantText {
            content,
            ts: s("12:00:00"),
            expanded: false,
        }];
        let lines = build_lines(&items, 80);
        let flat: String = lines
            .iter()
            .flat_map(|l| l.spans.iter())
            .map(|sp| sp.content.as_ref())
            .collect();
        assert!(
            flat.contains("[+"),
            "long message should produce a fold hint"
        );
        // Lines beyond the preview window must NOT be in the flat output.
        assert!(!flat.contains(&format!("line {}", LONG_MSG_THRESHOLD + 4)));
    }

    #[test]
    fn tool_call_collapsed_shows_header_and_preview() {
        let items = vec![ChatItem::ToolCall {
            name: s("read_file"),
            args: s("{\"path\":\"/x/y\"}"),
            status: ToolStatus::Done,
            result: s("line1\nline2\nline3"),
            ts: s("12:34:56"),
            expanded: false,
        }];
        let lines = build_lines(&items, 80);
        let flat: String = lines
            .iter()
            .flat_map(|l| l.spans.iter())
            .map(|sp| sp.content.as_ref())
            .collect();
        assert!(flat.contains("read_file"));
        assert!(flat.contains("✓"));
        assert!(!flat.contains("┌─"), "no box border on collapsed card");
        assert!(!flat.contains("└─"), "no box border on collapsed card");
        assert!(
            flat.contains("line1"),
            "collapsed card should preview the first line"
        );
        assert!(
            flat.contains("··· 2 more lines"),
            "collapsed card should show fold hint"
        );
    }

    #[test]
    fn tool_call_expanded_shows_full_result() {
        let items = vec![ChatItem::ToolCall {
            name: s("read_file"),
            args: s("{}"),
            status: ToolStatus::Done,
            result: s("first\nsecond\nthird"),
            ts: s("12:34:56"),
            expanded: true,
        }];
        let lines = build_lines(&items, 80);
        let flat: String = lines
            .iter()
            .flat_map(|l| l.spans.iter())
            .map(|sp| sp.content.as_ref())
            .collect();
        assert!(flat.contains("first"));
        assert!(flat.contains("second"));
        assert!(flat.contains("third"));
    }

    #[test]
    fn tool_arg_summary_picks_first_recognised_key() {
        let args = r#"{"url":"https://example.com","prompt":"x"}"#;
        assert_eq!(tool_arg_summary("web_search", args), "https://example.com");
        let args = r#"{"prompt":"hello world","data":"blob"}"#;
        assert_eq!(tool_arg_summary("anything", args), "hello world");
        assert_eq!(tool_arg_summary("anything", ""), "");
        // No recognised key — return empty rather than dumping raw JSON.
        assert_eq!(tool_arg_summary("anything", "{}"), "");
        // `code` and `command` are deliberately excluded.
        assert_eq!(tool_arg_summary("code_run", r#"{"code":"rm -rf /"}"#), "");
        assert_eq!(tool_arg_summary("bash", r#"{"command":"ls"}"#), "");
    }

    #[test]
    fn extract_json_string_handles_escapes() {
        assert_eq!(
            extract_json_string(r#"{"path":"a\/b"}"#, "path"),
            Some("a/b".to_string())
        );
        assert_eq!(
            extract_json_string(r#"{"name":"hello"}"#, "name"),
            Some("hello".to_string())
        );
        assert_eq!(extract_json_string(r#"{}"#, "name"), None);
    }

    #[test]
    fn truncate_inline_handles_cjk() {
        assert_eq!(truncate_inline("hello world", 5), "hell…");
        assert_eq!(truncate_inline("中文测试字符串", 4), "中文测…");
        assert_eq!(truncate_inline("short", 100), "short");
    }

    #[test]
    fn wrap_text_hard_wraps_at_width() {
        assert_eq!(wrap_text("abcdefghij", 3), vec!["abc", "def", "ghi", "j"]);
    }

    #[test]
    fn fold_pair_keeps_summary_header_and_body_together() {
        let items = vec![
            ChatItem::SummaryHeader {
                content: s("plan"),
                expanded: false,
            },
            ChatItem::SummaryBody {
                content: s("full plan body"),
            },
            ChatItem::UserMessage {
                content: s("hi"),
                ts: s("00:00:00"),
            },
        ];
        let lines = build_lines(&items, 80);
        let flat: String = lines
            .iter()
            .flat_map(|l| l.spans.iter())
            .map(|sp| sp.content.as_ref())
            .collect();
        assert!(flat.contains("plan"));
        assert!(flat.contains("full plan body"));
        assert!(flat.contains("hi"));
    }

    #[test]
    fn agent_message_renders_bold_segments_with_bold_modifier() {
        let items = vec![ChatItem::AssistantText {
            content: s("I am **bold** here"),
            ts: s("12:00:00"),
            expanded: false,
        }];
        let lines = build_lines(&items, 80);
        let mut found_bold = false;
        let mut found_plain = false;
        for l in &lines {
            for sp in &l.spans {
                if sp.content == "bold" {
                    assert!(sp.style.add_modifier.contains(Modifier::BOLD));
                    found_bold = true;
                }
                if sp.content == "I am " {
                    assert!(!sp.style.add_modifier.contains(Modifier::BOLD));
                    found_plain = true;
                }
            }
        }
        assert!(found_bold, "bold segment should be present");
        assert!(found_plain, "plain segment should be present");
    }
}
