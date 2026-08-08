//! MCP Bridge — registers MCP tools into the ToolRegistry.
//!
//! Converts MCP tool definitions from `ga-mcp` into `ToolHandler` instances
//! that can be dispatched by the agent loop.

use async_trait::async_trait;
use oz_core_types::{ToolContext, ToolError, ToolOutput};

use crate::registry::ToolHandler;

/// Wraps an MCP tool definition as a `ToolHandler`.
///
/// Tool naming convention: `mcp__{server}__{tool_name}`
/// This avoids naming conflicts with builtin tools.
pub struct McpToolHandler {
    server_name: String,
    tool_name: String,
    full_name: String,
    description: String,
    input_schema: serde_json::Value,
}

impl McpToolHandler {
    pub fn new(
        server_name: &str,
        tool_name: &str,
        description: &str,
        input_schema: serde_json::Value,
    ) -> Self {
        let full_name = format!("mcp__{}__{}", server_name, tool_name);
        McpToolHandler {
            server_name: server_name.to_string(),
            tool_name: tool_name.to_string(),
            full_name,
            description: description.to_string(),
            input_schema,
        }
    }

    pub fn from_mcp_tool(server_name: &str, tool: &oz_mcp::McpTool) -> Self {
        Self::new(server_name, &tool.name, &tool.description, tool.input_schema.clone())
    }

    pub fn full_name(&self) -> &str {
        &self.full_name
    }

    pub fn server_name(&self) -> &str {
        &self.server_name
    }
}

#[async_trait]
impl ToolHandler for McpToolHandler {
    fn name(&self) -> String {
        self.full_name.clone()
    }

    fn description(&self) -> String {
        self.description.clone()
    }

    fn parameters(&self) -> serde_json::Value {
        self.input_schema.clone()
    }

    async fn execute(&self, _args: serde_json::Value, _ctx: &ToolContext) -> Result<ToolOutput, ToolError> {
        // MCP tool execution would call the actual MCP client.
        // For now, returns a placeholder indicating MCP integration is available.
        Ok(ToolOutput::success_with_prompt(
            serde_json::json!({
                "status": "mcp_not_connected",
                "server": self.server_name,
                "tool": self.tool_name,
                "note": "MCP tool execution requires a live MCP connection. The tool is registered and available for future use."
            }),
            format!("\n[MCP] Tool `{}` from server `{}` is registered. Connect the MCP server to use it.",
                self.tool_name, self.server_name),
        ))
    }
}

/// Register all MCP tools from a manager into a ToolRegistry.
/// Filters out known-broken tools (web_fetch_exa returns null on free tier).
pub fn register_mcp_tools(
    registry: &mut crate::registry::ToolRegistry,
    manager: &oz_mcp::McpManager,
) -> usize {
    // Known-broken MCP tools — these return empty/null content.
    const SKIP_TOOLS: &[&str] = &["web_fetch_exa"];

    let mut count = 0;
    for tool in manager.all_tools() {
        if SKIP_TOOLS.contains(&tool.name.as_str()) {
            tracing::info!("[mcp] skipping known-broken tool: {}", tool.name);
            continue;
        }
        let handler = McpToolHandler::from_mcp_tool("mcp", &tool);
        let name = handler.full_name().to_string();
        registry.register_with_name(&name, handler);
        count += 1;
    }
    count
}

/// Convert an MCP tool definition to a ToolHandler for a specific server.
pub fn mcp_tool_to_handler(server_name: &str, tool: &oz_mcp::McpTool) -> Box<dyn ToolHandler> {
    Box::new(McpToolHandler::from_mcp_tool(server_name, tool))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_mcp_tool_handler_name() {
        let handler = McpToolHandler::new(
            "playwright",
            "screenshot",
            "Take a screenshot",
            serde_json::json!({"type": "object"}),
        );
        assert_eq!(handler.name(), "mcp__playwright__screenshot");
        assert_eq!(handler.description(), "Take a screenshot");
    }

    #[test]
    fn test_mcp_tool_handler_parameters() {
        let schema = serde_json::json!({"type": "object", "properties": {"url": {"type": "string"}}});
        let handler = McpToolHandler::new("test_srv", "test_tool", "desc", schema.clone());
        assert_eq!(handler.parameters(), schema);
    }

    #[tokio::test]
    async fn test_mcp_tool_handler_execute_placeholder() {
        let handler = McpToolHandler::new("test_srv", "test_tool", "A test tool", serde_json::json!({}));
        let ctx = oz_core_types::ToolContext::test();
        let result = handler.execute(serde_json::json!({}), &ctx).await.unwrap();
        assert_eq!(result.data["status"], "mcp_not_connected");
        assert_eq!(result.data["server"], "test_srv");
    }
}
