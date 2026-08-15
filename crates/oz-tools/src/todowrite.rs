use async_trait::async_trait;
use oz_core_types::{ToolContext, ToolError, ToolOutput};

use crate::registry::ToolHandler;

pub struct TodoWriteTool;

#[async_trait]
impl ToolHandler for TodoWriteTool {
    fn name(&self) -> String { "todowrite".to_string() }
    fn description(&self) -> String {
        "Create ONE todo item. Call multiple times for multi-step tasks — do NOT put multiple steps in a single call. Each todo must be a single verifiable action. For complex tasks (writes/build/test): create task_spec.md with [verify] acceptance assertions first, then break it into steps.".to_string()
    }
    fn description_zh(&self) -> String {
        "创建【一条】待办事项。多步骤任务请多次调用——不要把多个步骤塞进一次调用。每个待办事项必须是单一可验证的动作。".to_string()
    }
    fn parameters(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "content": {
                    "type": "string",
                    "description": "Todo description"
                },
                "priority": {
                    "type": "string",
                    "enum": ["high", "medium", "low"],
                    "description": "Priority"
                }
            },
            "required": ["content"]
        })
    }

    async fn execute(&self, args: serde_json::Value, _ctx: &ToolContext) -> Result<ToolOutput, ToolError> {
        let content = args["content"].as_str().unwrap_or("").to_string();
        let priority = args["priority"].as_str().unwrap_or("medium").to_string();

        let id = format!("todo_{}", uuid::Uuid::new_v4().to_string().split('-').next().unwrap_or("0"));

        Ok(ToolOutput::success_with_prompt(
            serde_json::json!({
                "status": "ok",
                "todo_id": id,
                "content": content,
                "priority": priority,
            }),
            format!("\n[todowrite] created {:?} todo: {content}", priority),
        ))
    }
}
