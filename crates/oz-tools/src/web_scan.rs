use std::sync::Mutex;

use async_trait::async_trait;
use oz_browser::BrowserClient;
use oz_core_types::{ToolContext, ToolError, ToolOutput};

use crate::registry::ToolHandler;

/// Blocked IP ranges for SSRF prevention.
const BLOCKED_IP_RANGES: &[&str] = &[
    "127.0.0.1",
    "localhost",
    "0.0.0.0",
    "[::1]",
    "10.",
    "172.16.",
    "172.17.",
    "172.18.",
    "172.19.",
    "172.20.",
    "172.21.",
    "172.22.",
    "172.23.",
    "172.24.",
    "172.25.",
    "172.26.",
    "172.27.",
    "172.28.",
    "172.29.",
    "172.30.",
    "172.31.",
    "192.168.",
    "169.254.",
    "metadata.google.internal",
];

pub fn is_url_safe(url: &str) -> bool {
    let lower = url.to_lowercase();
    for blocked in BLOCKED_IP_RANGES {
        if lower.contains(&blocked.to_lowercase()) {
            return false;
        }
    }
    true
}

/// Fetch and simplify HTML from a URL using the browser.
pub struct WebScanTool {
    browser: Mutex<Option<BrowserClient>>,
}

impl WebScanTool {
    pub fn new() -> Self {
        WebScanTool {
            browser: Mutex::new(None),
        }
    }

    fn get_browser(&self) -> BrowserClient {
        let mut b = self
            .browser
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if b.is_none() {
            *b = Some(BrowserClient::new("http://127.0.0.1:18765"));
        }
        b.as_ref().unwrap().clone()
    }
}

impl Default for WebScanTool {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl ToolHandler for WebScanTool {
    fn name(&self) -> String {
        "web_scan".to_string()
    }
    fn description(&self) -> String {
        "Open a URL in the browser, get simplified HTML content. Use for reading web pages."
            .to_string()
    }
    fn parameters(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "url": {
                    "type": "string",
                    "description": "The URL to open and read"
                },
                "max_chars": {
                    "type": "integer",
                    "description": "Max chars (default 5000)",
                    "default": 5000
                }
            },
            "required": ["url"]
        })
    }

    async fn execute(
        &self,
        args: serde_json::Value,
        _ctx: &ToolContext,
    ) -> Result<ToolOutput, ToolError> {
        let url = args["url"]
            .as_str()
            .ok_or_else(|| ToolError::Custom("missing url".into()))?;
        if !is_url_safe(url) {
            return Ok(ToolOutput::bad_json(format!(
                "web_scan: URL `{url}` targets a blocked address for security reasons."
            )));
        }
        let max_chars = args
            .get("max_chars")
            .and_then(|v| v.as_u64())
            .unwrap_or(5000) as usize;

        let mut browser = self.get_browser();
        let title = browser.navigate(url).await?;
        let html = browser.get_simplified_html(max_chars).await?;

        Ok(ToolOutput::success(serde_json::json!({
            "title": title,
            "url": url,
            "html": html,
            "char_count": html.len(),
        })))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_web_scan_missing_url() {
        let tool = WebScanTool::new();
        let result = tool
            .execute(serde_json::json!({}), &ToolContext::default())
            .await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn test_web_scan_connect_error() {
        let tool = WebScanTool::new();
        let result = tool
            .execute(
                serde_json::json!({"url": "http://localhost:1", "max_chars": 100}),
                &ToolContext::default(),
            )
            .await;
        // localhost is blocked by SSRF protection
        assert!(result.is_ok());
        let output = result.unwrap();
        assert!(
            output.next_prompt.unwrap_or_default().contains("blocked"),
            "expected blocked URL message"
        );
    }
}

#[linkme::distributed_slice(crate::registry::TOOL_FACTORIES)]
fn register_web_scan(reg: &mut crate::registry::ToolRegistry) {
    reg.register(crate::web_scan::WebScanTool::new());
}
