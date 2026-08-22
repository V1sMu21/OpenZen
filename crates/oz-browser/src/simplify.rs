//! HTML simplification — removes scripts, styles, navigation cruft.
//!
//! Uses regex-based cleaning to strip unwanted tags while preserving
//! visible text content and interactive elements (forms, inputs, links).

use regex::Regex;

/// Precompiled once: simplify_html ran ~19 Regex::new calls per invocation
/// and is invoked per web_scan page.
struct SimplifyPatterns {
    block: [Regex; 4],
    ws: Regex,
    remove_pairs: Vec<Regex>,
    remove_self_closing: Vec<Regex>,
}

static PATTERNS: std::sync::LazyLock<SimplifyPatterns> = std::sync::LazyLock::new(|| {
    let remove_tags = ["meta", "link", "svg", "nav", "footer", "header", "aside"];
    SimplifyPatterns {
        block: [
            Regex::new(r"(?is)<script[^>]*>.*?</script>").unwrap(),
            Regex::new(r"(?is)<style[^>]*>.*?</style>").unwrap(),
            Regex::new(r"(?is)<noscript[^>]*>.*?</noscript>").unwrap(),
            Regex::new(r"(?is)<!--.*?-->").unwrap(),
        ],
        ws: Regex::new(r"\s+").unwrap(),
        remove_pairs: remove_tags
            .iter()
            .map(|t| Regex::new(&format!(r"(?is)<{t}[^>]*>.*?</{t}>")).unwrap())
            .collect(),
        remove_self_closing: remove_tags
            .iter()
            .map(|t| Regex::new(&format!(r"(?is)<{t}[^>]*/?>")).unwrap())
            .collect(),
    }
});

/// Simplify raw HTML: remove script/style tags, limit depth, truncate.
pub fn simplify_html(html: &str, max_chars: usize) -> String {
    if html.is_empty() {
        return String::new();
    }

    let p = &*PATTERNS;
    let mut result = html.to_string();
    result = p.block[0].replace_all(&result, "").to_string();
    result = p.block[1].replace_all(&result, "").to_string();
    result = p.block[2].replace_all(&result, "").to_string();

    for re in &p.remove_pairs {
        result = re.replace_all(&result, "").to_string();
    }
    for re in &p.remove_self_closing {
        result = re.replace_all(&result, "").to_string();
    }

    result = p.block[3].replace_all(&result, "").to_string();
    result = p.ws.replace_all(&result, " ").to_string();

    if result.len() > max_chars {
        // Char-safe cut — byte truncation panicked on CJK pages (P0-A class).
        let mut end = max_chars;
        while end > 0 && !result.is_char_boundary(end) {
            end -= 1;
        }
        result.truncate(end);
        result.push_str("... (truncated)");
    }

    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_removes_script_with_content() {
        let html = "<html><script>let x = 1;</script><body>hi</body></html>";
        let result = simplify_html(html, 1000);
        assert!(!result.contains("<script>"));
        assert!(!result.contains("let x"));
    }

    #[test]
    fn test_removes_nested_style() {
        let html = "<html><style>body { background: red; }</style><body>ok</body></html>";
        let result = simplify_html(html, 1000);
        assert!(!result.contains("<style>"));
        assert!(result.contains("ok"));
    }

    #[test]
    fn test_preserves_form_elements() {
        let html = "<form><input name='q' value='test'><button>Go</button></form>";
        let result = simplify_html(html, 1000);
        assert!(result.contains("input") || result.contains("form"));
    }

    #[test]
    fn test_truncation_adds_marker() {
        let long = "a".repeat(500);
        let html = format!("<body>{}</body>", long);
        let result = simplify_html(&html, 50);
        assert!(result.contains("truncated"));
        assert!(result.len() < 200);
    }

    #[test]
    fn test_empty_input() {
        assert_eq!(simplify_html("", 100), "");
    }

    #[test]
    fn test_no_html_tags() {
        let result = simplify_html("plain text content", 100);
        assert_eq!(result, "plain text content");
    }
}
