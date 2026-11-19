//! Markdown → plain text for desktop notification banners.
//!
//! System notification banners render plain text only, so a model reply that
//! starts with `### 完成情况` or `**已修复**` used to arrive verbatim in the
//! bubble — "### 完成情况" reads like leaked source rather than a message.
//!
//! Three entry points, used at different distances from the banner:
//!
//! - [`strip_markdown`] — normalize markers but keep the line structure.
//! - [`compact`] — flatten the result to one banner line and cap its length.
//! - [`summary`] — pick the first *informative* line of a reply (skipping
//!   headings, code fences and tables) so the banner shows the conclusion
//!   instead of the document skeleton.
//!
//! The functions are lossless enough to be safe on ordinary prose: a bare
//! `#hashtag`, `snake_case` identifiers, `Vec<String>` and shell pipes
//! survive unchanged.

/// Which structural role a source line played. `summary` uses it to prefer
/// prose over document furniture.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum LineKind {
    Heading,
    Prose,
    Table,
    /// Blank line / horizontal rule / table separator — carries no text.
    Break,
}

/// Convert markdown-ish text to plain text, keeping paragraph breaks.
pub fn strip_markdown(input: &str) -> String {
    let mut lines: Vec<String> = Vec::new();
    let mut in_fence = false;
    for raw in input.lines() {
        let trimmed = raw.trim();
        if is_fence(trimmed) {
            in_fence = !in_fence;
            continue;
        }
        if in_fence {
            // Code is content, not scaffolding: keep its bytes, minus fences.
            lines.push(raw.trim_end().to_string());
            continue;
        }
        if trimmed.is_empty() {
            lines.push(String::new());
            continue;
        }
        let (text, kind) = render_line(raw);
        if kind == LineKind::Break && !trimmed.is_empty() {
            // Horizontal rules and table separators are scaffolding, not
            // paragraph breaks: dropping them keeps table rows together.
            continue;
        }
        lines.push(text);
    }

    // Trim and collapse runs of blank lines.
    let mut out = String::new();
    for line in lines {
        let line = line.trim_end();
        if line.is_empty() {
            if !out.is_empty() && !out.ends_with("\n\n") {
                out.push('\n');
            }
            continue;
        }
        out.push_str(line);
        out.push('\n');
    }
    out.trim_end_matches('\n').to_string()
}

/// Flatten text to a single banner line, capped at roughly `max_chars`
/// characters (a trailing `…` marks truncation).
pub fn compact(input: &str, max_chars: usize) -> String {
    let plain = strip_markdown(input);
    let mut flat = String::new();
    for line in plain.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        if !flat.is_empty() {
            flat.push(' ');
        }
        flat.push_str(line);
    }
    truncate_chars(&flat, max_chars)
}

/// Pick the first informative line of a markdown reply and flatten it.
///
/// Headings are skipped when real prose exists (a banner that already says
/// "任务完成" does not need the reply's `## 完成情况` heading), and code
/// fences / tables are only used as a last resort.
pub fn summary(input: &str, max_chars: usize) -> String {
    let mut in_fence = false;
    let mut heading: Option<String> = None;
    let mut table: Option<String> = None;
    let mut prose: Option<String> = None;

    for raw in input.lines() {
        let trimmed = raw.trim();
        if is_fence(trimmed) {
            in_fence = !in_fence;
            continue;
        }
        if in_fence || trimmed.is_empty() {
            continue;
        }
        let (text, kind) = render_line(raw);
        let text = text.trim();
        if text.is_empty() {
            continue;
        }
        match kind {
            LineKind::Heading => heading.get_or_insert_with(|| text.to_string()),
            LineKind::Table => table.get_or_insert_with(|| text.to_string()),
            LineKind::Prose => {
                prose = Some(text.to_string());
                break;
            }
            LineKind::Break => continue,
        };
    }

    let chosen = prose.or(heading).or(table).unwrap_or_default();
    compact(&chosen, max_chars)
}

// ── line-level rendering ──────────────────────────────────────────────

