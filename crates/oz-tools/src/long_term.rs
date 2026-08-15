use async_trait::async_trait;
use oz_core_types::{ToolContext, ToolError, ToolOutput};
use oz_skill_mcp::SkillMcpStore;

use crate::registry::ToolHandler;

/// Record important information into long-term memory.
///
/// Now delegates to `SkillMcpStore` and `skill_mcp_store` tool under the hood.
/// Backward-compatible with the old `long_term` API.
pub struct LongTermTool;

#[async_trait]
impl ToolHandler for LongTermTool {
    fn name(&self) -> String { "long_term".to_string() }
    fn description(&self) -> String {
        "Record important information into long-term memory (facts, SOPs, history). "
            .to_string()
    }
    fn parameters(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "data": {
                    "type": "string",
                    "description": "Info to remember"
                },
                "category": {
                    "type": "string",
                    "enum": ["fact", "sop", "history"],
                    "description": "Type: fact, sop, or history"
                }
            },
            "required": ["data", "category"]
        })
    }

    async fn execute(&self, args: serde_json::Value, ctx: &ToolContext) -> Result<ToolOutput, ToolError> {
        let data = args["data"].as_str().ok_or_else(|| ToolError::Custom("missing data".into()))?;
        let category_str = args["category"].as_str().unwrap_or("fact");

        let skill_mcp_dir = ctx.skill_mcp_dir.as_deref().unwrap_or(&ctx.working_dir);
        let mut store = SkillMcpStore::new(
            &std::path::PathBuf::from(&ctx.working_dir),
            Some(std::path::PathBuf::from(skill_mcp_dir)),
        );

        match category_str {
            "sop" => {
                let tools: Vec<(String, serde_json::Value)> = Vec::new();
                store.sops.crystallise(data, data, &tools, None)
                    .map_err(|e| ToolError::Custom(e.to_string()))?;

                Ok(ToolOutput::success_with_prompt(
                    serde_json::json!({"status": "memorized", "category": "sop"}),
                    "\n[long_term] SOP stored in .skill_mcp/sops/".to_string(),
                ))
            }
            "history" => {
                store.archive_session(data).await
                    .map_err(|e| ToolError::Custom(e.to_string()))?;

                Ok(ToolOutput::success_with_prompt(
                    serde_json::json!({"status": "memorized", "category": "history"}),
                    "\n[long_term] Session archived in .skill_mcp/sessions/".to_string(),
                ))
            }
            _ => {
                store.distill_memory(data, &oz_core_types::SkillMcpType::Fact).await
                    .map_err(|e| ToolError::Custom(e.to_string()))?;

                Ok(ToolOutput::success_with_prompt(
                    serde_json::json!({"status": "memorized", "category": "fact"}),
                    "\n[long_term] Fact stored in .skill_mcp/facts/".to_string(),
                ))
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ctx() -> ToolContext {
        let tmp = std::env::temp_dir().join("oz_test_long_term_v2");
        let _ = std::fs::create_dir_all(&tmp);
        ToolContext {
            working_dir: tmp.to_string_lossy().to_string(),
            assets_dir: "/tmp".into(),
            script_dir: "/tmp".into(),
            lang: "en".into(),
            skill_mcp_dir: Some(tmp.to_string_lossy().to_string()),
            harness_dir: None,
            session_id: String::new(),
        }
    }

    #[tokio::test]
    async fn test_long_term_fact() {
        let result = LongTermTool.execute(
            serde_json::json!({"data": "important fact", "category": "fact"}),
            &ctx(),
        ).await.unwrap();
        assert_eq!(result.data["status"], "memorized");
        assert_eq!(result.data["category"], "fact");
    }

    #[tokio::test]
    async fn test_long_term_history() {
        let result = LongTermTool.execute(
            serde_json::json!({"data": "session log content", "category": "history"}),
            &ctx(),
        ).await.unwrap();
        assert_eq!(result.data["status"], "memorized");
    }

    #[tokio::test]
    async fn test_long_term_missing_data() {
        let result = LongTermTool.execute(serde_json::json!({"category": "fact"}), &ctx()).await;
        assert!(result.is_err());
    }
}

#[linkme::distributed_slice(crate::registry::TOOL_FACTORIES)]
fn register_long_term(reg: &mut crate::registry::ToolRegistry) {
    reg.register(crate::long_term::LongTermTool);
}
