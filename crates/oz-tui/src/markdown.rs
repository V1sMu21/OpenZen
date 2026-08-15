//! Enhanced markdown-to-ratatui renderer for the TUI chat pane.
//!
//! Handles block-level elements (code blocks, headings, lists, tables,
//! blockquotes, horizontal rules) and inline formatting (**bold**,
//! *italic*, ~~strikethrough~~, `code`, [links](url), $math$).
//!
//! The top-level entry is [`render_markdown()`].
//!
//! The design is two-stage:
//!  1. Block-level split: walks the input and groups lines into
//!     `Code`, `Heading`, `Table`, `Ulist`, `Olist`, `Blockquote`,
//!     `Hr`, `Paragraph` blocks. Code fences are honoured first so
//!     their content is never re-interpreted as markdown.
//!  2. Inline tokenization: for each non-code block, scans character
//!     by character with a stateful pointer so `**bold *and italic* end**`,
//!     ``` ```code``` ```, and `[link](url)` parse correctly without
//!     consuming the wrong delimiter.

use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use crate::theme::*;

// ── Styles ──

fn base() -> Style {
    Style::default().fg(AGENT_FG)
}
fn bold_s() -> Style {
    Style::default().fg(HIGHLIGHT_FG).add_modifier(Modifier::BOLD)
}
fn italic_s() -> Style {
    Style::default().fg(AGENT_FG).add_modifier(Modifier::ITALIC)
}
fn bold_italic_s() -> Style {
    Style::default().fg(HIGHLIGHT_FG).add_modifier(Modifier::BOLD | Modifier::ITALIC)
}
fn del_s() -> Style {
    Style::default().fg(MUTED_FG).add_modifier(Modifier::CROSSED_OUT)
}
fn code_inline_s() -> Style {
    Style::default().fg(USER_FG).add_modifier(Modifier::ITALIC)
}
fn code_block_s() -> Style {
    Style::default().fg(USER_FG)
}
fn link_s() -> Style {
    Style::default().fg(ACCENT_FG).add_modifier(Modifier::UNDERLINED)
}
fn heading_s(level: u8) -> Style {
    let color = match level {
        1 => HIGHLIGHT_FG,
        2 => ACCENT_FG,
        _ => AGENT_FG,
    };
    Style::default().fg(color).add_modifier(Modifier::BOLD)
}
fn quote_s() -> Style {
    Style::default().fg(MUTED_FG).add_modifier(Modifier::ITALIC)
}
fn hr_s() -> Style {
    Style::default().fg(MUTED_FG)
}
fn table_head_s() -> Style {
    Style::default().fg(HIGHLIGHT_FG).add_modifier(Modifier::BOLD)
}
fn table_cell_s() -> Style {
    Style::default().fg(AGENT_FG)
}
fn bullet_s() -> Style {
    Style::default().fg(HIGHLIGHT_FG)
}

// ── Entry point ──

pub fn render_markdown(text: &str, width: usize) -> Vec<Line<'static>> {
    let mut out: Vec<Line<'static>> = Vec::new();
    let text = strip_unwanted_tags(text);
    let blocks = split_blocks(&text);
    for block in blocks {
        render_block(&mut out, &block, width);
    }
    out
}

fn strip_unwanted_tags(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let bytes = s.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        let rest = &s[i..];
        if rest.starts_with("<thinking>") {
            if let Some(end) = rest.find("</thinking>") {
                i += end + "</thinking>".len();
                continue;
            }
        }
        if rest.starts_with("<summary>") {
            if let Some(end) = rest.find("</summary>") {
                i += end + "</summary>".len();
                continue;
            }
        }
        if rest.starts_with("<respond>") || rest.starts_with("</respond>") {
            i += rest.find('>').unwrap() + 1;
            continue;
        }
        let c = s[i..].chars().next().unwrap();
        out.push(c);
        i += c.len_utf8();
    }
    out
}

// ── Block model ──

#[derive(Debug, Clone)]
#[allow(clippy::enum_variant_names)]
enum Block {
    Paragraph(String),
    Heading { level: u8, text: String },
    CodeBlock { lang: String, content: String },
    Hr,
    Ulist(Vec<String>),
    Olist(Vec<String>),
    Table { head: Vec<String>, rows: Vec<Vec<String>> },
    Blockquote(Vec<String>),
}