fn render_line(line: &str) -> (String, LineKind) {
    let mut s = line.trim();
    // Blockquote markers (`> `, `>> `) are structural, not words.
    while let Some(rest) = s.strip_prefix('>') {
        s = rest.trim_start();
    }
    if s.is_empty() {
        return (String::new(), LineKind::Break);
    }
    if is_hr(s) || is_table_separator(s) {
        return (String::new(), LineKind::Break);
    }
    if let Some(text) = heading_text(s) {
        let text = strip_closing_hashes(text.trim_start());
        return (inline(&text), LineKind::Heading);
    }
    if is_table_row(s) {
        let cells: Vec<String> = s
            .trim_matches('|')
            .split('|')
            .map(|c| inline(c.trim()))
            .filter(|c| !c.trim().is_empty())
            .collect();
        return (cells.join(" · "), LineKind::Table);
    }
    if let Some(text) = bullet_text(s) {
        return (format!("• {}", inline(text)), LineKind::Prose);
    }
    (inline(s), LineKind::Prose)
}

fn is_fence(trimmed: &str) -> bool {
    trimmed.starts_with("```") || trimmed.starts_with("~~~")
}

/// `### 标题` → `标题`. Returns `None` for `#hashtag` / `C# 语言` style text
/// where the hashes are not a heading marker (markdown requires a space).
fn heading_text(line: &str) -> Option<&str> {
    let hashes = line.chars().take_while(|c| *c == '#').count();
    if hashes == 0 || hashes > 6 {
        return None;
    }
    let rest = &line[hashes..];
    if rest.is_empty() {
        return Some("");
    }
    if rest.starts_with(' ') || rest.starts_with('\t') {
        Some(rest)
    } else {
        None
    }
}

/// ATX headings may be closed with trailing `#` (`## 标题 ##`).
fn strip_closing_hashes(text: &str) -> String {
    let t = text.trim_end();
    if t.ends_with('#') {
        let body = t.trim_end_matches('#').trim_end();
        if body.len() < t.len() && t[body.len()..].starts_with(' ') {
            return body.to_string();
        }
    }
    t.to_string()
}

fn bullet_text(line: &str) -> Option<&str> {
    for marker in ["- ", "* ", "+ "] {
        if let Some(rest) = line.strip_prefix(marker) {
            return Some(rest.trim_start());
        }
    }
    let digits = line.chars().take_while(|c| c.is_ascii_digit()).count();
    if (1..=3).contains(&digits) {
        let rest = &line[digits..];
        if let Some(r) = rest.strip_prefix(". ").or_else(|| rest.strip_prefix(") ")) {
            return Some(r.trim_start());
        }
    }
    None
}

fn is_hr(line: &str) -> bool {
    let mut chars = line.chars().filter(|c| !c.is_whitespace());
    let Some(first) = chars.next() else {
        return false;
    };
    if !matches!(first, '-' | '*' | '_' | '=') {
        return false;
    }
    let mut count = 1;
    for c in chars {
        if c != first {
            return false;
        }
        count += 1;
    }
    count >= 3
}

fn is_table_separator(line: &str) -> bool {
    let t = line.trim();
    if !t.contains('-') {
        return false;
    }
    if !t.chars().all(|c| matches!(c, '|' | '-' | ':' | ' ' | '\t')) {
        return false;
    }
    t.chars().filter(|c| *c == '-').count() >= 3
}

/// A table row in the shapes LLM replies actually emit (`| a | b |`).
fn is_table_row(line: &str) -> bool {
    let pipes = line.chars().filter(|c| *c == '|').count();
    pipes >= 2 && (line.starts_with('|') || line.ends_with('|'))
}

// ── inline rendering ──────────────────────────────────────────────────

/// Strip inline markers from one line: code spans, links/images, autolinks,
/// a small set of HTML tags, entities, then emphasis markers.
fn inline(line: &str) -> String {
    let chars: Vec<char> = line.chars().collect();
    let mut out = String::new();
    let mut i = 0;
    while i < chars.len() {
        if chars[i] == '`' {
            let run = count_run(&chars, i, '`');
            match find_run(&chars, i + run, '`', run) {
                // A code span's content is verbatim — do not touch its `**`.
                Some((close_start, close_end)) => {
                    out.extend(&chars[i + run..close_start]);
                    i = close_end;
                }
                // Unmatched backticks are noise; drop them.
                None => i += run,
            }
            continue;
        }
        let start = i;
        while i < chars.len() && chars[i] != '`' {
            i += 1;
        }
        let segment: String = chars[start..i].iter().collect();
        out.push_str(&decorate(&segment));
    }
    out
}

fn count_run(chars: &[char], at: usize, c: char) -> usize {
    let mut n = 0;
    while at + n < chars.len() && chars[at + n] == c {
        n += 1;
    }
    n
}

