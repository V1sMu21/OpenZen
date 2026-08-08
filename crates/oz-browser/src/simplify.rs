//! HTML simplification — removes scripts, styles, navigation cruft.
//!
//! Uses regex-based cleaning to strip unwanted tags while preserving
//! visible text content and interactive elements (forms, inputs, links).

use regex::Regex;

/// Simplify raw HTML: remove script/style tags, limit depth, truncate.
pub fn simplify_html(html: &str, max_chars: usize) -> String {
    if html.is_empty() {
        return String::new();
    }

    let re_script = Regex::new(r"(?is)<script[^>]*>.*?</script>").unwrap();
    let re_style = Regex::new(r"(?is)<style[^>]*>.*?</style>").unwrap();
    let re_noscript = Regex::new(r"(?is)<noscript[^>]*>.*?</noscript>").unwrap();
    let re_comment = Regex::new(r"(?is)<!--.*?-->").unwrap();
    let re_ws = Regex::new(r"\s+").unwrap();

    let mut result = html.to_string();
    result = re_script.replace_all(&result, "").to_string();
    result = re_style.replace_all(&result, "").to_string();
    result = re_noscript.replace_all(&result, "").to_string();

    let remove_tags = ["meta", "link", "svg", "nav", "footer", "header", "aside"];
    for tag in &remove_tags {
        let re = Regex::new(&format!(r"(?is)<{tag}[^>]*>.*?</{tag}>")).unwrap();
        result = re.replace_all(&result, "").to_string();
        let re_self = Regex::new(&format!(r"(?is)<{tag}[^>]*/?>")).unwrap();
        result = re_self.replace_all(&result, "").to_string();
    }

    result = re_comment.replace_all(&result, "").to_string();
    result = re_ws.replace_all(&result, " ").to_string();

    if result.len() > max_chars {
        result.truncate(max_chars);
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