fn split_blocks(text: &str) -> Vec<Block> {
    let mut blocks: Vec<Block> = Vec::new();
    let lines: Vec<&str> = text.lines().collect();
    let mut i = 0;
    while i < lines.len() {
        let line = lines[i];
        let trimmed = line.trim_start();

        // ── Fenced code block ──
        if trimmed.starts_with("```") {
            let lang = trimmed.trim_start_matches("```").trim().to_string();
            let mut content = String::new();
            i += 1;
            while i < lines.len() {
                if lines[i].trim_start().starts_with("```") {
                    i += 1;
                    break;
                }
                if !content.is_empty() {
                    content.push('\n');
                }
                content.push_str(lines[i]);
                i += 1;
            }
            blocks.push(Block::CodeBlock { lang, content });
            continue;
        }

        // ── Heading ──
        if let Some((level, text)) = parse_heading(trimmed) {
            blocks.push(Block::Heading { level, text: text.to_string() });
            i += 1;
            continue;
        }

        // ── Horizontal rule ──
        if is_hr(trimmed) {
            blocks.push(Block::Hr);
            i += 1;
            continue;
        }

        // ── Table (consecutive lines starting with |) ──
        if trimmed.starts_with('|') && trimmed.contains('|') {
            let mut tbl_lines: Vec<&str> = Vec::new();
            while i < lines.len() && lines[i].trim_start().starts_with('|') {
                tbl_lines.push(lines[i]);
                i += 1;
            }
            if let Some(t) = parse_table(&tbl_lines) {
                blocks.push(t);
            } else {
                for l in tbl_lines {
                    if !blocks.is_empty() && !matches!(blocks.last(), Some(Block::Paragraph(_))) {
                        blocks.push(Block::Paragraph(String::new()));
                    }
                    push_paragraph_line(&mut blocks, l);
                }
            }
            continue;
        }

        // ── Unordered list ──
        if is_unordered_list_item(trimmed) {
            let mut items: Vec<String> = Vec::new();
            while i < lines.len() {
                let t = lines[i].trim_start();
                if is_unordered_list_item(t) {
                    let content = strip_unordered_marker(t).to_string();
                    items.push(content);
                    i += 1;
                } else if t.is_empty() {
                    i += 1;
                    break;
                } else if t.starts_with("  ") || t.starts_with("\t") {
                    if let Some(last) = items.last_mut() {
                        last.push(' ');
                        last.push_str(t.trim());
                    }
                    i += 1;
                } else {
                    break;
                }
            }
            blocks.push(Block::Ulist(items));
            continue;
        }

        // ── Ordered list ──
        if is_ordered_list_item(trimmed) {
            let mut items: Vec<String> = Vec::new();
            while i < lines.len() {
                let t = lines[i].trim_start();
                if is_ordered_list_item(t) {
                    let content = strip_ordered_marker(t).to_string();
                    items.push(content);
                    i += 1;
                } else if t.is_empty() {
                    i += 1;
                    break;
                } else if t.starts_with("  ") || t.starts_with("\t") {
                    if let Some(last) = items.last_mut() {
                        last.push(' ');
                        last.push_str(t.trim());
                    }
                    i += 1;
                } else {
                    break;
                }
            }
            blocks.push(Block::Olist(items));
            continue;
        }

        // ── Blockquote ──
        if trimmed.starts_with('>') {
            let mut lines_: Vec<String> = Vec::new();
            while i < lines.len() {
                let t = lines[i].trim_start();
                if t.starts_with('>') {
                    let stripped = t.trim_start_matches('>').trim_start().to_string();
                    lines_.push(stripped);
                    i += 1;
                } else if t.is_empty() {
                    i += 1;
                    break;
                } else {
                    break;
                }
            }
            blocks.push(Block::Blockquote(lines_));
            continue;
        }

        // ── Empty line: paragraph break ──
        if trimmed.is_empty() {
            i += 1;
            continue;
        }

        // ── Paragraph (gather until blank or special start) ──
        let mut para = String::new();
        while i < lines.len() {
            let t = lines[i];
            let t_trim = t.trim_start();
            if t_trim.is_empty() {
                break;
            }
            if t_trim.starts_with("```")
                || parse_heading(t_trim).is_some()
                || is_hr(t_trim)
                || t_trim.starts_with('|')
                || is_unordered_list_item(t_trim)
                || is_ordered_list_item(t_trim)
                || t_trim.starts_with('>')
            {
                break;
            }
            if !para.is_empty() {
                para.push(' ');
            }
            para.push_str(t);
            i += 1;
        }
        if !para.is_empty() {
            blocks.push(Block::Paragraph(para));
        }
    }
    blocks
}

fn push_paragraph_line(blocks: &mut Vec<Block>, line: &str) {
    match blocks.last_mut() {
        Some(Block::Paragraph(p)) => {
            if !p.is_empty() {
                p.push(' ');
            }
            p.push_str(line);
        }
        _ => blocks.push(Block::Paragraph(line.to_string())),
    }
}

fn parse_heading(line: &str) -> Option<(u8, &str)> {
    let bytes = line.as_bytes();
    let mut level = 0u8;
    while (level as usize) < bytes.len() && bytes[level as usize] == b'#' {
        level += 1;
    }
    if level == 0 || level > 6 {
        return None;
    }
    if (level as usize) >= bytes.len() || bytes[level as usize] != b' ' {
        return None;
    }
    let text = line[level as usize + 1..].trim();
    Some((level, text))
}