/// Find a run of exactly `len` `c` starting at `from`. Returns byte-free
/// character indices `(start, end)`.
fn find_run(chars: &[char], from: usize, c: char, len: usize) -> Option<(usize, usize)> {
    let mut i = from;
    while i < chars.len() {
        if chars[i] == c {
            let n = count_run(chars, i, c);
            if n == len {
                return Some((i, i + n));
            }
            i += n;
        } else {
            i += 1;
        }
    }
    None
}

fn decorate(s: &str) -> String {
    let s = unwrap_links(s);
    let s = unwrap_autolinks(&s);
    let s = strip_html_tags(&s);
    let s = decode_entities(&s);
    strip_emphasis(&s)
}

/// `[text](url)` → `text`, `![alt](url)` → `alt`.
fn unwrap_links(s: &str) -> String {
    let chars: Vec<char> = s.chars().collect();
    let mut out = String::new();
    let mut i = 0;
    while i < chars.len() {
        let image = chars[i] == '!' && i + 1 < chars.len() && chars[i + 1] == '[';
        if image || chars[i] == '[' {
            let open = if image { i + 1 } else { i };
            if let Some(close) = find_char(&chars, open + 1, ']') {
                if close + 1 < chars.len() && chars[close + 1] == '(' {
                    if let Some(paren) = find_char(&chars, close + 2, ')') {
                        let label: String = chars[open + 1..close].iter().collect();
                        let url: String = chars[close + 2..paren].iter().collect();
                        let label = label.trim();
                        if !label.is_empty() {
                            out.push_str(label);
                        } else if !image {
                            out.push_str(url.trim());
                        }
                        i = paren + 1;
                        continue;
                    }
                }
            }
        }
        out.push(chars[i]);
        i += 1;
    }
    out
}

fn find_char(chars: &[char], from: usize, target: char) -> Option<usize> {
    (from..chars.len()).find(|i| chars[*i] == target)
}

/// `<https://example.com>` → `https://example.com`.
fn unwrap_autolinks(s: &str) -> String {
    let chars: Vec<char> = s.chars().collect();
    let mut out = String::new();
    let mut i = 0;
    while i < chars.len() {
        if chars[i] == '<' {
            if let Some(end) = find_char(&chars, i + 1, '>') {
                let inner: String = chars[i + 1..end].iter().collect();
                let t = inner.trim();
                if t.starts_with("http://") || t.starts_with("https://") || t.starts_with("mailto:")
                {
                    out.push_str(t);
                    i = end + 1;
                    continue;
                }
            }
        }
        out.push(chars[i]);
        i += 1;
    }
    out
}

/// Remove a small, explicit set of HTML tags. Unknown `<...>` (generics,
/// comparisons) is left alone — `Vec<String>` must survive.
fn strip_html_tags(s: &str) -> String {
    const TAGS: &[&str] = &[
        "br",
        "b",
        "strong",
        "i",
        "em",
        "u",
        "s",
        "del",
        "code",
        "pre",
        "span",
        "div",
        "p",
        "a",
        "li",
        "ul",
        "ol",
        "h1",
        "h2",
        "h3",
        "h4",
        "h5",
        "h6",
        "kbd",
        "sup",
        "sub",
        "mark",
        "small",
        "table",
        "tr",
        "td",
        "th",
        "thead",
        "tbody",
        "blockquote",
        "hr",
        "img",
        "details",
        "summary",
    ];
    let chars: Vec<char> = s.chars().collect();
    let mut out = String::new();
    let mut i = 0;
    while i < chars.len() {
        if chars[i] == '<' {
            if let Some(end) = find_char(&chars, i + 1, '>') {
                let inner: String = chars[i + 1..end].iter().collect();
                let name = inner
                    .trim_start_matches('/')
                    .trim_end_matches('/')
                    .split_whitespace()
                    .next()
                    .unwrap_or("")
                    .to_ascii_lowercase();
                if TAGS.contains(&name.as_str()) {
                    // `<br>` was a hard line break; a space keeps the words apart.
                    out.push(' ');
                    i = end + 1;
                    continue;
                }
            }
        }
        out.push(chars[i]);
        i += 1;
    }
    out
}

