use async_trait::async_trait;
use oz_core_types::{ToolContext, ToolError, ToolOutput};

use crate::registry::ToolHandler;

/// Update working memory checkpoint during a multi-step task.
pub struct WorkingMemTool;

#[async_trait]
impl ToolHandler for WorkingMemTool {
    fn name(&self) -> String {
        "working_mem".to_string()
    }
    fn description(&self) -> String {
        "Update the working memory checkpoint with current progress, key info, or plan summary."
            .to_string()
    }
    fn parameters(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "key_info": {
                    "type": "string",
                    "description": "Key info to remember"
                },
                "related_sop": {
                    "type": "string",
                    "description": "Related SOP or instructions"
                },
                "plan_mode": {
                    "type": "string",
                    "description": "Plan mode override"
                }
            },
            "required": []
        })
    }

    async fn execute(
        &self,
        args: serde_json::Value,
        _ctx: &ToolContext,
    ) -> Result<ToolOutput, ToolError> {
        let key_info = args.get("key_info").and_then(|v| v.as_str());
        let _related_sop = args.get("related_sop").and_then(|v| v.as_str());

        let mut data = serde_json::json!({"status": "ok"});
        if let Some(ki) = key_info {
            data["key_info"] = serde_json::json!(ki);
        }

        Ok(ToolOutput::success_with_prompt(
            data,
            "\n[working_mem] checkpoint updated",
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use oz_core_types::ToolContext as TC;

    fn ctx() -> TC {
        TC {
            working_dir: "/tmp".into(),
            assets_dir: "/tmp".into(),
            script_dir: "/tmp".into(),
            lang: "en".into(),
            skill_mcp_dir: None,
            harness_dir: None,
            session_id: String::new(),
        }
    }

    #[tokio::test]
    async fn test_working_mem_basic() {
        let r = WorkingMemTool
            .execute(
                serde_json::json!({"key_info": "current task: read file"}),
                &ctx(),
            )
            .await
            .unwrap();
        assert_eq!(r.data["status"], "ok");
        assert_eq!(r.data["key_info"], "current task: read file");
    }

    #[tokio::test]
    async fn test_working_mem_empty() {
        let r = WorkingMemTool
            .execute(serde_json::json!({}), &ctx())
            .await
            .unwrap();
        assert_eq!(r.data["status"], "ok");
    }

    #[tokio::test]
    async fn test_working_mem_with_all_params() {
        let r = WorkingMemTool
            .execute(
                serde_json::json!({
                    "key_info": "step 2",
                    "related_sop": "some instructions",
                    "plan_mode": "careful"
                }),
                &ctx(),
            )
            .await
            .unwrap();
        assert_eq!(r.data["status"], "ok");
        assert_eq!(r.data["key_info"], "step 2");
    }

    #[tokio::test]
    async fn test_working_mem_related_sop_no_key_info() {
        let r = WorkingMemTool
            .execute(serde_json::json!({"related_sop": "step-by-step"}), &ctx())
            .await
            .unwrap();
        assert_eq!(r.data["status"], "ok");
        assert!(r.data.get("key_info").is_none());
    }

    #[tokio::test]
    async fn test_working_mem_next_prompt_contains_marker() {
        let r = WorkingMemTool
            .execute(serde_json::json!({"key_info": "test"}), &ctx())
            .await
            .unwrap();
        let prompt = r.next_prompt.unwrap_or_default();
        assert!(
            prompt.contains("working_mem"),
            "next_prompt should reference working_mem"
        );
    }

    #[tokio::test]
    async fn test_working_mem_should_not_exit() {
        let r = WorkingMemTool
            .execute(serde_json::json!({}), &ctx())
            .await
            .unwrap();
        assert!(!r.should_exit);
    }

    #[tokio::test]
    async fn test_working_mem_name_and_description() {
        assert_eq!(WorkingMemTool.name(), "working_mem");
        assert!(!WorkingMemTool.description().is_empty());
    }

    #[tokio::test]
    async fn test_working_mem_parameters_has_required_fields() {
        let params = WorkingMemTool.parameters();
        assert_eq!(params["type"], "object");
        assert!(params["properties"].is_object());
    }
}

#[linkme::distributed_slice(crate::registry::TOOL_FACTORIES)]
fn register_working_mem(reg: &mut crate::registry::ToolRegistry) {
    reg.register(crate::working_mem::WorkingMemTool);
}