fn is_hr(line: &str) -> bool {
    let t = line.trim();
    if t.len() < 3 {
        return false;
    }
    let ch = t.chars().next().unwrap();
    if ch != '-' && ch != '*' && ch != '_' {
        return false;
    }
    t.chars().all(|c| c == ch || c == ' ')
        && t.chars().filter(|c| *c == ch).count() >= 3
}

fn is_unordered_list_item(line: &str) -> bool {
    if let Some(rest) = line.strip_prefix("- ") {
        return !rest.is_empty();
    }
    if let Some(rest) = line.strip_prefix("* ") {
        return !rest.is_empty();
    }
    if let Some(rest) = line.strip_prefix("+ ") {
        return !rest.is_empty();
    }
    false
}

fn is_ordered_list_item(line: &str) -> bool {
    let bytes = line.as_bytes();
    let mut i = 0;
    while i < bytes.len() && bytes[i].is_ascii_digit() {
        i += 1;
    }
    i > 0 && i < bytes.len() && bytes[i] == b'.' && i + 1 < bytes.len() && bytes[i + 1] == b' '
}

fn strip_unordered_marker(line: &str) -> &str {
    if let Some(rest) = line.strip_prefix("- ") {
        return rest;
    }
    if let Some(rest) = line.strip_prefix("* ") {
        return rest;
    }
    if let Some(rest) = line.strip_prefix("+ ") {
        return rest;
    }
    line
}

fn strip_ordered_marker(line: &str) -> &str {
    let bytes = line.as_bytes();
    let mut i = 0;
    while i < bytes.len() && bytes[i].is_ascii_digit() {
        i += 1;
    }
    if i < bytes.len() && bytes[i] == b'.' {
        line[i + 1..].trim_start()
    } else {
        line
    }
}

fn parse_table(lines: &[&str]) -> Option<Block> {
    if lines.len() < 2 {
        return None;
    }
    let split_row = |row: &str| -> Vec<String> {
        row.trim()
            .trim_matches('|')
            .split('|')
            .map(|c| c.trim().to_string())
            .collect()
    };
    let head_cells = split_row(lines[0]);
    // Second line must be the separator: | --- | :---: | etc.
    let sep = lines[1].trim();
    if !sep.starts_with('|') {
        return None;
    }
    let sep_cells: Vec<&str> = sep.trim_matches('|').split('|').collect();
    if !sep_cells.iter().all(|c| {
        let t = c.trim();
        !t.is_empty()
            && t.chars().all(|ch| ch == '-' || ch == ':' || ch == ' ')
    }) {
        return None;
    }
    if sep_cells.len() != head_cells.len() {
        return None;
    }
    let mut rows = Vec::new();
    for row in &lines[2..] {
        let cells = split_row(row);
        if cells.len() == head_cells.len() {
            rows.push(cells);
        }
    }
    Some(Block::Table { head: head_cells, rows })
}

// ── Block renderers ──

fn render_block(out: &mut Vec<Line<'static>>, block: &Block, width: usize) {
    match block {
        Block::Paragraph(text) => render_paragraph(out, text, width),
        Block::Heading { level, text } => render_heading(out, level, text, width),
        Block::CodeBlock { lang, content } => render_code_block(out, content, lang, width),
        Block::Hr => render_hr(out, width),
        Block::Ulist(items) => render_ulist(out, items, width),
        Block::Olist(items) => render_olist(out, items, width),
        Block::Table { head, rows } => render_table(out, head, rows, width),
        Block::Blockquote(lines_) => render_blockquote(out, lines_, width),
    }
}

fn render_paragraph(out: &mut Vec<Line<'static>>, text: &str, width: usize) {
    for seg in wrap_text(text, width) {
        let spans = parse_inline(&seg);
        out.push(Line::from(spans));
    }
}

fn render_heading(out: &mut Vec<Line<'static>>, level: &u8, text: &str, width: usize) {
    let prefix = match level {
        1 => "▌",
        2 => "▍",
        _ => "  ",
    };
    let full = format!("{} {}", prefix, text);
    for seg in wrap_text(&full, width) {
        out.push(Line::from(Span::styled(seg, heading_s(*level))));
    }
    out.push(Line::from(""));
}

fn render_hr(out: &mut Vec<Line<'static>>, width: usize) {
    let n = width.min(60);
    let line: String = "─".repeat(n);
    out.push(Line::from(Span::styled(line, hr_s())));
    out.push(Line::from(""));
}

fn render_code_block(out: &mut Vec<Line<'static>>, content: &str, lang: &str, width: usize) {
    let label = if lang.is_empty() { "code".to_string() } else { lang.to_string() };
    let header = format!("─── {} ───", label);
    out.push(Line::from(Span::styled(header, hr_s())));
    let inner_w = width.saturating_sub(4).max(10);
    for line in content.lines() {
        for seg in wrap_text(line, inner_w) {
            out.push(Line::from(Span::styled(format!("  {}", seg), code_block_s())));
        }
    }
    out.push(Line::from(Span::styled("───".to_string(), hr_s())));
    out.push(Line::from(""));
}