fn decode_entities(s: &str) -> String {
    let mut t = s.to_string();
    for (from, to) in [
        ("&nbsp;", " "),
        ("&amp;", "&"),
        ("&lt;", "<"),
        ("&gt;", ">"),
        ("&quot;", "\""),
        ("&#39;", "'"),
        ("&apos;", "'"),
        ("&mdash;", "—"),
        ("&ndash;", "–"),
        ("&hellip;", "…"),
        ("&times;", "×"),
        ("&middot;", "·"),
    ] {
        if t.contains(from) {
            t = t.replace(from, to);
        }
    }
    t
}

fn strip_emphasis(s: &str) -> String {
    // `**bold**` / `__bold__` next to CJK text carries no spaces, so the
    // double-star form must not demand word boundaries. Single `_` does,
    // otherwise `snake_case` and `file_name.rs` lose their underscores.
    let mut t = strip_marker(s, '*', true, false, 1);
    t = strip_marker(&t, '_', true, true, 1);
    t = strip_marker(&t, '~', true, false, 1);
    t = strip_marker(&t, '*', false, false, 2);
    t = strip_marker(&t, '_', false, true, 1);
    t
}

/// Remove paired emphasis markers (`**`, `__`, `~~` when `double`, otherwise
/// `*`, `_`).
///
/// `require_boundary` keeps identifiers (`snake_case`) intact; `min_content`
/// keeps arithmetic (`2*3*4`) intact while still stripping `*重点*` in CJK
/// text, where `is_alphanumeric` is true for the neighbouring characters.
fn strip_marker(
    s: &str,
    marker: char,
    double: bool,
    require_boundary: bool,
    min_content: usize,
) -> String {
    let chars: Vec<char> = s.chars().collect();
    let want = if double { 2 } else { 1 };
    let mut out = String::new();
    let mut i = 0;
    while i < chars.len() {
        if chars[i] != marker {
            out.push(chars[i]);
            i += 1;
            continue;
        }
        let run = count_run(&chars, i, marker);
        if run < want {
            for _ in 0..run {
                out.push(marker);
            }
            i += run;
            continue;
        }

        let open_end = i + want;
        let open_ok = !require_boundary || i == 0 || !chars[i - 1].is_alphanumeric();
        let mut j = open_end;
        let mut close = None;
        while j < chars.len() {
            if chars[j] == marker && count_run(&chars, j, marker) >= want {
                close = Some(j);
                break;
            }
            j += 1;
        }

        let stripped = close.and_then(|cs| {
            let ce = cs + want;
            let content = &chars[open_end..cs];
            let close_ok = !require_boundary || ce >= chars.len() || !chars[ce].is_alphanumeric();
            let content_ok = content.len() >= min_content
                && !content[0].is_whitespace()
                && !content[content.len() - 1].is_whitespace();
            (open_ok && close_ok && content_ok).then_some((cs, ce))
        });

        match stripped {
            Some((cs, ce)) => {
                out.extend(&chars[open_end..cs]);
                i = ce;
            }
            None => {
                for _ in 0..run {
                    out.push(marker);
                }
                i += run;
            }
        }
    }
    out
}

