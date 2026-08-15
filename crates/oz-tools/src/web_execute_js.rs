//! web_execute_js — execute JavaScript in connected browser tabs via TMWebDriver.
use std::sync::Mutex;
use async_trait::async_trait;
use oz_browser::BrowserClient;
use oz_core_types::{ToolContext, ToolError, ToolOutput};
use crate::registry::ToolHandler;

pub struct WebExecuteJsTool { browser: Mutex<Option<BrowserClient>> }

impl Default for WebExecuteJsTool {
    fn default() -> Self {
        Self::new()
    }
}

impl WebExecuteJsTool {
    pub fn new() -> Self { WebExecuteJsTool { browser: Mutex::new(None) } }
    pub fn set_browser(&self, client: BrowserClient) { *self.browser.lock().unwrap() = Some(client); }
}

#[async_trait]
impl ToolHandler for WebExecuteJsTool {
    fn name(&self) -> String { "web_execute_js".to_string() }
    fn description(&self) -> String {
        "Execute JavaScript in a connected browser tab. Use to click elements, \
         extract data, fill forms, scroll, or take screenshots. \
         Multi-call OK. Use web_list_tabs to find tab IDs.".into()
    }
    fn parameters(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "code": {"type": "string", "description": "JavaScript code to execute"},
                "session_id": {"type": "string", "description": "Browser tab ID (optional)"},
                "timeout": {"type": "integer", "description": "Timeout in seconds (default: 15)"}
            },
            "required": ["code"]
        })
    }
    async fn execute(&self, args: serde_json::Value, _ctx: &ToolContext) -> Result<ToolOutput, ToolError> {
        let client = {
            let guard = self.browser.lock().unwrap();
            guard.clone()
                .ok_or_else(|| ToolError::Custom("No browser connected. Start TMWebDriver first.".into()))?
        };
        let code = args["code"].as_str().unwrap_or("");
        if code.is_empty() { return Ok(ToolOutput::bad_json("web_execute_js: code is required")); }
        let result = client.execute_js(code).await?;
        let output = match &result.value {
            Some(v) if v.is_string() => v.as_str().unwrap_or("").to_string(),
            Some(v) => serde_json::to_string(v).unwrap_or_default(),
            None => result.error.unwrap_or_default(),
        };
        let truncated = if output.len() > 8000 {
            format!("{}... (truncated, {} bytes)", &output[..8000], output.len())
        } else { output };
        Ok(ToolOutput::success(serde_json::json!({"status": "ok", "result": truncated})))
    }
}

pub struct WebListTabsTool { browser: Mutex<Option<BrowserClient>> }

impl Default for WebListTabsTool {
    fn default() -> Self {
        Self::new()
    }
}

impl WebListTabsTool {
    pub fn new() -> Self { WebListTabsTool { browser: Mutex::new(None) } }
    pub fn set_browser(&self, client: BrowserClient) { *self.browser.lock().unwrap() = Some(client); }
}

#[async_trait]
impl ToolHandler for WebListTabsTool {
    fn name(&self) -> String { "web_list_tabs".to_string() }
    fn description(&self) -> String { "List connected browser tabs with IDs and URLs.".into() }
    fn parameters(&self) -> serde_json::Value { serde_json::json!({"type": "object", "properties": {}, "required": []}) }
    async fn execute(&self, _args: serde_json::Value, _ctx: &ToolContext) -> Result<ToolOutput, ToolError> {
        let client = {
            let guard = self.browser.lock().unwrap();
            guard.clone().ok_or_else(|| ToolError::Custom("No browser connected.".into()))?
        };
        let tabs = client.get_tabs().await?;
        let list: Vec<serde_json::Value> = tabs.iter().map(|t| serde_json::json!({
            "id": t.id, "url": t.url, "title": t.title
        })).collect();
        Ok(ToolOutput::success(serde_json::json!({"tabs": list, "count": list.len()})))
    }
}