fn render_ulist(out: &mut Vec<Line<'static>>, items: &[String], width: usize) {
    let inner = width.saturating_sub(4);
    for item in items {
        for (i, seg) in wrap_text(item, inner).into_iter().enumerate() {
            let spans = if i == 0 {
                vec![
                    Span::styled(" • ", bullet_s()),
                    Span::styled(seg, base()),
                ]
            } else {
                vec![Span::styled("   ", base()), Span::styled(seg, base())]
            };
            out.push(Line::from(spans));
        }
    }
    out.push(Line::from(""));
}

fn render_olist(out: &mut Vec<Line<'static>>, items: &[String], width: usize) {
    let inner = width.saturating_sub(5);
    for (idx, item) in items.iter().enumerate() {
        let n = idx + 1;
        let marker = format!(" {}. ", n);
        let marker_w = marker.chars().count();
        for (i, seg) in wrap_text(item, inner).into_iter().enumerate() {
            let spans = if i == 0 {
                vec![
                    Span::styled(marker.clone(), bullet_s()),
                    Span::styled(seg, base()),
                ]
            } else {
                let pad: String = " ".repeat(marker_w);
                vec![
                    Span::styled(pad, base()),
                    Span::styled(seg, base()),
                ]
            };
            out.push(Line::from(spans));
        }
    }
    out.push(Line::from(""));
}

fn render_table(out: &mut Vec<Line<'static>>, head: &[String], rows: &[Vec<String>], width: usize) {
    if head.is_empty() {
        return;
    }
    let n_cols = head.len();

    // Compute natural column widths from content (in chars, not bytes,
    // so CJK doesn't throw off the alignment). Then shrink proportionally
    // if the total exceeds the available width.
    let mut col_w: Vec<usize> = head.iter().map(|h| h.chars().count()).collect();
    for row in rows {
        for (c, cell) in row.iter().enumerate() {
            if c < n_cols {
                let w = cell.chars().count();
                if w > col_w[c] {
                    col_w[c] = w;
                }
            }
        }
    }
    // Add 1 padding on each side of every cell
    for w in col_w.iter_mut() {
        *w += 2;
    }
    // Total width = sum(col_w) + n_cols+1 (for the | separators)
    let total_chars: usize = col_w.iter().sum::<usize>() + n_cols + 1;
    let avail = width.saturating_sub(1).max(8);
    if total_chars > avail {
        // Scale down the widest columns proportionally
        let over = total_chars - avail;
        let mut remaining = over;
        loop {
            let widest = match col_w.iter().enumerate().max_by_key(|(_, w)| *w) {
                Some((idx, _)) if col_w[idx] > 4 => idx,
                _ => break,
            };
            col_w[widest] = col_w[widest].saturating_sub(1);
            remaining -= 1;
            if remaining == 0 {
                break;
            }
        }
    }

    let render_row = |cells: &[String], style: Style, col_w: &[usize]| -> Line<'static> {
        let mut spans: Vec<Span<'static>> = vec![Span::styled("│", style)];
        for (c, cell) in cells.iter().enumerate() {
            if c >= col_w.len() {
                break;
            }
            let w = col_w[c];
            let pad = w.saturating_sub(cell.chars().count() + 1);
            let truncated = truncate_chars(cell, w.saturating_sub(2));
            spans.push(Span::styled(
                format!(" {}{} ", truncated, " ".repeat(pad)),
                style,
            ));
            spans.push(Span::styled("│", style));
        }
        Line::from(spans)
    };

    // Top border
    let mut top = String::from("┌");
    for (c, w) in col_w.iter().enumerate() {
        top.push_str(&"─".repeat(*w));
        if c + 1 < n_cols {
            top.push('┬');
        }
    }
    top.push('┐');
    out.push(Line::from(Span::styled(top, hr_s())));

    // Header row
    out.push(render_row(head, table_head_s(), &col_w));

    // Header separator
    let mut sep = String::from("├");
    for (c, w) in col_w.iter().enumerate() {
        sep.push_str(&"─".repeat(*w));
        if c + 1 < n_cols {
            sep.push('┼');
        }
    }
    sep.push('┤');
    out.push(Line::from(Span::styled(sep, hr_s())));

    // Body rows
    for row in rows {
        out.push(render_row(row, table_cell_s(), &col_w));
    }

    // Bottom border
    let mut bot = String::from("└");
    for (c, w) in col_w.iter().enumerate() {
        bot.push_str(&"─".repeat(*w));
        if c + 1 < n_cols {
            bot.push('┴');
        }
    }
    bot.push('┘');
    out.push(Line::from(Span::styled(bot, hr_s())));
    out.push(Line::from(""));
}

fn render_blockquote(out: &mut Vec<Line<'static>>, lines_: &[String], width: usize) {
    let inner = width.saturating_sub(4);
    for raw in lines_ {
        for seg in wrap_text(raw, inner) {
            out.push(Line::from(vec![
                Span::styled(" ▎ ", quote_s()),
                Span::styled(seg, quote_s()),
            ]));
        }
    }
    out.push(Line::from(""));
}

