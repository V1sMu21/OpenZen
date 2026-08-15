use async_trait::async_trait;
use oz_core_types::{ToolContext, ToolError, ToolOutput};
use oz_skill_mcp::{Skill, SkillMcpStore};

use crate::registry::ToolHandler;

pub struct SkillMcpSearchTool;

#[async_trait]
impl ToolHandler for SkillMcpSearchTool {
    fn name(&self) -> String {
        "skill_mcp_search".to_string()
    }
    fn description(&self) -> String {
        "Search the skill/MCP registry for relevant skills, SOPs, and facts matching a query."
            .to_string()
    }
    fn parameters(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "query": {
                    "type": "string",
                    "description": "Search query"
                },
                "max_results": {
                    "type": "integer",
                    "description": "Max results (default 3)",
                    "default": 3
                }
            },
            "required": ["query"]
        })
    }

    async fn execute(
        &self,
        args: serde_json::Value,
        ctx: &ToolContext,
    ) -> Result<ToolOutput, ToolError> {
        let query = args["query"].as_str().unwrap_or("").trim().to_string();
        let max = args["max_results"].as_u64().unwrap_or(5) as usize;
        let skill_mcp_dir = ctx
            .skill_mcp_dir
            .as_deref()
            .unwrap_or_else(|| &ctx.working_dir);

        let store = SkillMcpStore::new(
            &std::path::PathBuf::from(&ctx.working_dir),
            Some(std::path::PathBuf::from(skill_mcp_dir)),
        );

        // Vague / list-style queries: when the model asks "what skills
        // are installed?" it often calls search with a word like
        // "installed" or "available" that no skill name contains. In
        // that case we fall back to listing all active skills so the
        // user actually gets an answer instead of an empty result.
        let vague_list_triggers = [
            "installed",
            "available",
            "list",
            "all",
            "show",
            "what",
            "which",
            "have",
            "loaded",
            "registered",
            "available",
            "exists",
            "exist",
            "skill",
            "skills",
            "mcp",
            "tools",
        ];
        let lower = query.to_lowercase();
        let is_vague = query.is_empty()
            || query
                .split_whitespace()
                .any(|t| vague_list_triggers.contains(&t))
            || lower.contains("what skill")
            || lower.contains("which skill")
            || lower.contains("show me")
            || lower.contains("list all");

        if is_vague {
            let all_skills: Vec<&Skill> = store
                .skills
                .list()
                .iter()
                .filter(|s| s.metadata.is_active())
                .collect();
            let all_sops = store
                .sops
                .all()
                .iter()
                .filter(|s| s.metadata.is_active())
                .collect::<Vec<_>>();

            let mut results = Vec::new();
            for skill in all_skills.iter().take(max) {
                results.push(serde_json::json!({
                    "type": "skill",
                    "name": skill.name,
                    "description": skill.description,
                    "tags": skill.tags,
                    "quality": skill.metadata.quality_score,
                    "success_count": skill.metadata.success_count,
                }));
            }
            for sop in all_sops.iter().take(max) {
                results.push(serde_json::json!({
                    "type": "sop",
                    "name": sop.name,
                    "description": sop.description,
                    "tags": sop.tags,
                    "quality": sop.metadata.quality_score,
                    "success_count": sop.metadata.success_count,
                }));
            }

            let total = all_skills.len() + all_sops.len();
            let prompt = if total == 0 {
                "\n[skill_mcp_search] No skills or SOPs are currently registered.".to_string()
            } else {
                let mut p = format!(
                    "\n[skill_mcp_search] No keyword match for '{}', but here are all {total} registered items:\n\n",
                    if query.is_empty() { "(empty query)" } else { query.as_str() }
                );
                if !all_skills.is_empty() {
                    p.push_str(&format!("**Skills ({}):**\n", all_skills.len()));
                    for s in &all_skills {
                        p.push_str(&format!("- **{}** — {}\n", s.name, s.description));
                    }
                }
                if !all_sops.is_empty() {
                    p.push_str(&format!("\n**SOPs ({}):**\n", all_sops.len()));
                    for s in &all_sops {
                        p.push_str(&format!("- **{}** — {}\n", s.name, s.description));
                    }
                }
                p
            };
            return Ok(ToolOutput::success_with_prompt(
                serde_json::json!({
                    "status": "ok",
                    "results": results,
                    "total": results.len(),
                    "fallback": "list_all",
                }),
                prompt,
            ));
        }

        let skills = store.skills.find_matching(&query);
        let sops = store.sops.find_matching(&query);

        let mut results = Vec::new();

        // P2-7: FTS5 memory backend — search distilled facts/insights too.
        let fts_memory = oz_memory::MemoryFts::open(
            &std::path::PathBuf::from(skill_mcp_dir).join("memory_fts.sqlite"),
        )
        .ok();
        let fts_hits = fts_memory
            .as_ref()
            .and_then(|f| f.search(&query, max).ok())
            .unwrap_or_default();

        for skill in skills.iter().take(max) {
            results.push(serde_json::json!({
                "type": "skill",
                "name": skill.name,
                "description": skill.description,
                "tags": skill.tags,
                "quality": skill.metadata.quality_score,
                "success_count": skill.metadata.success_count,
            }));
        }

        for sop in sops.iter().take(max) {
            results.push(serde_json::json!({
                "type": "sop",
                "name": sop.name,
                "description": sop.description,
                "tags": sop.tags,
                "quality": sop.metadata.quality_score,
                "success_count": sop.metadata.success_count,
            }));
        }

        for hit in fts_hits.iter().take(max) {
            results.push(serde_json::json!({
                "type": "memory",
                "category": hit.category,
                "content": hit.content,
            }));
        }

        let prompt = if results.is_empty() {
            "\n[skill_mcp_search] No matching skill or SOP found.".to_string()
        } else {
            let mut p = format!(
                "\n[skill_mcp_search] Found {} results. Full content loaded below:\n\n",
                results.len()
            );
            for (i, r) in results.iter().enumerate() {
                let kind = r["type"].as_str().unwrap_or("?");
                let name = r["name"].as_str().unwrap_or("?");
                let desc = r["description"].as_str().unwrap_or("");
                p.push_str(&format!(
                    "--- Result {}/{} [{kind}] **{name}** — {desc} ---\n\n",
                    i + 1,
                    results.len()
                ));
            }
            // Append full skill content for matched skills
            for skill in skills.iter().take(max) {
                let snippet = skill.to_full_content();
                // Guard: cap at ~3000 chars to prevent runaway tokens from huge skills
                let max_chars = 3000;
                let body: String = if snippet.len() > max_chars {
                    let trunc_at = snippet
                        .char_indices()
                        .nth(max_chars)
                        .map(|(i, _)| i)
                        .unwrap_or(snippet.len());
                    format!("{}…\n[truncated at {} chars, {} total — use `file_read(\"{}\")` for full content]",
                        &snippet[..trunc_at], max_chars, snippet.len(), skill.source_path.display())
                } else {
                    snippet
                };
                p.push_str(&body);
                p.push_str("\n\n");
            }
            if !fts_hits.is_empty() {
                p.push_str(&format!("**Memory hits ({}):**\n", fts_hits.len()));
                for hit in &fts_hits {
                    p.push_str(&format!("- [{}] {}\n", hit.category, hit.content));
                }
            }
            p
        };

        Ok(ToolOutput::success_with_prompt(
            serde_json::json!({"status": "ok", "results": results, "total": results.len()}),
            prompt,
        ))
    }
}

