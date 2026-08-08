use std::sync::Mutex;

use async_trait::async_trait;
use oz_browser::BrowserClient;
use oz_core_types::{ToolContext, ToolError, ToolOutput};

use crate::registry::ToolHandler;

/// Callback-based patterns that are blocked for data exfiltration prevention.
const BLOCKED_JS_PATTERNS: &[&str] = &[
    "document.cookie",
    "fetch(",
    "XMLHttpRequest",
    "navigator.sendBeacon",
    "WebSocket(",
    "localStorage",
    "sessionStorage",
    "indexedDB",
    "open(",
];

fn is_js_safe(code: &str) -> bool {
    let lower = code.to_lowercase();
    for blocked in BLOCKED_JS_PATTERNS {
        if lower.contains(&blocked.to_lowercase()) {
            return false;
        }
    }
    true
}

/// Execute JavaScript in the browser page.
pub struct WebJsTool {
    browser: Mutex<Option<BrowserClient>>,
}

impl WebJsTool {
    pub fn new() -> Self {
        WebJsTool { browser: Mutex::new(None) }
    }

    fn get_browser(&self) -> BrowserClient {
        let mut b = self.browser.lock().unwrap();
        if b.is_none() {
            *b = Some(BrowserClient::new("http://127.0.0.1:18765"));
        }
        b.as_ref().unwrap().clone()
    }
}

impl Default for WebJsTool {
    fn default() -> Self { Self::new() }
}

#[async_trait]
impl ToolHandler for WebJsTool {
    fn name(&self) -> String { "web_js".to_string() }
    fn description(&self) -> String { "Execute JavaScript in the current browser page and return the result.".to_string() }
    fn parameters(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "code": {
                    "type": "string",
                    "description": "JavaScript code to execute"
                }
            },
            "required": ["code"]
        })
    }

    async fn execute(&self, args: serde_json::Value, _ctx: &ToolContext) -> Result<ToolOutput, ToolError> {
        let code = args["code"].as_str().ok_or_else(|| ToolError::Custom("missing code".into()))?;
        if !is_js_safe(code) {
            return Ok(ToolOutput::bad_json(
                "web_js: blocked pattern detected (data exfiltration risk). Operation denied."
            ));
        }

        let browser = self.get_browser();
        let result = browser.execute_js(code).await?;

        Ok(ToolOutput::success(serde_json::json!({
            "value": result.value,
            "error": result.error,
        })))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_web_js_missing_code() {
        let tool = WebJsTool::new();
        let result = tool.execute(serde_json::json!({}), &ToolContext::default()).await;
        assert!(result.is_err());
    }
}

#[linkme::distributed_slice(crate::registry::TOOL_FACTORIES)]
fn register_web_js(reg: &mut crate::registry::ToolRegistry) {
    reg.register(crate::web_js::WebJsTool::new());
}