fn truncate_chars(s: &str, max_chars: usize) -> String {
    if max_chars == 0 {
        return String::new();
    }
    if s.chars().count() <= max_chars {
        return s.to_string();
    }
    let mut out: String = s.chars().take(max_chars.saturating_sub(1)).collect();
    out.push('…');
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn heading_markers_are_stripped() {
        let md = "### 完成情况\n\n已交付 `demo/x.html`。";
        assert_eq!(strip_markdown(md), "完成情况\n\n已交付 demo/x.html。");
    }

    #[test]
    fn emphasis_and_code_markers_are_stripped() {
        assert_eq!(
            strip_markdown("**已修复** __粗体__ ~~删除~~ *斜体* `cargo test`"),
            "已修复 粗体 删除 斜体 cargo test"
        );
    }

    #[test]
    fn identifiers_and_paths_survive() {
        // Underscores inside words are not emphasis; generics are not tags.
        assert_eq!(
            strip_markdown("edit demo/build_bamboo.py and `my_file.rs`, then Vec<String>"),
            "edit demo/build_bamboo.py and my_file.rs, then Vec<String>"
        );
    }

    #[test]
    fn emphasis_works_without_spaces_in_cjk_text() {
        assert_eq!(strip_markdown("已**修复**完成"), "已修复完成");
        assert_eq!(strip_markdown("这是*重点*内容"), "这是重点内容");
    }

    #[test]
    fn arithmetic_and_unclosed_markers_are_left_alone() {
        assert_eq!(strip_markdown("2*3*4 = 24"), "2*3*4 = 24");
        assert_eq!(strip_markdown("**未闭合"), "**未闭合");
        assert_eq!(strip_markdown("a ** b"), "a ** b");
    }

    #[test]
    fn real_reply_summary_uses_the_conclusion_not_the_heading() {
        // Verbatim opening of a real session reply.
        let md = "## 做了什么\n\n**调研**：zcode 的会话库在 `~/.zcode/cli/db/db.sqlite`（`session` / `message` / `part` 三表）。按 `directory LIKE '%visualmusic%'` 查出 **33 个会话**。";
        let got = summary(md, 110);
        assert!(
            got.starts_with("调研：zcode 的会话库在 ~/.zcode/cli/db/db.sqlite"),
            "got: {got}"
        );
        assert!(!got.contains('#') && !got.contains('*') && !got.contains('`'));
    }

    #[test]
    fn summary_skips_code_fences() {
        let md = "### 交付\n\n```bash\nnpm run build\n```\n\n已生成 `demo/pine.html` 并在右侧边栏打开预览。";
        assert_eq!(
            summary(md, 110),
            "已生成 demo/pine.html 并在右侧边栏打开预览。"
        );
    }

    #[test]
    fn heading_only_reply_still_produces_text() {
        assert_eq!(summary("## 任务完成 ##", 110), "任务完成");
        assert_eq!(summary("### 完成", 110), "完成");
    }

    #[test]
    fn bullet_lists_become_bullets() {
        assert_eq!(
            strip_markdown("- 第一项\n- **第二项**"),
            "• 第一项\n• 第二项"
        );
        assert_eq!(strip_markdown("1. 第一步"), "• 第一步");
    }

    #[test]
    fn tables_become_readable_rows() {
        let md = "| 会话 | 时间 |\n|---|---|\n| sess_1 | 10-05 |";
        assert_eq!(strip_markdown(md), "会话 · 时间\nsess_1 · 10-05");
        assert_eq!(summary(md, 110), "会话 · 时间");
    }

    #[test]
    fn links_and_images_keep_their_text() {
        assert_eq!(
            strip_markdown("见 [文档](https://example.com/a_b) 与 ![封面](https://x/y.png)"),
            "见 文档 与 封面"
        );
        assert_eq!(
            strip_markdown("自动链接 <https://example.com/x>"),
            "自动链接 https://example.com/x"
        );
    }

    #[test]
    fn blockquotes_and_rules_are_removed() {
        assert_eq!(
            strip_markdown("> 引用一句\n\n---\n\n结尾"),
            "引用一句\n\n结尾"
        );
    }

    #[test]
    fn plain_text_passes_through() {
        let plain = "已完成：通知正文改成了纯文本（110 字以内）。";
        assert_eq!(strip_markdown(plain), plain);
        assert_eq!(summary(plain, 110), plain);
    }

    #[test]
    fn hashtags_are_not_headings() {
        assert_eq!(strip_markdown("#hashtag 和 C# 语言"), "#hashtag 和 C# 语言");
    }

    #[test]
    fn unicode_truncation_is_char_safe() {
        let long = "改".repeat(200);
        let got = summary(&long, 20);
        assert_eq!(got.chars().count(), 20);
        assert!(got.ends_with('…'));
        // Astral-plane characters must not be split.
        let emoji = "🚀".repeat(30);
        assert_eq!(summary(&emoji, 10).chars().count(), 10);
    }

    #[test]
    fn compact_flattens_multiline_and_caps_length() {
        let md = "# 标题\n\n第一段。\n\n第二段。";
        let got = compact(md, 100);
        assert_eq!(got, "标题 第一段。 第二段。");
        assert!(compact(md, 6).chars().count() <= 6);
    }

    #[test]
    fn empty_input_is_empty() {
        assert_eq!(strip_markdown(""), "");
        assert_eq!(summary("   \n\n ", 110), "");
        assert_eq!(compact("", 110), "");
    }

    #[test]
    fn sanitized_text_is_stable_when_sanitized_again() {
        let md = "### 结果\n\n- **第一**\n- `第二`\n\n| a | b |\n|---|---|\n| 1 | 2 |";
        let once = strip_markdown(md);
        assert_eq!(strip_markdown(&once), once);
        let head = summary(md, 110);
        assert_eq!(summary(&head, 110), head);
    }
}
