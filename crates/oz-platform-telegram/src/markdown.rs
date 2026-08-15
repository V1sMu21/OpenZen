pub fn to_telegram_markdown_v2(text: &str) -> String {
    let mut result = String::with_capacity(text.len());
    let chars: Vec<char> = text.chars().collect();
    let mut i = 0;
    let len = chars.len();

    while i < len {
        if chars[i] == '`' {
            let fence_len = count_consecutive(&chars, i, '`');
            if fence_len >= 3 {
                let lang_end = find_line_end_or(&chars, i + fence_len, '\n');
                let lang = chars[i + fence_len..lang_end].iter().collect::<String>();
                if let Some(close) = find_closing_fence(&chars, lang_end, fence_len) {
                    let code = &chars[lang_end..close].iter().collect::<String>();
                    let code_escaped = escape_pre(code);
                    if lang.trim().is_empty() {
                        result.push_str(&format!("```\n{code_escaped}\n```"));
                    } else {
                        result.push_str(&format!("```{}\n{code_escaped}\n```", lang.trim()));
                    }
                    i = close + fence_len;
                    continue;
                }
            }
            let code_end = find_matching(&chars, i + 1, '`');
            let code = chars[i + 1..code_end].iter().collect::<String>();
            result.push_str(&format!("`{}`", escape_code(&code)));
            i = code_end + 1;
            continue;
        }

        if chars[i] == '*' && i + 1 < len && chars[i + 1] == '*' {
            let bold_end = find_subsequence(&chars, i + 2, &['*', '*']);
            if bold_end < len {
                let text: String = chars[i + 2..bold_end].iter().collect();
                result.push_str(&format!("*{}*", escape_markdown_v2(&text)));
                i = bold_end + 2;
                continue;
            }
        }

        if chars[i] == '_' && i + 1 < len && chars[i + 1] != '_' {
            let italic_end = find_matching(&chars, i + 1, '_');
            if italic_end < len {
                let text: String = chars[i + 1..italic_end].iter().collect();
                result.push_str(&format!("_{}_", escape_markdown_v2(&text)));
                i = italic_end + 1;
                continue;
            }
        }

        if chars[i] == '[' {
            if let Some((label, url, end)) = parse_link(&chars, i) {
                result.push_str(&format!(
                    "[{}]({})",
                    escape_markdown_v2(&label),
                    escape_link_target(&url)
                ));
                i = end;
                continue;
            }
        }

        if is_special_char(chars[i]) {
            result.push('\\');
        }
        result.push(chars[i]);
        i += 1;
    }

    result
}

pub fn split_into_segments(text: &str, max_len: usize) -> Vec<String> {
    let mut segments = Vec::new();
    let mut remaining = text;

    while !remaining.is_empty() {
        let r = to_telegram_markdown_v2(remaining);
        if r.len() <= max_len {
            segments.push(remaining.to_string());
            break;
        }

        let mut lo = 1;
        let mut hi = remaining.len();
        let mut best = 1;
        while lo <= hi {
            let mid = (lo + hi) / 2;
            let candidate = &remaining[..mid];
            let rendered = to_telegram_markdown_v2(candidate);
            if rendered.len() <= max_len {
                best = mid;
                lo = mid + 1;
            } else {
                hi = mid - 1;
            }
        }

        let cut = remaining[..best]
            .rfind('\n')
            .filter(|&pos| pos > best * 60 / 100)
            .unwrap_or(best);

        segments.push(remaining[..cut].trim_end().to_string());
        remaining = remaining[cut..].trim_start();
    }

    if segments.is_empty() {
        segments.push("...".into());
    }
    segments
}

pub fn trim_to_fit(text: &str, max_len: usize) -> String {
    if text.len() <= max_len {
        return text.to_string();
    }
    let mut result = text[..max_len.saturating_sub(3)].to_string();
    result.push_str("...");
    result
}

fn escape_markdown_v2(text: &str) -> String {
    text.chars()
        .map(|c| {
            if is_special_char(c) {
                format!("\\{c}")
            } else {
                c.to_string()
            }
        })
        .collect()
}

fn escape_pre(text: &str) -> String {
    text.replace('`', "\\`").replace('\\', "\\\\")
}

fn escape_code(text: &str) -> String {
    text.replace('`', "\\`").replace('\\', "\\\\")
}

fn escape_link_target(text: &str) -> String {
    text.replace(')', "\\)").replace('\\', "\\\\")
}

fn is_special_char(c: char) -> bool {
    matches!(
        c,
        '_' | '*'
            | '['
            | ']'
            | '('
            | ')'
            | '~'
            | '`'
            | '>'
            | '#'
            | '+'
            | '-'
            | '='
            | '|'
            | '{'
            | '}'
            | '.'
            | '!'
    )
}

fn count_consecutive(chars: &[char], start: usize, target: char) -> usize {
    let mut count = 0;
    for &c in &chars[start..] {
        if c == target {
            count += 1;
        } else {
            break;
        }
    }
    count
}

fn find_line_end_or(chars: &[char], start: usize, delimiter: char) -> usize {
    for (i, &c) in chars.iter().enumerate().skip(start) {
        if c == delimiter {
            return i;
        }
    }
    chars.len()
}

fn find_matching(chars: &[char], start: usize, target: char) -> usize {
    for (i, &c) in chars.iter().enumerate().skip(start) {
        if c == target {
            return i;
        }
    }
    chars.len()
}

fn find_subsequence(chars: &[char], start: usize, pattern: &[char]) -> usize {
    for i in start..chars.len().saturating_sub(pattern.len() - 1) {
        if chars[i..].starts_with(pattern) {
            return i;
        }
    }
    chars.len()
}

fn find_closing_fence(chars: &[char], start: usize, fence_len: usize) -> Option<usize> {
    let mut i = start;
    while i + fence_len <= chars.len() {
        if chars[i] == '`' && count_consecutive(chars, i, '`') >= fence_len {
            return Some(i);
        }
        i += 1;
    }
    None
}

fn parse_link(chars: &[char], start: usize) -> Option<(String, String, usize)> {
    let label_start = start + 1;
    let label_end = find_matching(chars, label_start, ']');
    if label_end >= chars.len() {
        return None;
    }
    if label_end + 1 >= chars.len() || chars[label_end + 1] != '(' {
        return None;
    }
    let url_start = label_end + 2;
    let url_end = find_matching(chars, url_start, ')');
    if url_end >= chars.len() {
        return None;
    }
    let label: String = chars[label_start..label_end].iter().collect();
    let url: String = chars[url_start..url_end].iter().collect();
    Some((label, url, url_end + 1))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn escape_special_chars() {
        let input = "hello_world *bold*";
        let result = to_telegram_markdown_v2(input);
        assert!(result.contains("hello\\_world"));
    }

    #[test]
    fn code_fence_preserved() {
        let input = "```rust\nlet x = 1;\n```";
        let result = to_telegram_markdown_v2(input);
        assert!(result.contains("```rust"));
    }
}