// ── Inline parser ──

#[derive(Debug, Clone, PartialEq)]
enum InlineNode {
    Text(String),
    Bold(String),
    Italic(String),
    BoldItalic(String),
    Strike(String),
    Code(String),
    Link { text: String, url: String },
    Math { text: String, display: bool },
}

/// Tokenize `s` into a flat list of inline nodes. Walks the string
/// character-by-character with a stateful cursor so the delimiters
/// don't fight each other (`**bold *and italic* end**` parses as
/// bold(bold " and " italic("and italic") " end")).
fn tokenize_inline(s: &str) -> Vec<InlineNode> {
    let bytes = s.as_bytes();
    let mut nodes: Vec<InlineNode> = Vec::new();
    let mut i = 0;
    let mut text = String::new();

    let flush_text = |nodes: &mut Vec<InlineNode>, text: &mut String| {
        if !text.is_empty() {
            nodes.push(InlineNode::Text(std::mem::take(text)));
        }
    };

    while i < bytes.len() {
        // Inline code: `…`
        if bytes[i] == b'`' {
            if let Some(end) = find_byte(bytes, i + 1, b'`') {
                let code = std::str::from_utf8(&bytes[i + 1..end]).unwrap_or("").to_string();
                if !text.is_empty() {
                    nodes.push(InlineNode::Text(std::mem::take(&mut text)));
                }
                nodes.push(InlineNode::Code(code));
                i = end + 1;
                continue;
            }
        }

        // Math: $$…$$ (display) or $…$ (inline)
        if bytes[i] == b'$' {
            if i + 1 < bytes.len() && bytes[i + 1] == b'$' {
                if let Some(end) = find_subslice(bytes, i + 2, b"$$") {
                    let body = std::str::from_utf8(&bytes[i + 2..end]).unwrap_or("").to_string();
                    if !text.is_empty() {
                        nodes.push(InlineNode::Text(std::mem::take(&mut text)));
                    }
                    nodes.push(InlineNode::Math { text: body, display: true });
                    i = end + 2;
                    continue;
                }
            } else {
                if let Some(end) = find_byte(bytes, i + 1, b'$') {
                    if end > i + 1 {
                        let body = std::str::from_utf8(&bytes[i + 1..end]).unwrap_or("").to_string();
                        if !text.is_empty() {
                            nodes.push(InlineNode::Text(std::mem::take(&mut text)));
                        }
                        nodes.push(InlineNode::Math { text: body, display: false });
                        i = end + 1;
                        continue;
                    }
                }
            }
        }

        // Bold+Italic: ***…*** or ___…___
        if let Some(consumed) = try_triple(bytes, i, b'*') {
            let inner = std::str::from_utf8(&bytes[i + 3..i + consumed - 3]).unwrap_or("");
            let inner_nodes = tokenize_inline(inner);
            if !text.is_empty() {
                nodes.push(InlineNode::Text(std::mem::take(&mut text)));
            }
            for n in inner_nodes {
                if let InlineNode::Text(t) = n {
                    nodes.push(InlineNode::BoldItalic(t));
                } else {
                    nodes.push(n);
                }
            }
            i += consumed;
            continue;
        }
        if let Some(consumed) = try_triple(bytes, i, b'_') {
            let inner = std::str::from_utf8(&bytes[i + 3..i + consumed - 3]).unwrap_or("");
            let inner_nodes = tokenize_inline(inner);
            if !text.is_empty() {
                nodes.push(InlineNode::Text(std::mem::take(&mut text)));
            }
            for n in inner_nodes {
                if let InlineNode::Text(t) = n {
                    nodes.push(InlineNode::BoldItalic(t));
                } else {
                    nodes.push(n);
                }
            }
            i += consumed;
            continue;
        }

        // Bold: **…** or __…__
        if i + 1 < bytes.len()
            && (bytes[i] == b'*' && bytes[i + 1] == b'*'
                || bytes[i] == b'_' && bytes[i + 1] == b'_')
        {
            let delim = [bytes[i], bytes[i + 1]];
            if let Some(end) = find_subslice(bytes, i + 2, &delim) {
                let inner = std::str::from_utf8(&bytes[i + 2..end]).unwrap_or("");
                let inner_nodes = tokenize_inline(inner);
                if !text.is_empty() {
                    nodes.push(InlineNode::Text(std::mem::take(&mut text)));
                }
                for n in inner_nodes {
                    if let InlineNode::Text(t) = n {
                        nodes.push(InlineNode::Bold(t));
                    } else {
                        nodes.push(n);
                    }
                }
                i = end + 2;
                continue;
            }
        }

        // Italic: *…* or _…_
        if bytes[i] == b'*' || bytes[i] == b'_' {
            // Don't treat as italic if it's at the start of a word and forms part
            // of a multi-char delimiter that we've already failed to close.
            let next = if i + 1 < bytes.len() { bytes[i + 1] } else { 0 };
            let prev = if i > 0 { bytes[i - 1] } else { b' ' };
            // Skip if this is inside a word (e.g., "snake_case_var")
            if (prev.is_ascii_alphanumeric() || prev == b'_' || prev == b'*')
                && (next.is_ascii_alphanumeric() || next == b'_' || next == b'*')
            {
                text.push(bytes[i] as char);
                i += 1;
                continue;
            }
            if let Some(end) = find_byte(bytes, i + 1, bytes[i]) {
                let inner = std::str::from_utf8(&bytes[i + 1..end]).unwrap_or("");
                if !inner.is_empty() {
                    let inner_nodes = tokenize_inline(inner);
                    if !text.is_empty() {
                        nodes.push(InlineNode::Text(std::mem::take(&mut text)));
                    }
                    for n in inner_nodes {
                        if let InlineNode::Text(t) = n {
                            nodes.push(InlineNode::Italic(t));
                        } else {
                            nodes.push(n);
                        }
                    }
                    i = end + 1;
                    continue;
                }
            }
        }

        // Strikethrough: ~~…~~
        if i + 1 < bytes.len() && bytes[i] == b'~' && bytes[i + 1] == b'~' {
            if let Some(end) = find_subslice(bytes, i + 2, b"~~") {
                let inner = std::str::from_utf8(&bytes[i + 2..end]).unwrap_or("").to_string();
                if !text.is_empty() {
                    nodes.push(InlineNode::Text(std::mem::take(&mut text)));
                }
                nodes.push(InlineNode::Strike(inner));
                i = end + 2;
                continue;
            }
        }

        // Link: [text](url)
        if bytes[i] == b'[' {
            if let Some((text_, url_, consumed)) = try_link(bytes, i) {
                if !text.is_empty() {
                    nodes.push(InlineNode::Text(std::mem::take(&mut text)));
                }
                nodes.push(InlineNode::Link { text: text_, url: url_ });
                i += consumed;
                continue;
            }
        }

        // Auto-link: bare URL
        if bytes[i].is_ascii_alphanumeric() {
            if let Some((url, consumed)) = try_autolink(bytes, i) {
                if !text.is_empty() {
                    nodes.push(InlineNode::Text(std::mem::take(&mut text)));
                }
                nodes.push(InlineNode::Link { text: url.clone(), url });
                i += consumed;
                continue;
            }
        }

        let c = bytes[i] as char;
        text.push(c);
        i += 1;
    }
    flush_text(&mut nodes, &mut text);
    nodes
}

