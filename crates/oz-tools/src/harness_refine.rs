use async_trait::async_trait;
use oz_core::harness::{HarnessKind, refine};
use oz_core_types::{ToolContext, ToolError, ToolOutput};

use crate::registry::ToolHandler;

/// Harness directory: explicit host-provided ledger dir first, then the
/// sibling of the skill/MCP store, `{skill_mcp_dir}/harness`.
fn harness_dir(ctx: &ToolContext) -> std::path::PathBuf {
    if let Some(dir) = &ctx.harness_dir {
        return std::path::PathBuf::from(dir);
    }
    let base = ctx
        .skill_mcp_dir
        .as_deref()
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| std::path::PathBuf::from(&ctx.working_dir).join(".skill_mcp"));
    base.join("harness")
}

/// Record a small, evidence-backed refinement into the continual harness
/// ledger (B2). Mirrors Prime Agent's `/refine`: the base system prompt is
/// never touched — only this supplemental state, with snapshots for rollback.
pub struct HarnessRefineTool;

#[async_trait]
impl ToolHandler for HarnessRefineTool {
    fn name(&self) -> String { "harness_refine".to_string() }
    fn description(&self) -> String {
        "Record a small, evidence-backed lesson into the persistent harness ledger (memory / skill note / subagent spec). Every change is snapshot for rollback. Requires non-empty evidence citing observed behavior.".to_string()
    }
    fn parameters(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "kind": {
                    "type": "string",
                    "enum": ["memory", "skill_note", "subagent_spec"],
                    "description": "What kind of entry to refine"
                },
                "content": {
                    "type": "string",
                    "description": "The lesson/note content"
                },
                "evidence": {
                    "type": "string",
                    "description": "Why this entry exists — cite observed behavior (e.g. 'cargo test failed 3x with E0308 until X')"
                },
                "mode": {
                    "type": "string",
                    "enum": ["upsert", "delete"],
                    "description": "upsert = create or update by content match (default); delete = remove a matching entry"
                }
            },
            "required": ["kind", "content", "evidence"]
        })
    }

    async fn execute(&self, args: serde_json::Value, ctx: &ToolContext) -> Result<ToolOutput, ToolError> {
        let kind = match args.get("kind").and_then(|v| v.as_str()) {
            Some("memory") => HarnessKind::Memory,
            Some("skill_note") => HarnessKind::SkillNote,
            Some("subagent_spec") => HarnessKind::SubagentSpec,
            _ => return Err(ToolError::Custom("kind must be memory | skill_note | subagent_spec".into())),
        };
        let content = args.get("content").and_then(|v| v.as_str()).unwrap_or("");
        let evidence = args.get("evidence").and_then(|v| v.as_str()).unwrap_or("");
        let mode = args.get("mode").and_then(|v| v.as_str()).unwrap_or("upsert");
        let dir = harness_dir(ctx);

        match refine(&dir, kind, content, evidence, "model-initiated refine", mode) {
            Ok(rec) => Ok(ToolOutput::success_with_prompt(
                serde_json::json!({
                    "status": "ok",
                    "record_id": rec.id,
                    "entry_id": rec.entry_id,
                }),
                format!("\n[harness_refine] recorded {:?} refinement (id {}) — snapshot kept for rollback", rec.kind, rec.id),
            )),
            Err(e) => Ok(ToolOutput::success_with_prompt(
                serde_json::json!({"status": "error", "error": e}),
                format!("\n[harness_refine] rejected: {e}"),
            )),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ctx() -> ToolContext {
        let dir = tempfile::tempdir().unwrap();
        let dir_path = dir.into_path();
        let skill = dir_path.join(".skill_mcp");
        std::fs::create_dir_all(&skill).unwrap();
        ToolContext {
            working_dir: dir_path.to_string_lossy().to_string(),
            assets_dir: String::new(),
            script_dir: String::new(),
            lang: "en".into(),
            skill_mcp_dir: Some(skill.to_string_lossy().to_string()),
            harness_dir: None,
            session_id: String::new(),
        }
    }

    #[tokio::test]
    async fn test_refine_records_entry() {
        let c = ctx();
        let r = HarnessRefineTool
            .execute(
                serde_json::json!({
                    "kind": "memory",
                    "content": "use --locked for reproducible builds",
                    "evidence": "cargo build failed twice without lockfile sync"
                }),
                &c,
            )
            .await
            .unwrap();
        assert_eq!(r.data["status"], "ok");
        let state = oz_core::harness::HarnessState::load(&harness_dir(&c));
        assert_eq!(state.entries.len(), 1);
        assert_eq!(state.entries[0].content, "use --locked for reproducible builds");
    }

    #[tokio::test]
    async fn test_refine_rejects_missing_evidence() {
        let c = ctx();
        let r = HarnessRefineTool
            .execute(
                serde_json::json!({
                    "kind": "memory",
                    "content": "some note",
                    "evidence": ""
                }),
                &c,
            )
            .await
            .unwrap();
        assert_eq!(r.data["status"], "error");
        assert!(r.data["error"].as_str().unwrap_or("").contains("evidence"));
    }

    #[tokio::test]
    async fn test_refine_rejects_bad_kind() {
        let c = ctx();
        let r = HarnessRefineTool
            .execute(
                serde_json::json!({
                    "kind": "bogus",
                    "content": "x",
                    "evidence": "y"
                }),
                &c,
            )
            .await;
        assert!(r.is_err());
    }

    #[tokio::test]
    async fn test_explicit_harness_dir_wins() {
        let explicit = tempfile::tempdir().unwrap();
        let explicit_path = explicit.path().to_string_lossy().to_string();
        let mut c = ctx();
        c.harness_dir = Some(explicit_path.clone());
        let r = HarnessRefineTool
            .execute(
                serde_json::json!({
                    "kind": "memory",
                    "content": "lesson for explicit dir",
                    "evidence": "seen once"
                }),
                &c,
            )
            .await
            .unwrap();
        assert_eq!(r.data["status"], "ok");
        // The ledger must land in the explicit dir, not under skill_mcp_dir.
        let state = oz_core::harness::HarnessState::load(std::path::Path::new(&explicit_path));
        assert_eq!(state.entries.len(), 1);
        assert_eq!(state.entries[0].content, "lesson for explicit dir");
        let old_location = std::path::PathBuf::from(c.skill_mcp_dir.unwrap()).join("harness");
        let fallback = oz_core::harness::HarnessState::load(&old_location);
        assert!(fallback.entries.is_empty(), "explicit dir must win over fallback");
    }
}
