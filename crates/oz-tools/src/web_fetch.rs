use async_trait::async_trait;
use oz_core_types::{ToolContext, ToolError, ToolOutput};

use crate::registry::ToolHandler;

/// Fetch a URL's page content and return readable text (title + body).
/// Uses reqwest for HTTP — no browser required.
pub struct WebFetchTool;

#[async_trait]
impl ToolHandler for WebFetchTool {
    fn name(&self) -> String { "web_fetch".to_string() }
    fn description(&self) -> String {
        "Fetch a web page by URL and return its readable text content (strips HTML). \
         Use after web_search to read full page content. Default 3000 chars."
            .to_string()
    }
    fn parameters(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "url": {
                    "type": "string",
                    "description": "The URL to fetch and read"
                },
                "max_chars": {
                    "type": "integer",
                    "description": "Max chars (default 3000)",
                    "default": 3000
                }
            },
            "required": ["url"]
        })
    }

    async fn execute(&self, args: serde_json::Value, _ctx: &ToolContext) -> Result<ToolOutput, ToolError> {
        let url = args["url"].as_str().ok_or_else(|| ToolError::Custom("missing 'url' parameter".into()))?;

        if !super::web_scan::is_url_safe(url) {
            return Ok(ToolOutput::bad_json(
                format!("web_fetch: URL `{url}` targets a blocked address for security reasons.")
            ));
        }

        let max_chars = args.get("max_chars").and_then(|v| v.as_u64()).unwrap_or(3000) as usize;

        match fetch_and_extract(url, max_chars).await {
            Ok(result) => Ok(ToolOutput::success(serde_json::json!({
                "url": url,
                "title": result.title,
                "text": result.text,
                "char_count": result.text.len(),
            }))),
            Err(e) => Err(ToolError::Custom(format!("web_fetch failed: {}", e))),
        }
    }
}

struct PageContent {
    title: String,
    text: String,
}

async fn fetch_and_extract(url: &str, max_chars: usize) -> Result<PageContent, String> {
    let client = reqwest::Client::builder()
        .user_agent("Mozilla/5.0 (compatible; OpenZen/1.0)")
        .timeout(std::time::Duration::from_secs(15))
        .build()
        .map_err(|e| format!("client build failed: {e}"))?;

    let resp = client.get(url).send().await
        .map_err(|e| format!("request failed: {e}"))?;

    if !resp.status().is_success() {
        return Err(format!("HTTP {}", resp.status()));
    }

    let html = resp.text().await
        .map_err(|e| format!("read body failed: {e}"))?;

    let title = extract_title(&html);
    let text = extract_text(&html, max_chars);

    Ok(PageContent { title, text })
}

fn extract_title(html: &str) -> String {
    if let Some(start) = html.find("<title") {
        if let Some(gt) = html[start..].find('>') {
            let content_start = start + gt + 1;
            if let Some(end) = html[content_start..].find("</title>") {
                return html_to_text(&html[content_start..content_start + end]);
            }
        }
    }
    String::new()
}

fn extract_text(html: &str, max_chars: usize) -> String {
    let mut text = html_to_text(html);
    if text.len() > max_chars {
        // Safe truncation: round down to nearest UTF-8 char boundary.
        let mut boundary = max_chars;
        while boundary > 0 && !text.is_char_boundary(boundary) {
            boundary -= 1;
        }
        text.truncate(boundary);
        text.push_str("\n\n[... truncated ...]");
    }
    text
}

/// Strip HTML tags, decode common entities, collapse whitespace.
fn html_to_text(html: &str) -> String {
    // Remove script and style blocks with their content
    let no_scripts = remove_blocks(html, ("<script", "</script>"));
    let no_styles = remove_blocks(&no_scripts, ("<style", "</style>"));

    let mut text = String::new();
    let mut inside_tag = false;
    let mut last_was_newline = false;

    for ch in no_styles.chars() {
        if ch == '<' {
            inside_tag = true;
            continue;
        }
        if ch == '>' && inside_tag {
            inside_tag = false;
            if !last_was_newline {
                text.push(' ');
                last_was_newline = false;
            }
            continue;
        }
        if !inside_tag {
            if ch == '\n' || ch == '\r' {
                if !last_was_newline {
                    text.push('\n');
                    last_was_newline = true;
                }
            } else if ch.is_whitespace() {
                if !last_was_newline && !text.ends_with(' ') && !text.ends_with('\n') {
                    text.push(' ');
                }
            } else {
                text.push(ch);
                last_was_newline = false;
            }
        }
    }

    // Decode common HTML entities
    let decoded = decode_entities(&text);

    // Collapse multiple blank lines
    collapse_blank_lines(&decoded)
}

fn remove_blocks(html: &str, (start_tag, end_tag): (&str, &str)) -> String {
    let mut result = String::with_capacity(html.len());
    let mut pos = 0;
    let lower = html.to_lowercase();

    loop {
        match lower[pos..].find(start_tag) {
            None => {
                result.push_str(&html[pos..]);
                break;
            }
            Some(start_idx) => {
                result.push_str(&html[pos..pos + start_idx]);
                let block_start = pos + start_idx;
                if let Some(end_idx) = lower[block_start..].find(end_tag) {
                    pos = block_start + end_idx + end_tag.len();
                } else {
                    // No closing tag, skip rest
                    pos = html.len();
                }
            }
        }
    }
    result
}

fn decode_entities(text: &str) -> String {
    text.replace("&amp;", "&")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&#39;", "'")
        .replace("&apos;", "'")
        .replace("&nbsp;", " ")
        .replace("&#x27;", "'")
}

fn collapse_blank_lines(text: &str) -> String {
    let mut result = String::with_capacity(text.len());
    let mut blank_count = 0;
    for line in text.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            blank_count += 1;
            if blank_count <= 2 {
                result.push('\n');
            }
        } else {
            blank_count = 0;
            result.push_str(trimmed);
            result.push('\n');
        }
    }
    result.trim().to_string()
}

#[linkme::distributed_slice(crate::registry::TOOL_FACTORIES)]
fn register_web_fetch(reg: &mut crate::registry::ToolRegistry) {
    reg.register(WebFetchTool);
}
