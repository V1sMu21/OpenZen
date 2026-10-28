use async_trait::async_trait;
use oz_core_types::{ToolContext, ToolError, ToolOutput};
use oz_skill_mcp::SkillMcpStore;

use crate::registry::ToolHandler;

/// Record important information into long-term memory.
///
/// Now delegates to `SkillMcpStore` and `skill_mcp_store` tool under the hood.
/// Backward-compatible with the old `long_term` API.
pub struct LongTermTool;

/// Longest title we hand to the SOP store as a name.
const MAX_TITLE_CHARS: usize = 80;

/// Derive a short, filesystem-safe SOP title from a memory payload.
///
/// The model sends whole documents to `long_term`; the store turns the name
/// into a filename, so the title must be one bounded line, never the blob.
pub(crate) fn sop_title(data: &str) -> String {
    let first = data
        .lines()
        .map(str::trim)
        .find(|line| !line.is_empty())
        .unwrap_or("")
        .trim_start_matches('#')
        .trim();
    let capped: String = first.chars().take(MAX_TITLE_CHARS).collect();
    if capped.trim().is_empty() {
        "long-term memory".to_string()
    } else {
        capped
    }
}

#[async_trait]
impl ToolHandler for LongTermTool {
    fn name(&self) -> String {
        "long_term".to_string()
    }
    fn description(&self) -> String {
        "Record important information into long-term memory (facts, SOPs, history). ".to_string()
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

    async fn execute(
        &self,
        args: serde_json::Value,
        ctx: &ToolContext,
    ) -> Result<ToolOutput, ToolError> {
        let data = args["data"]
            .as_str()
            .ok_or_else(|| ToolError::Custom("missing data".into()))?;
        let category_str = args["category"].as_str().unwrap_or("fact");

        let skill_mcp_dir = ctx.skill_mcp_dir.as_deref().unwrap_or(&ctx.working_dir);
        let mut store = SkillMcpStore::new(
            &std::path::PathBuf::from(&ctx.working_dir),
            Some(std::path::PathBuf::from(skill_mcp_dir)),
        );

        match category_str {
            "sop" => {
                let tools: Vec<(String, serde_json::Value)> = Vec::new();
                // The payload is a whole SOP document, not a title: pass a
                // bounded first-line title as the name and keep the full text
                // as the description/content. Handing the blob in as the name
                // made the store build a ~1000-char filename and every
                // `long_term(category="sop")` call died with
                // `File name too long (os error 63)` (2026-10-23).
                let title = crate::long_term::sop_title(data);
                store
                    .sops
                    .crystallise(&title, data, &tools, None, None)
                    .map_err(|e| ToolError::Custom(e.to_string()))?;

                Ok(ToolOutput::success_with_prompt(
                    serde_json::json!({"status": "memorized", "category": "sop", "name": title}),
                    "\n[long_term] SOP stored in .skill_mcp/sops/".to_string(),
                ))
            }
            "history" => {
                store
                    .archive_session(data)
                    .await
                    .map_err(|e| ToolError::Custom(e.to_string()))?;

                Ok(ToolOutput::success_with_prompt(
                    serde_json::json!({"status": "memorized", "category": "history"}),
                    "\n[long_term] Session archived in .skill_mcp/sessions/".to_string(),
                ))
            }
            _ => {
                store
                    .distill_memory(data, &oz_core_types::SkillMcpType::Fact)
                    .await
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
            is_subagent: false,
            working_dir: tmp.to_string_lossy().to_string(),
            assets_dir: "/tmp".into(),
            script_dir: "/tmp".into(),
            lang: "en".into(),
            skill_mcp_dir: Some(tmp.to_string_lossy().to_string()),
            harness_dir: None,
            session_id: String::new(),
            tool_budget_secs: 0,
        }
    }

    #[tokio::test]
    async fn test_long_term_fact() {
        let result = LongTermTool
            .execute(
                serde_json::json!({"data": "important fact", "category": "fact"}),
                &ctx(),
            )
            .await
            .unwrap();
        assert_eq!(result.data["status"], "memorized");
        assert_eq!(result.data["category"], "fact");
    }

    #[tokio::test]
    async fn test_long_term_history() {
        let result = LongTermTool
            .execute(
                serde_json::json!({"data": "session log content", "category": "history"}),
                &ctx(),
            )
            .await
            .unwrap();
        assert_eq!(result.data["status"], "memorized");
    }

    #[tokio::test]
    async fn test_long_term_missing_data() {
        let result = LongTermTool
            .execute(serde_json::json!({"category": "fact"}), &ctx())
            .await;
        assert!(result.is_err());
    }

    #[test]
    fn sop_title_is_one_bounded_line() {
        // Real payload from session 4596ac63: the whole SOP document was used
        // as the store name, producing a ~1000-char filename and
        // `File name too long (os error 63)` on every call.
        let doc = format!(
            "【DSH 接入 OpenCode Claude 的实测契约】\n① 唯一配置面：{}\n",
            "很长的正文".repeat(200)
        );
        let title = sop_title(&doc);
        assert_eq!(title, "【DSH 接入 OpenCode Claude 的实测契约】");

        let long_single_line = "x".repeat(500);
        assert_eq!(
            sop_title(&long_single_line).chars().count(),
            MAX_TITLE_CHARS
        );

        assert_eq!(sop_title("\n\n   \n"), "long-term memory");
        assert_eq!(sop_title("## 标题\n正文"), "标题");
    }

    #[tokio::test]
    async fn test_long_term_sop_with_a_whole_document_payload() {
        // Would previously fail with `File name too long (os error 63)`.
        let doc = format!(
            "【DSH 接入 OpenCode Claude 的实测契约】\n{}",
            "细节行，含 / 与 : 等字符。\n".repeat(60)
        );
        let result = LongTermTool
            .execute(serde_json::json!({"data": doc, "category": "sop"}), &ctx())
            .await
            .unwrap();
        assert_eq!(result.data["status"], "memorized");
        assert_eq!(result.data["category"], "sop");
        let name = result.data["name"].as_str().unwrap_or("");
        assert!(!name.is_empty() && name.chars().count() <= MAX_TITLE_CHARS);
    }
}

#[linkme::distributed_slice(crate::registry::TOOL_FACTORIES)]
fn register_long_term(reg: &mut crate::registry::ToolRegistry) {
    reg.register(crate::long_term::LongTermTool);
}
