use async_trait::async_trait;
use oz_core_types::{ToolContext, ToolError, ToolOutput};

use crate::registry::ToolHandler;

pub struct TodoUpdateTool;

#[async_trait]
impl ToolHandler for TodoUpdateTool {
    fn name(&self) -> String { "todoupdate".to_string() }
    fn description(&self) -> String {
        "Mark ONE todo item as in_progress, completed, or cancelled. Call once per todo — do NOT batch multiple updates. REQUIRED: always pass the todo's `content` so the chat card shows what changed.".to_string()
    }
    fn description_zh(&self) -> String {
        "标记【一条】待办事项为进行中、已完成或已取消。一次只更新一个——不要批量更新多个。必须传 `content` 让卡片显示变更内容。".to_string()
    }
    fn parameters(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "id": {
                    "type": "string",
                    "description": "Todo ID returned by todowrite"
                },
                "status": {
                    "type": "string",
                    "enum": ["in_progress", "completed", "cancelled"],
                    "description": "New status for the todo item"
                },
                "content": {
                    "type": "string",
                    "description": "The todo's description text (same as what todowrite returned) — echoed back so the chat card shows what was updated"
                }
            },
            "required": ["id", "status"]
        })
    }

    async fn execute(&self, args: serde_json::Value, _ctx: &ToolContext) -> Result<ToolOutput, ToolError> {
        let id = args["id"].as_str().unwrap_or("").to_string();
        let status = args["status"].as_str().unwrap_or("in_progress").to_string();
        let content = args["content"].as_str().unwrap_or("").to_string();

        if id.is_empty() {
            return Err(ToolError::Custom("todoupdate requires a valid todo id".into()));
        }

        let status_emoji = match status.as_str() {
            "in_progress" => "◉",
            "completed" => "✅",
            "cancelled" => "✕",
            _ => "→",
        };

        let mut result = serde_json::json!({
            "status": "ok",
            "todo_id": id.clone(),
            "new_status": status.clone(),
        });
        if !content.is_empty() {
            result["content"] = serde_json::Value::String(content.clone());
        }

        let prompt = if content.is_empty() {
            format!("\n[todoupdate] {id} → {status}")
        } else {
            format!("\n[todoupdate] {status_emoji} {status}: {content}")
        };

        Ok(ToolOutput::success_with_prompt(result, &prompt))
    }
}
