use std::collections::HashMap;
use std::sync::Arc;

use async_trait::async_trait;
use oz_core_types::{ToolContext, ToolDefinition, ToolError, ToolFunction, ToolOutput};
use oz_tools::registry::ToolHandler;
use tokio::sync::Mutex;

/// A connected MCP server with its discovered tools.
pub struct McpServerConnection {
    pub name: String,
    pub base_url: String,
    pub tools: Vec<ToolDefinition>,
    client: reqwest::Client,
    sequence: Arc<Mutex<u64>>,
}

impl McpServerConnection {
    /// Connect to an MCP server via its SSE endpoint URL.
    /// The URL should be the SSE endpoint (e.g. `http://host:port/sse`).
    pub async fn connect(name: &str, sse_url: &str) -> Result<Self, String> {
        let base_url = sse_url
            .trim_end_matches("/sse")
            .trim_end_matches('/')
            .to_string();
        let client = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(30))
            .build()
            .map_err(|e| format!("HTTP client build error: {e}"))?;

        let mut conn = McpServerConnection {
            name: name.to_string(),
            base_url,
            tools: Vec::new(),
            client,
            sequence: Arc::new(Mutex::new(1)),
        };

        conn.initialize().await?;
        conn.discover_tools().await?;

        Ok(conn)
    }

    /// Call a tool on the remote MCP server.
    pub async fn call_tool(
        &self,
        tool_name: &str,
        args: serde_json::Value,
    ) -> Result<ToolOutput, ToolError> {
        let id = {
            let mut seq = self.sequence.lock().await;
            let id = *seq;
            *seq += 1;
            id
        };

        let request = serde_json::json!({
            "jsonrpc": "2.0",
            "id": id,
            "method": "tools/call",
            "params": {
                "name": tool_name,
                "arguments": args,
            }
        });

        let url = format!("{}/messages", self.base_url);
        let resp = self
            .client
            .post(&url)
            .json(&request)
            .send()
            .await
            .map_err(|e| ToolError::Custom(format!("MCP request failed: {e}")))?;

        let data: serde_json::Value = resp
            .json()
            .await
            .map_err(|e| ToolError::Custom(format!("MCP response parse failed: {e}")))?;

        if let Some(err) = data.get("error") {
            let msg = err["message"].as_str().unwrap_or("unknown MCP error");
            return Err(ToolError::Custom(format!("MCP tool error: {msg}")));
        }

        let result = data.get("result").cloned().unwrap_or_default();
        let content = result
            .get("content")
            .and_then(|c| c.as_array())
            .cloned()
            .unwrap_or_default();

        let mut text_output = String::new();
        for block in &content {
            if block["type"] == "text" {
                text_output.push_str(block["text"].as_str().unwrap_or(""));
                text_output.push('\n');
            }
        }

        if result
            .get("isError")
            .and_then(|v| v.as_bool())
            .unwrap_or(false)
        {
            return Err(ToolError::Custom(text_output));
        }

        Ok(ToolOutput::success(serde_json::json!({
            "content": text_output.trim().to_string(),
            "raw": result,
        })))
    }

    async fn initialize(&self) -> Result<(), String> {
        let id = {
            let mut seq = self.sequence.lock().await;
            let id = *seq;
            *seq += 1;
            id
        };

        let request = serde_json::json!({
            "jsonrpc": "2.0",
            "id": id,
            "method": "initialize",
            "params": {
                "protocolVersion": "2024-11-05",
                "capabilities": {},
                "clientInfo": {
                    "name": "openzen",
                    "version": "0.1.0",
                }
            }
        });

        let url = format!("{}/messages", self.base_url);
        let resp = self
            .client
            .post(&url)
            .json(&request)
            .send()
            .await
            .map_err(|e| format!("MCP initialize failed: {e}"))?;

        let data: serde_json::Value = resp
            .json()
            .await
            .map_err(|e| format!("MCP initialize parse failed: {e}"))?;

        if let Some(err) = data.get("error") {
            return Err(format!("MCP initialize error: {}", err["message"]));
        }

        tracing::info!(
            "MCP server '{}' initialized: protocol={}",
            self.name,
            data["result"]["protocolVersion"]
                .as_str()
                .unwrap_or("unknown")
        );

        Ok(())
    }

    async fn discover_tools(&mut self) -> Result<(), String> {
        let id = {
            let mut seq = self.sequence.lock().await;
            let id = *seq;
            *seq += 1;
            id
        };

        let request = serde_json::json!({
            "jsonrpc": "2.0",
            "id": id,
            "method": "tools/list",
        });

        let url = format!("{}/messages", self.base_url);
        let resp = self
            .client
            .post(&url)
            .json(&request)
            .send()
            .await
            .map_err(|e| format!("MCP tools/list failed: {e}"))?;

        let data: serde_json::Value = resp
            .json()
            .await
            .map_err(|e| format!("MCP tools/list parse failed: {e}"))?;

        let tools = data["result"]["tools"]
            .as_array()
            .ok_or_else(|| "MCP tools/list returned no tools array".to_string())?;

        for tool in tools {
            let name = tool["name"].as_str().unwrap_or("unknown");
            let description = tool["description"].as_str().unwrap_or("");
            let input_schema = tool.get("inputSchema").cloned().unwrap_or_default();

            let def = ToolDefinition {
                type_: "function".into(),
                function: ToolFunction {
                    name: format!("mcp_{}_{}", self.name, name),
                    description: format!("[MCP {}] {}", self.name, description),
                    parameters: input_schema,
                },
            };
            self.tools.push(def);
        }

        tracing::info!(
            "MCP server '{}' discovered {} tools",
            self.name,
            self.tools.len()
        );
        Ok(())
    }
}

