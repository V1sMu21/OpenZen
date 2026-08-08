use async_trait::async_trait;
use oz_core_types::{ToolContext, ToolDefinition, ToolError, ToolFunction, ToolOutput};

use crate::registry::ToolHandler;

pub struct NoTool;

#[async_trait]
impl ToolHandler for NoTool {
    fn name(&self) -> String { "respond".to_string() }
    fn description(&self) -> String { "Provide a final text response to the user. Use this when you have completed the task and want to reply.".to_string() }
    fn parameters(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "response": {
                    "type": "string",
                    "description": "Text response to user"
                }
            },
            "required": ["response"]
        })
    }

    async fn execute(&self, args: serde_json::Value, _ctx: &ToolContext) -> Result<ToolOutput, ToolError> {
        let response = args["response"].as_str().unwrap_or("");
        Ok(ToolOutput {
            data: serde_json::json!({"status": "ok", "response": response}),
            next_prompt: None,
            should_exit: true,
            images: vec![],
        })
    }
}

// Backward compat
use std::sync::Arc;
use oz_core_types::StepOutcome;

pub fn definition() -> ToolDefinition {
    let t = NoTool;
    ToolDefinition {
        type_: "function".into(),
        function: ToolFunction {
            name: t.name().into(),
            description: t.description().into(),
            parameters: t.parameters(),
        },
    }
}

pub fn handler() -> super::ToolHandler {
    let t = Arc::new(NoTool);
    Arc::new(move |_name: &str, args: &serde_json::Value, ctx: &oz_core_types::ToolContext| {
        let args = args.clone();
        let ctx = ctx.clone();
        let t = t.clone();
        let rt = tokio::runtime::Runtime::new().expect("Failed to create tokio runtime");
        let result = rt.block_on(t.execute(args, &ctx))
            .unwrap_or_else(|e| ToolOutput::bad_json(e.to_string()));
        StepOutcome { data: result.data, next_prompt: result.next_prompt, should_exit: result.should_exit, images: result.images }
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use oz_core_types::ToolContext as TC;

    fn ctx() -> TC {
        TC { working_dir: "/tmp".into(), assets_dir: "/tmp".into(), script_dir: "/tmp".into(), lang: "en".into(), skill_mcp_dir: None }
    }

    #[tokio::test]
    async fn test_name() {
        assert_eq!(NoTool.name(), "respond");
    }

    #[tokio::test]
    async fn test_execute_with_response() {
        let r = NoTool.execute(serde_json::json!({"response": "hello"}), &ctx()).await.unwrap();
        assert_eq!(r.data["status"], "ok");
        assert_eq!(r.data["response"], "hello");
        assert!(r.should_exit);
        assert!(r.next_prompt.is_none());
    }

    #[tokio::test]
    async fn test_execute_empty() {
        let r = NoTool.execute(serde_json::json!({}), &ctx()).await.unwrap();
        assert_eq!(r.data["status"], "ok");
        assert_eq!(r.data["response"], "");
    }
}

#[linkme::distributed_slice(crate::registry::TOOL_FACTORIES)]
fn register_no_tool(reg: &mut crate::registry::ToolRegistry) {
    reg.register(crate::no_tool::NoTool);
}