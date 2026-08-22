//! Knowledge write & refine tools — allow the agent to manually store and refine knowledge.
//!
//! These complement the automatic crystallization and refinement in `oz-core`.

use async_trait::async_trait;
use oz_core_types::{ToolContext, ToolError, ToolOutput};
use oz_skill_mcp::skill::Skill;
use oz_skill_mcp::SkillMcpStore;

use crate::registry::ToolHandler;

pub struct SkillMcpStoreTool;

#[async_trait]
impl ToolHandler for SkillMcpStoreTool {
    fn name(&self) -> String {
        "skill_mcp_store".to_string()
    }
    fn description(&self) -> String {
        "Store a new fact, skill, or SOP into the skill/MCP registry. Use this to remember important information.".to_string()
    }
    fn parameters(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "category": {
                    "type": "string",
                    "enum": ["fact", "skill", "sop"],
                    "description": "Type: fact, skill, or sop"
                },
                "name": {
                    "type": "string",
                    "description": "Name (used as filename)"
                },
                "content": {
                    "type": "string",
                    "description": "Content to store"
                },
                "description": {
                    "type": "string",
                    "description": "Short description"
                },
                "tags": {
                    "type": "array",
                    "items": {"type": "string"},
                    "description": "Searchable tags."
                }
            },
            "required": ["category", "name", "content"]
        })
    }

    async fn execute(
        &self,
        args: serde_json::Value,
        ctx: &ToolContext,
    ) -> Result<ToolOutput, ToolError> {
        let category = args["category"].as_str().unwrap_or("fact");
        let name = args["name"].as_str().unwrap_or("unnamed");
        let content = args["content"].as_str().unwrap_or("");
        let description = args["description"].as_str().unwrap_or("");
        let tags: Vec<String> = args["tags"]
            .as_array()
            .map(|arr| {
                arr.iter()
                    .filter_map(|v| v.as_str().map(String::from))
                    .collect()
            })
            .unwrap_or_default();

        let skill_mcp_dir = ctx.skill_mcp_dir.as_deref().unwrap_or(&ctx.working_dir);
        let mut store = SkillMcpStore::new(
            &std::path::PathBuf::from(&ctx.working_dir),
            Some(std::path::PathBuf::from(skill_mcp_dir)),
        );

        match category {
            "skill" => {
                let skill_md = if content.starts_with('#') {
                    content.to_string()
                } else {
                    format!(
                        "# {} — {}\nTags: {}\n\n{}\n",
                        name,
                        description,
                        tags.join(", "),
                        content
                    )
                };

                let skill = Skill {
                    name: name.to_string(),
                    description: description.to_string(),
                    tags,
                    required_tools: Vec::new(),
                    content: skill_md,
                    source_path: std::path::PathBuf::new(),
                    metadata: oz_core_types::SkillMcpMetadata::new(name, description, vec![]),
                    quality: 0.5,
                };

                store
                    .skills
                    .register(skill)
                    .map_err(|e| ToolError::Custom(e.to_string()))?;

                Ok(ToolOutput::success_with_prompt(
                    serde_json::json!({"status": "stored", "category": "skill", "name": name}),
                    format!(
                        "\n[skill_mcp_store] Skill '{}' stored in skill/MCP registry.",
                        name
                    ),
                ))
            }
            "sop" => {
                let tools: Vec<(String, serde_json::Value)> = Vec::new();
                let sop = store
                    .crystallise_sop(name, description, &tools, None, None)
                    .map_err(|e| ToolError::Custom(e.to_string()))?;

                Ok(ToolOutput::success_with_prompt(
                    serde_json::json!({"status": "stored", "category": "sop", "name": sop.name}),
                    format!(
                        "\n[skill_mcp_store] SOP '{}' stored in skill/MCP registry.",
                        name
                    ),
                ))
            }
            _ => {
                // Fact
                store
                    .distill_memory(name, &oz_core_types::SkillMcpType::Fact)
                    .await
                    .map_err(|e| ToolError::Custom(e.to_string()))?;

                Ok(ToolOutput::success_with_prompt(
                    serde_json::json!({"status": "stored", "category": "fact", "name": name}),
                    format!("\n[skill_mcp_store] Fact '{}' stored.", name),
                ))
            }
        }
    }
}

pub struct SkillMcpRefineTool;