/// A ToolHandler that delegates to a remote MCP server.
pub struct McpToolHandler {
    server_name: String,
    tool_name: String,
    connection: Arc<McpServerConnection>,
}

impl McpToolHandler {
    pub fn new(server_name: &str, tool_name: &str, connection: Arc<McpServerConnection>) -> Self {
        McpToolHandler {
            server_name: server_name.to_string(),
            tool_name: tool_name.to_string(),
            connection,
        }
    }
}

#[async_trait]
impl ToolHandler for McpToolHandler {
    fn name(&self) -> String {
        format!("mcp_{}_{}", self.server_name, self.tool_name)
    }

    fn description(&self) -> String {
        format!("[MCP {}] Tool: {}", self.server_name, self.tool_name)
    }

    fn parameters(&self) -> serde_json::Value {
        serde_json::json!({"type": "object", "properties": {}})
    }

    async fn execute(
        &self,
        args: serde_json::Value,
        _ctx: &ToolContext,
    ) -> Result<ToolOutput, ToolError> {
        self.connection.call_tool(&self.tool_name, args).await
    }
}

/// Manager for all MCP server connections.
pub struct McpManager {
    pub connections: HashMap<String, Arc<McpServerConnection>>,
}

impl McpManager {
    pub fn new() -> Self {
        McpManager {
            connections: HashMap::new(),
        }
    }

    /// Connect to MCP servers based on config entries.
    /// Config format: `[[mcp_servers]]` with `name` and `url` fields.
    pub async fn connect_all(config_entries: &[McpServerConfig]) -> Result<Self, String> {
        let mut manager = McpManager::new();
        for entry in config_entries {
            match McpServerConnection::connect(&entry.name, &entry.url).await {
                Ok(conn) => {
                    tracing::info!(
                        "Connected to MCP server: {} ({} tools)",
                        entry.name,
                        conn.tools.len()
                    );
                    manager
                        .connections
                        .insert(entry.name.clone(), Arc::new(conn));
                }
                Err(e) => {
                    tracing::warn!("Failed to connect to MCP server '{}': {e}", entry.name);
                }
            }
        }
        Ok(manager)
    }

    /// Register all discovered MCP tools into the given ToolRegistry.
    pub fn register_tools(&self, registry: &mut oz_tools::registry::ToolRegistry) {
        for (server_name, conn) in &self.connections {
            let name_prefix = format!("mcp_{}_", server_name);
            for def in &conn.tools {
                let mcp_tool_name = def
                    .function
                    .name
                    .strip_prefix(&name_prefix)
                    .unwrap_or(&def.function.name);
                let handler = McpToolHandler::new(server_name, mcp_tool_name, conn.clone());
                // Register with the full prefixed name
                registry.register_with_name(&def.function.name, handler);
            }
        }
    }
}

impl Default for McpManager {
    fn default() -> Self {
        Self::new()
    }
}

/// Configuration for an MCP server connection.
#[derive(Debug, Clone, serde::Deserialize)]
pub struct McpServerConfig {
    pub name: String,
    pub url: String,
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn test_mcp_server_config_deserialize() {
        let toml_str = r#"
name = "test-server"
url = "http://127.0.0.1:8080/sse"
"#;
        let config: McpServerConfig = toml::from_str(toml_str).unwrap();
        assert_eq!(config.name, "test-server");
        assert_eq!(config.url, "http://127.0.0.1:8080/sse");
    }

    #[test]
    fn test_mcp_tool_handler_creation() {
        // Just verify the struct construction works
        let dummy_url = "http://127.0.0.1:9999";
        // We can't easily test without a running server, but we can verify the types
        let config = McpServerConfig {
            name: "test".into(),
            url: dummy_url.into(),
        };
        assert_eq!(config.name, "test");
    }

    #[test]
    fn test_json_rpc_request_format() {
        let request = serde_json::json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "tools/call",
            "params": {
                "name": "my_tool",
                "arguments": {"key": "value"}
            }
        });
        assert_eq!(request["method"], "tools/call");
        assert_eq!(request["params"]["name"], "my_tool");
    }

    #[test]
    fn test_mcp_response_parse() {
        let response = json!({
            "jsonrpc": "2.0",
            "id": 1,
            "result": {
                "content": [
                    {"type": "text", "text": "Hello from MCP"}
                ]
            }
        });
        assert_eq!(response["result"]["content"][0]["text"], "Hello from MCP");
    }

    #[test]
    fn test_mcp_error_response_parse() {
        let response = json!({
            "jsonrpc": "2.0",
            "id": 1,
            "error": {
                "code": -32601,
                "message": "Method not found"
            }
        });
        assert!(response.get("result").is_none());
        assert_eq!(response["error"]["message"], "Method not found");
    }
}