/// Try to match a triple-delimited run `***…***` (or `___…___`) starting at `i`.
/// Returns the total number of bytes consumed (including delimiters) on success.
fn try_triple(bytes: &[u8], i: usize, ch: u8) -> Option<usize> {
    if i + 5 > bytes.len() {
        return None;
    }
    if bytes[i] != ch || bytes[i + 1] != ch || bytes[i + 2] != ch {
        return None;
    }
    let mut j = i + 3;
    while j + 2 < bytes.len() {
        if bytes[j] == ch && bytes[j + 1] == ch && bytes[j + 2] == ch {
            return Some(j + 3 - i);
        }
        j += 1;
    }
    None
}

/// Try to parse `[text](url)` starting at byte `i` (which is `[`).
/// Returns the link text, URL, and total bytes consumed.
fn try_link(bytes: &[u8], i: usize) -> Option<(String, String, usize)> {
    if bytes[i] != b'[' {
        return None;
    }
    let close_bracket = find_byte(bytes, i + 1, b']')?;
    let text_bytes = &bytes[i + 1..close_bracket];
    let text = std::str::from_utf8(text_bytes).ok()?.to_string();
    let after = close_bracket + 1;
    if after >= bytes.len() || bytes[after] != b'(' {
        return None;
    }
    let close_paren = find_byte(bytes, after + 1, b')')?;
    let url_bytes = &bytes[after + 1..close_paren];
    let url = std::str::from_utf8(url_bytes).ok()?.to_string();
    Some((text, url, close_paren + 1 - i))
}

/// Try to parse a bare autolink `https://…` or `http://…` starting at `i`.
fn try_autolink(bytes: &[u8], i: usize) -> Option<(String, usize)> {
    let rest = &bytes[i..];
    let prefix: &[u8] = if rest.starts_with(b"https://") {
        b"https://"
    } else if rest.starts_with(b"http://") {
        b"http://"
    } else {
        return None;
    };
    let mut j = prefix.len();
    while j < rest.len() {
        let c = rest[j];
        if c.is_ascii_whitespace() || c == b')' || c == b']' || c == b'>' || c == b'<' {
            break;
        }
        j += 1;
    }
    if j == prefix.len() {
        return None;
    }
    let url = std::str::from_utf8(&rest[..j]).ok()?.to_string();
    Some((url, j))
}

fn find_byte(bytes: &[u8], from: usize, ch: u8) -> Option<usize> {
    let mut i = from;
    while i < bytes.len() {
        if bytes[i] == ch {
            return Some(i);
        }
        i += 1;
    }
    None
}