pub struct SkillMcpListTool;

#[async_trait]
impl ToolHandler for SkillMcpListTool {
    fn name(&self) -> String {
        "skill_mcp_list".to_string()
    }
    fn description(&self) -> String {
        "List all available skills and SOPs in the skill/MCP registry.".to_string()
    }
    fn parameters(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "category": {
                    "type": "string",
                    "enum": ["all", "skills", "sops"],
                    "description": "Category to list",
                    "default": "all"
                }
            },
            "required": []
        })
    }

    async fn execute(
        &self,
        args: serde_json::Value,
        ctx: &ToolContext,
    ) -> Result<ToolOutput, ToolError> {
        let category = args["category"].as_str().unwrap_or("all");
        let skill_mcp_dir = ctx
            .skill_mcp_dir
            .as_deref()
            .unwrap_or_else(|| &ctx.working_dir);

        let store = SkillMcpStore::new(
            &std::path::PathBuf::from(&ctx.working_dir),
            Some(std::path::PathBuf::from(skill_mcp_dir)),
        );

        let mut skills_list = Vec::new();
        let mut sops_list = Vec::new();

        if category == "all" || category == "skills" {
            for s in store.skills.list() {
                skills_list.push(serde_json::json!({
                    "name": s.name,
                    "description": s.description,
                    "tags": s.tags,
                    "tools": s.required_tools,
                    "quality": s.metadata.quality_score,
                    "uses": s.metadata.total_uses(),
                }));
            }
        }

        if category == "all" || category == "sops" {
            for s in store.sops.all() {
                sops_list.push(serde_json::json!({
                    "name": s.name,
                    "description": s.description,
                    "tags": s.tags,
                    "quality": s.metadata.quality_score,
                    "uses": s.metadata.total_uses(),
                }));
            }
        }

        let total = skills_list.len() + sops_list.len();
        let prompt = if total == 0 {
            "\n[skill_mcp_list] No skill or SOP artifacts found.".to_string()
        } else {
            let mut p = format!("\n[skill_mcp_list] {total} items available:\n\n");
            if !skills_list.is_empty() {
                p.push_str("**Skills:**\n");
                for s in &skills_list {
                    p.push_str(&format!(
                        "- {} — {} [{:?}] (uses: {})\n",
                        s["name"].as_str().unwrap_or("?"),
                        s["description"].as_str().unwrap_or(""),
                        s["tags"].as_array().map(|a| a.len()).unwrap_or(0),
                        s["uses"].as_u64().unwrap_or(0),
                    ));
                }
            }
            if !sops_list.is_empty() {
                p.push_str("\n**SOPs:**\n");
                for s in &sops_list {
                    p.push_str(&format!(
                        "- {} — {} [{:?} tags] (uses: {})\n",
                        s["name"].as_str().unwrap_or("?"),
                        s["description"].as_str().unwrap_or(""),
                        s["tags"].as_array().map(|a| a.len()).unwrap_or(0),
                        s["uses"].as_u64().unwrap_or(0),
                    ));
                }
            }
            p
        };

        Ok(ToolOutput::success_with_prompt(
            serde_json::json!({
                "status": "ok",
                "skills": skills_list,
                "sops": sops_list,
                "total": total,
            }),
            prompt,
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use oz_core_types::ToolContext;

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

    fn setup_test_skill(ctx: &ToolContext) {
        let store = SkillMcpStore::new(
            &std::path::PathBuf::from(&ctx.working_dir),
            Some(std::path::PathBuf::from(
                ctx.skill_mcp_dir.as_deref().unwrap_or(""),
            )),
        );
        // Create a test skill via file
        let skills_dir = store.base_dir().join("skills").join("test_search_skill");
        std::fs::create_dir_all(&skills_dir).unwrap();
        std::fs::write(
            skills_dir.join("SKILL.md"),
            "# test_search_skill — A skill for searching\nTags: test, search\n\n## Procedure\n1. Do something\n",
        ).unwrap();
    }

    #[tokio::test]
    async fn test_skill_mcp_search_empty_query() {
        let c = ctx();
        setup_test_skill(&c);
        // Empty query should now return "ok" + fallback list, not the
        // old "empty_query" status (which left the user with an
        // unhelpful empty result when asking "what's installed?").
        let result = SkillMcpSearchTool
            .execute(serde_json::json!({"query": ""}), &c)
            .await
            .unwrap();
        assert_eq!(result.data["status"], "ok");
        assert_eq!(result.data["fallback"], "list_all");
        assert!(result.data["total"].as_u64().unwrap() >= 1);
    }

    #[tokio::test]
    async fn test_skill_mcp_search_vague_query_lists_all() {
        let c = ctx();
        setup_test_skill(&c);
        // "installed" was the trigger that surfaced the bug — no skill
        // name contains "installed", so the old code returned zero
        // results and the user saw "no skills found" even though we
        // have one.
        let result = SkillMcpSearchTool
            .execute(serde_json::json!({"query": "installed"}), &c)
            .await
            .unwrap();
        assert_eq!(result.data["status"], "ok");
        assert!(result.data["total"].as_u64().unwrap() >= 1);
    }

    #[tokio::test]
    async fn test_skill_mcp_search_with_skill() {
        let c = ctx();
        setup_test_skill(&c);
        let result = SkillMcpSearchTool
            .execute(serde_json::json!({"query": "search"}), &c)
            .await
            .unwrap();
        assert_eq!(result.data["status"], "ok");
    }

    #[tokio::test]
    async fn test_skill_mcp_search_fts_memory_backend() {
        let c = ctx();
        setup_test_skill(&c);
        let fts_path =
            std::path::PathBuf::from(c.skill_mcp_dir.as_deref().unwrap()).join("memory_fts.sqlite");
        let fts = oz_memory::MemoryFts::open(&fts_path).unwrap();
        fts.insert(
            "",
            "fact",
            "deployment uses port 8001 for the kanban backend",
        )
        .unwrap();
        drop(fts);
        let result = SkillMcpSearchTool
            .execute(serde_json::json!({"query": "deployment"}), &c)
            .await
            .unwrap();
        let results = result.data["results"].as_array().unwrap();
        assert!(
            results
                .iter()
                .any(|r| r["type"] == "memory" && r["category"] == "fact"),
            "FTS memory backend must surface indexed facts, got: {results:?}"
        );
    }

    #[tokio::test]
    async fn test_skill_mcp_list_empty() {
        let c = ctx();
        let result = SkillMcpListTool
            .execute(serde_json::json!({}), &c)
            .await
            .unwrap();
        assert_eq!(result.data["total"], 0);
    }

    #[tokio::test]
    async fn test_skill_mcp_list_with_skill() {
        let c = ctx();
        setup_test_skill(&c);
        let result = SkillMcpListTool
            .execute(serde_json::json!({"category": "skills"}), &c)
            .await
            .unwrap();
        assert!(result.data["total"].as_u64().unwrap() > 0);
    }
}
