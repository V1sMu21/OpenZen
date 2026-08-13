//! Bridges the async [`ToolRegistry`] to the [`Handler`] trait from oz-core.
//!
//! [`ToolRegistryHandler`] implements [`oz_core::handler::Handler`] so the
//! agent loop can dispatch tools through the async trait-based system.

use async_trait::async_trait;
use oz_core::handler::{Handler, WorkingMemory};
use oz_core_types::{MockResponse, MockToolCall, StepOutcome, ToolContext, ToolDefinition, ToolResultItem};

use crate::registry::ToolRegistry;

/// Wraps a [`ToolRegistry`] into the [`Handler`] trait expected by the agent loop.
pub struct ToolRegistryHandler {
    registry: ToolRegistry,
    working: WorkingMemory,
    definitions: Vec<ToolDefinition>,
}

impl ToolRegistryHandler {
    pub fn new(registry: ToolRegistry) -> Self {
        let definitions = registry.to_schema("en");
        ToolRegistryHandler { registry, working: WorkingMemory::default(), definitions }
    }

    pub fn tool_definitions(&self) -> &[ToolDefinition] {
        &self.definitions
    }

    pub fn working_memory(&self) -> &WorkingMemory {
        &self.working
    }

    pub fn working_memory_mut(&mut self) -> &mut WorkingMemory {
        &mut self.working
    }
}

#[async_trait]
impl Handler for ToolRegistryHandler {
    fn working(&self) -> &WorkingMemory {
        &self.working
    }

    fn working_mut(&mut self) -> &mut WorkingMemory {
        &mut self.working
    }

    async fn dispatch(
        &self,
        tool_name: &str,
        args: serde_json::Value,
        _response: &MockResponse,
        _index: u32,
        ctx: &ToolContext,
    ) -> Result<StepOutcome, oz_core_types::ToolError> {
        let output = self.registry.dispatch(tool_name, args, ctx).await?;
        Ok(StepOutcome::from(output))
    }

    fn turn_end(
        &mut self,
        _response: &MockResponse,
        _tool_calls: &[MockToolCall],
        _tool_results: &[ToolResultItem],
        _turn: u32,
        next_prompt: String,
        _exit_reason: Option<String>,
    ) -> String {
        next_prompt
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::registry::ToolHandler;
    use oz_core_types::ToolOutput;

    struct PingTool;
    #[async_trait]
    impl ToolHandler for PingTool {
        fn name(&self) -> String { "ping".to_string() }
        fn description(&self) -> String { "responds pong".to_string() }
        fn parameters(&self) -> serde_json::Value { serde_json::json!({}) }
        async fn execute(&self, _a: serde_json::Value, _c: &ToolContext) -> Result<ToolOutput, oz_core_types::ToolError> {
            Ok(ToolOutput::success(serde_json::json!({"pong": true})))
        }
    }

    fn ctx() -> ToolContext {
        ToolContext {
            working_dir: "/tmp".into(), assets_dir: "/tmp".into(),
            script_dir: "/tmp".into(), lang: "en".into(),
            skill_mcp_dir: None,
            session_id: String::new(),
        }
    }

    #[tokio::test]
    async fn test_dispatch_known_tool() {
        let mut reg = ToolRegistry::new();
        reg.register(PingTool);
        let mut handler = ToolRegistryHandler::new(reg);
        let resp = oz_core_types::MockResponse::new("");
        let result = handler.dispatch("ping", serde_json::json!({}), &resp, 0, &ctx()).await;
        assert!(result.is_ok());
        assert_eq!(result.unwrap().data["pong"], true);
    }

    #[tokio::test]
    async fn test_dispatch_unknown_returns_error_prompt() {
        let reg = ToolRegistry::new();
        let mut handler = ToolRegistryHandler::new(reg);
        let resp = oz_core_types::MockResponse::new("");
        let result = handler.dispatch("nope", serde_json::json!({}), &resp, 0, &ctx()).await.unwrap();
        let msg = result.next_prompt.unwrap_or_default();
        assert!(msg.contains("nope"), "unknown tool should include name in error: {msg}");
    }

    #[tokio::test]
    async fn test_tool_definitions_are_populated() {
        let reg = ToolRegistry::build_default();
        let handler = ToolRegistryHandler::new(reg);
        let defs = handler.tool_definitions();
        assert!(!defs.is_empty());
        for d in defs {
            assert_eq!(d.type_, "function");
            assert!(!d.function.name.is_empty());
        }
    }

    #[tokio::test]
    async fn test_working_memory_is_accessible() {
        let reg = ToolRegistry::new();
        let handler = ToolRegistryHandler::new(reg);
        assert!(handler.working_memory().key_info.is_none());
    }
}