fn find_subslice(bytes: &[u8], from: usize, needle: &[u8]) -> Option<usize> {
    if needle.is_empty() || from + needle.len() > bytes.len() {
        return None;
    }
    let mut i = from;
    while i + needle.len() <= bytes.len() {
        if &bytes[i..i + needle.len()] == needle {
            return Some(i);
        }
        i += 1;
    }
    None
}

fn parse_inline(line: &str) -> Vec<Span<'static>> {
    let nodes = tokenize_inline(line);
    let mut spans: Vec<Span<'static>> = Vec::new();
    for node in nodes {
        render_node(&mut spans, node);
    }
    if spans.is_empty() {
        spans.push(Span::styled(String::new(), base()));
    }
    spans
}

fn render_node(spans: &mut Vec<Span<'static>>, node: InlineNode) {
    match node {
        InlineNode::Text(t) => {
            push_styled(spans, t, base());
        }
        InlineNode::Bold(t) => {
            push_styled(spans, t, bold_s());
        }
        InlineNode::Italic(t) => {
            push_styled(spans, t, italic_s());
        }
        InlineNode::BoldItalic(t) => {
            push_styled(spans, t, bold_italic_s());
        }
        InlineNode::Strike(t) => {
            push_styled(spans, t, del_s());
        }
        InlineNode::Code(t) => {
            push_styled(spans, format!(" {} ", t), code_inline_s());
        }
        InlineNode::Link { text, url } => {
            // Show "text (url)" since terminals can't show hover previews.
            let display = if text == url {
                text
            } else {
                format!("{} ({})", text, truncate_chars(&url, 40))
            };
            push_styled(spans, display, link_s());
        }
        InlineNode::Math { text, display } => {
            // Terminals can't render LaTeX, so we wrap in $…$ with a
            // distinct color so users see the formula is preserved.
            let body = if display {
                format!("\n  $$ {} $$\n", text)
            } else {
                format!("${}$", text)
            };
            push_styled(spans, body, code_inline_s());
        }
    }
}

fn push_styled(spans: &mut Vec<Span<'static>>, text: String, style: Style) {
    if text.is_empty() {
        return;
    }
    if let Some(last) = spans.last_mut() {
        if last.style == style {
            last.content.to_mut().push_str(&text);
            return;
        }
    }
    spans.push(Span::styled(text, style));
}

fn truncate_chars(s: &str, max: usize) -> String {
    let count = s.chars().count();
    if count <= max {
        return s.to_string();
    }
    let keep = max.saturating_sub(1);
    let mut out: String = s.chars().take(keep).collect();
    out.push('…');
    out
}

// ── Text wrapping ──

fn wrap_text(s: &str, width: usize) -> Vec<String> {
    if width == 0 || s.is_empty() {
        return vec![s.to_string()];
    }
    let mut out: Vec<String> = Vec::new();
    for raw in s.split('\n') {
        if raw.is_empty() {
            out.push(String::new());
            continue;
        }
        let chars: Vec<char> = raw.chars().collect();
        let mut start = 0;
        while start < chars.len() {
            let end = (start + width).min(chars.len());
            out.push(chars[start..end].iter().collect());
            start = end;
        }
    }
    if out.is_empty() {
        out.push(String::new());
    }
    out
}

// ── Tests ──

#[cfg(test)]
mod tests {
    use super::*;