#[async_trait]
impl ToolHandler for SkillMcpRefineTool {
    fn name(&self) -> String {
        "skill_mcp_refine".to_string()
    }
    fn description(&self) -> String {
        "Refine an existing skill or SOP in the skill/MCP registry based on recent usage."
            .to_string()
    }
    fn parameters(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "name": {
                    "type": "string",
                    "description": "Name of artifact to refine"
                },
                "feedback": {
                    "type": "string",
                    "description": "What to improve"
                }
            },
            "required": ["name"]
        })
    }

    async fn execute(
        &self,
        args: serde_json::Value,
        ctx: &ToolContext,
    ) -> Result<ToolOutput, ToolError> {
        let name = args["name"].as_str().unwrap_or("");
        if name.is_empty() {
            return Ok(ToolOutput::success_with_prompt(
                serde_json::json!({"status": "error", "reason": "name required"}),
                "\n[skill_mcp_refine] Name is required.",
            ));
        }

        let feedback = args["feedback"].as_str().unwrap_or("");
        let skill_mcp_dir = ctx.skill_mcp_dir.as_deref().unwrap_or(&ctx.working_dir);
        let store = SkillMcpStore::new(
            &std::path::PathBuf::from(&ctx.working_dir),
            Some(std::path::PathBuf::from(skill_mcp_dir)),
        );

        // Check if it's a skill
        if let Some(skill) = store.skills.get(name) {
            let mut updated = skill.clone();
            updated.metadata.record_success(0);
            if !feedback.is_empty() {
                updated
                    .content
                    .push_str(&format!("\n\n## Refinement Feedback\n{}\n", feedback));
            }
            let mut mutable_store = SkillMcpStore::new(
                &std::path::PathBuf::from(&ctx.working_dir),
                Some(std::path::PathBuf::from(skill_mcp_dir)),
            );
            mutable_store
                .skills
                .register(updated)
                .map_err(|e| ToolError::Custom(e.to_string()))?;

            return Ok(ToolOutput::success_with_prompt(
                serde_json::json!({"status": "refined", "type": "skill", "name": name}),
                format!("\n[skill_mcp_refine] Skill '{}' updated.", name),
            ));
        }

        // Check if it's a SOP
        if store.sops.get(name).is_some() {
            // SOP refinement: just record success increment
            let mut mutable_store = SkillMcpStore::new(
                &std::path::PathBuf::from(&ctx.working_dir),
                Some(std::path::PathBuf::from(skill_mcp_dir)),
            );
            mutable_store
                .record_sop_success(name, 0)
                .map_err(|e| ToolError::Custom(e.to_string()))?;

            return Ok(ToolOutput::success_with_prompt(
                serde_json::json!({"status": "refined", "type": "sop", "name": name}),
                format!("\n[skill_mcp_refine] SOP '{}' marked as successful.", name),
            ));
        }

        Ok(ToolOutput::success_with_prompt(
            serde_json::json!({"status": "not_found", "name": name}),
            format!(
                "\n[skill_mcp_refine] No skill or SOP named '{}' found.",
                name
            ),
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ctx() -> ToolContext {
        let dir = tempfile::tempdir().unwrap();
        let dir_path = dir.keep().to_string_lossy().to_string();
        ToolContext {
            working_dir: dir_path.clone(),
            assets_dir: String::new(),
            script_dir: String::new(),
            lang: "en".into(),
            skill_mcp_dir: Some(dir_path),
            harness_dir: None,
            session_id: String::new(),
        }
    }

    #[tokio::test]
    async fn test_skill_mcp_store_fact() {
        let c = ctx();
        let result = SkillMcpStoreTool.execute(
            serde_json::json!({"category": "fact", "name": "test_fact", "content": "A test fact"}),
            &c,
        ).await.unwrap();
        assert_eq!(result.data["status"], "stored");
        assert_eq!(result.data["category"], "fact");
    }

    #[tokio::test]
    async fn test_skill_mcp_store_skill() {
        let c = ctx();
        let result = SkillMcpStoreTool
            .execute(
                serde_json::json!({
                    "category": "skill",
                    "name": "test_skill",
                    "content": "# test_skill — Test\nTags: test\n\n## Procedure\n1. Step\n",
                    "description": "A test skill",
                    "tags": ["test"]
                }),
                &c,
            )
            .await
            .unwrap();
        assert_eq!(result.data["status"], "stored");
    }

    #[tokio::test]
    async fn test_skill_mcp_refine_not_found() {
        let c = ctx();
        let result = SkillMcpRefineTool
            .execute(serde_json::json!({"name": "nonexistent"}), &c)
            .await
            .unwrap();
        assert_eq!(result.data["status"], "not_found");
    }
}
