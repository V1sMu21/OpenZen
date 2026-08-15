pub mod mcp_client;
pub mod middleware;
pub mod sse;
pub mod stdio;
pub mod types;
pub mod webui;

use oz_core_types::{ToolContext, ToolDefinition};
use oz_tools::registry::ToolRegistry;
use std::sync::Arc;
use tokio::sync::Mutex;

/// Shared MCP server state.
pub struct McpState {
    pub registry: ToolRegistry,
    pub ctx: ToolContext,
    pub session_id: String,
}

impl McpState {
    pub fn new(registry: ToolRegistry, ctx: ToolContext) -> Self {
        McpState {
            registry,
            ctx,
            session_id: uuid::Uuid::new_v4().to_string(),
        }
    }

    pub fn tool_definitions(&self) -> Vec<ToolDefinition> {
        self.registry.to_schema("en")
    }

    pub async fn call_tool(
        &self,
        name: &str,
        args: serde_json::Value,
    ) -> Result<serde_json::Value, String> {
        let result = self
            .registry
            .dispatch(name, args, &self.ctx)
            .await
            .map_err(|e| format!("tool error: {e}"))?;
        Ok(result.data)
    }
}

pub type SharedMcpState = Arc<Mutex<McpState>>;