    fn flat(lines: &[Line<'_>]) -> String {
        lines.iter().flat_map(|l| l.spans.iter()).map(|s| s.content.as_ref()).collect()
    }

    #[test]
    fn plain_text_renders() {
        let lines = render_markdown("hello world", 80);
        let f = flat(&lines);
        assert!(f.contains("hello world"));
    }

    #[test]
    fn bold_renders() {
        let lines = render_markdown("**bold** text", 80);
        let f = flat(&lines);
        assert!(f.contains("bold"));
        // The "bold" span should carry the BOLD modifier
        let has_bold_span = lines
            .iter()
            .flat_map(|l| l.spans.iter())
            .any(|s| s.content.contains("bold") && s.style.add_modifier.contains(Modifier::BOLD));
        assert!(has_bold_span, "expected bold span with BOLD modifier");
    }

    #[test]
    fn italic_renders() {
        let lines = render_markdown("*italic* text", 80);
        let has_italic = lines
            .iter()
            .flat_map(|l| l.spans.iter())
            .any(|s| s.content.contains("italic") && s.style.add_modifier.contains(Modifier::ITALIC));
        assert!(has_italic, "expected italic span with ITALIC modifier");
    }

    #[test]
    fn strikethrough_renders() {
        let lines = render_markdown("~~del~~ text", 80);
        let has_strike = lines
            .iter()
            .flat_map(|l| l.spans.iter())
            .any(|s| s.content.contains("del") && s.style.add_modifier.contains(Modifier::CROSSED_OUT));
        assert!(has_strike, "expected strikethrough span");
    }

    #[test]
    fn inline_code_renders() {
        let lines = render_markdown("use `code` here", 80);
        let has_code = lines
            .iter()
            .flat_map(|l| l.spans.iter())
            .any(|s| s.content.contains("code") && s.style.add_modifier.contains(Modifier::ITALIC));
        assert!(has_code, "expected inline code span");
    }

    #[test]
    fn fenced_code_block_renders() {
        let lines = render_markdown("before\n```rust\nfn main() {}\n```\nafter", 80);
        let f = flat(&lines);
        assert!(f.contains("fn main() {}"), "code body missing: {f}");
        assert!(f.contains("before"));
        assert!(f.contains("after"));
    }

    #[test]
    fn code_block_protects_inner_markers() {
        let lines = render_markdown("```\n**not bold**\n```", 80);
        let f = flat(&lines);
        assert!(f.contains("**not bold**"), "code block should preserve raw markers");
    }

    #[test]
    fn inline_code_protects_inner_markers() {
        let lines = render_markdown("`**not bold**`", 80);
        let f = flat(&lines);
        assert!(f.contains("**not bold**"), "inline code should preserve raw markers");
    }

    #[test]
    fn headings_h1_to_h6() {
        for level in 1u8..=6 {
            let prefix = "#".repeat(level as usize);
            let lines = render_markdown(&format!("{} title", prefix), 80);
            assert!(!lines.is_empty(), "h{level} produced no lines");
            let f = flat(&lines);
            assert!(f.contains("title"), "h{level} content missing");
        }
    }

    #[test]
    fn horizontal_rule_renders() {
        let lines = render_markdown("---", 80);
        let f = flat(&lines);
        assert!(f.contains("─"), "hr should render a horizontal line");
    }

    #[test]
    fn blockquote_renders() {
        let lines = render_markdown("> quoted text", 80);
        let f = flat(&lines);
        assert!(f.contains("quoted text"), "blockquote content missing: {f}");
    }

    #[test]
    fn unordered_list_renders() {
        let lines = render_markdown("- a\n- b\n- c", 80);
        let f = flat(&lines);
        assert!(f.contains("a") && f.contains("b") && f.contains("c"));
        // Should contain bullet markers
        assert!(f.contains("•") || f.contains("-"));
    }

    #[test]
    fn ordered_list_renders() {
        let lines = render_markdown("1. first\n2. second", 80);
        let f = flat(&lines);
        assert!(f.contains("first"));
        assert!(f.contains("second"));
        assert!(f.contains("1."));
        assert!(f.contains("2."));
    }

    #[test]
    fn table_renders_with_borders() {
        let md = "| Name | Age |\n| --- | --- |\n| a | 1 |";
        let lines = render_markdown(md, 80);
        let f = flat(&lines);
        assert!(f.contains("Name"));
        assert!(f.contains("Age"));
        assert!(f.contains("a"));
        assert!(f.contains("1"));
        // Should use box-drawing characters
        assert!(f.contains("┌") || f.contains("│") || f.contains("─"));
    }

    #[test]
    fn nested_bold_italic() {
        let lines = render_markdown("**bold *and italic* end**", 80);
        let f = flat(&lines);
        assert!(f.contains("bold"));
        assert!(f.contains("and italic"));
        assert!(f.contains("end"));
    }

    #[test]
    fn link_renders() {
        let lines = render_markdown("click [here](https://example.com) now", 80);
        let f = flat(&lines);
        assert!(f.contains("here"));
        assert!(f.contains("https://example.com"));
    }

    #[test]
    fn math_inline_renders() {
        let lines = render_markdown("Einstein $E = mc^2$ formula", 80);
        let f = flat(&lines);
        assert!(f.contains("E = mc^2"));
    }

    #[test]
    fn math_display_renders() {
        let lines = render_markdown("Block $$x = \\frac{-b}{2a}$$", 80);
        let f = flat(&lines);
        assert!(f.contains("x = \\frac"));
    }

    #[test]
    fn complex_markdown_combined() {
        let md = "# Title\n\nThis is **bold** and *italic* with `code`.\n\n- item 1\n- item 2\n\n| h1 | h2 |\n| --- | --- |\n| a | b |";
        let lines = render_markdown(md, 80);
        let f = flat(&lines);
        assert!(f.contains("Title"));
        assert!(f.contains("bold"));
        assert!(f.contains("italic"));
        assert!(f.contains("code"));
        assert!(f.contains("item 1"));
        assert!(f.contains("h1"));
        assert!(f.contains("a"));
    }

    #[test]
    fn empty_input() {
        let lines = render_markdown("", 80);
        assert!(lines.is_empty());
    }

    #[test]
    fn thinking_tag_stripped() {
        let lines = render_markdown("hello <thinking>secret</thinking> world", 80);
        let f = flat(&lines);
        assert!(f.contains("hello"));
        assert!(f.contains("world"));
        assert!(!f.contains("thinking"));
        assert!(!f.contains("secret"));
    }
}
