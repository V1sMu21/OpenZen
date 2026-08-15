pub mod cdp;
pub mod simplify;

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};

use oz_core_types::ToolError;

/// Result of executing JavaScript in a page.
#[derive(Debug, Clone)]
pub struct JsResult {
    pub value: Option<serde_json::Value>,
    pub error: Option<String>,
}

/// Information about a browser tab.
#[derive(Debug, Clone)]
pub struct TabInfo {
    pub id: String,
    pub title: String,
    pub url: String,
}

/// HTTP-based browser client — connects to a headless browser HTTP API.
pub struct BrowserClient {
    base_url: String,
    next_id: AtomicU64,
    sessions: HashMap<String, String>,
    default_session_id: Option<String>,
    client: reqwest::Client,
}

impl Clone for BrowserClient {
    fn clone(&self) -> Self {
        BrowserClient {
            base_url: self.base_url.clone(),
            next_id: AtomicU64::new(self.next_id.load(Ordering::Relaxed)),
            sessions: self.sessions.clone(),
            default_session_id: self.default_session_id.clone(),
            client: self.client.clone(),
        }
    }
}

impl BrowserClient {
    /// Create a new browser client pointing at `base_url` (e.g. `http://127.0.0.1:18765`).
    pub fn new(base_url: &str) -> Self {
        BrowserClient {
            base_url: base_url.trim_end_matches('/').to_string(),
            next_id: AtomicU64::new(1),
            sessions: HashMap::new(),
            default_session_id: None,
            client: reqwest::Client::builder()
                .timeout(std::time::Duration::from_secs(60))
                .build()
                .unwrap_or_default(),
        }
    }

    fn next_id(&self) -> u64 {
        self.next_id.fetch_add(1, Ordering::SeqCst)
    }

    /// Send a JSON-RPC command to the browser and get the result.
    async fn send_command(
        &self,
        method: &str,
        params: serde_json::Value,
    ) -> Result<serde_json::Value, ToolError> {
        let body = serde_json::json!({
            "id": self.next_id(),
            "method": method,
            "params": params,
        });
        let resp = self
            .client
            .post(&self.base_url)
            .json(&body)
            .send()
            .await
            .map_err(|e| ToolError::Custom(format!("browser request failed: {e}")))?;
        let data: serde_json::Value = resp
            .json()
            .await
            .map_err(|e| ToolError::Custom(format!("browser response parse failed: {e}")))?;
        Ok(data)
    }

    /// Navigate to a URL and return the page title.
    pub async fn navigate(&mut self, url: &str) -> Result<String, ToolError> {
        let result = self
            .send_command("navigate", serde_json::json!({"url": url}))
            .await?;
        let title = result["result"]["title"]
            .as_str()
            .unwrap_or(url)
            .to_string();
        Ok(title)
    }

    /// Get the current page HTML, simplified.
    pub async fn get_simplified_html(&self, max_chars: usize) -> Result<String, ToolError> {
        let result = self
            .send_command("getHtml", serde_json::json!({"maxChars": max_chars}))
            .await?;
        let html = result["result"]["html"].as_str().unwrap_or("");
        Ok(simplify::simplify_html(html, max_chars))
    }

    /// Execute JavaScript in the current page and return the result.
    pub async fn execute_js(&self, code: &str) -> Result<JsResult, ToolError> {
        let result = self
            .send_command("evaluate", serde_json::json!({"code": code}))
            .await?;
        let val = result["result"]["value"].clone();
        Ok(JsResult {
            value: if val.is_null() { None } else { Some(val) },
            error: result["result"]["error"].as_str().map(|s| s.to_string()),
        })
    }

    /// List all open tabs/sessions.
    pub async fn get_tabs(&self) -> Result<Vec<TabInfo>, ToolError> {
        let result = self.send_command("getTabs", serde_json::json!({})).await?;
        let tabs = result["result"]["tabs"]
            .as_array()
            .map(|arr| {
                arr.iter()
                    .map(|t| TabInfo {
                        id: t["id"].as_str().unwrap_or("").to_string(),
                        title: t["title"].as_str().unwrap_or("").to_string(),
                        url: t["url"].as_str().unwrap_or("").to_string(),
                    })
                    .collect()
            })
            .unwrap_or_default();
        Ok(tabs)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_browser_client_new() {
        let client = BrowserClient::new("http://127.0.0.1:18765");
        // Just verify it doesn't panic and stores the URL correctly
        assert!(client.base_url.contains("18765"));
    }

    #[test]
    fn test_next_id_increments() {
        let client = BrowserClient::new("http://localhost:18765");
        let id1 = client.next_id();
        let id2 = client.next_id();
        assert_eq!(id2, id1 + 1);
    }

    #[tokio::test]
    async fn test_connect_no_server_returns_error() {
        // No browser running on this port, so it should error gracefully
        let client = BrowserClient::new("http://127.0.0.1:1");
        let result = client.get_simplified_html(1000).await;
        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(err.contains("browser") || err.contains("request") || err.contains("failed"));
    }

    #[test]
    fn test_simplify_removes_script_tags() {
        let html = "<html><head><script>alert(1)</script></head><body><p>hello</p></body></html>";
        let result = simplify::simplify_html(html, 1000);
        assert!(!result.contains("<script>"));
        assert!(result.contains("hello"));
    }

    #[test]
    fn test_simplify_removes_style_tags() {
        let html =
            "<html><head><style>body{color:red}</style></head><body><p>text</p></body></html>";
        let result = simplify::simplify_html(html, 1000);
        assert!(!result.contains("<style>"));
        assert!(result.contains("text"));
    }

    #[test]
    fn test_simplify_truncates_long_content() {
        let long_text = "a".repeat(2000);
        let html = format!("<html><body><p>{}</p></body></html>", long_text);
        let result = simplify::simplify_html(&html, 100);
        assert!(
            result.len() <= 200,
            "simplified content should be truncated: len={}",
            result.len()
        );
    }

    #[test]
    fn test_simplify_empty_html() {
        let result = simplify::simplify_html("", 1000);
        assert!(result.is_empty() || result.contains("<html>") || result.contains("<body>"));
    }
}
