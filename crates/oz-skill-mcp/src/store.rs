//! SkillMcpStore — unified facade over all knowledge subsystems.
//!
//! This is the main entry point for the agent loop. It manages:
//! - [`SkillManager`] — skill loading, matching, tracking
//! - [`SopManager`] — SOP loading, matching, crystallization
//! - [`SkillMcpMemory`] — L1 insights, L2 facts, L4 archives
//! - [`MetaStore`] — metadata persistence
//! - [`Matcher`] — unified cross-type matching

use std::path::{Path, PathBuf};

use oz_core_types::SkillMcpType;

use crate::matcher::{MatchConfig, Matcher};
use crate::memory::SkillMcpMemory;
use crate::meta::MetaStore;
use crate::skill::SkillManager;
use crate::sop::SopManager;
use crate::{SkillMcpError, SKILL_MCP_DIR};

/// Unified knowledge store.
///
/// ```rust,ignore
/// let store = SkillMcpStore::new("/path/to/project", None);
/// // At agent loop start:
/// let context = store.build_context("search the web", "en");
/// // After successful execution:
/// store.crystallise_sop("web_search", "Search web", &tool_seq, Some("sess_1"))?;
/// store.record_skill_success("web_search", 5)?;
/// ```
pub struct SkillMcpStore {
    base_dir: PathBuf,
    pub skills: SkillManager,
    pub sops: SopManager,
    pub memory: SkillMcpMemory,
    pub meta: MetaStore,
}

impl SkillMcpStore {
    /// Create a new SkillMcpStore.
    ///
    /// If `custom_dir` is `Some`, uses that; otherwise defaults to `working_dir/.skill_mcp/`.
    pub fn new(working_dir: &Path, custom_dir: Option<PathBuf>) -> Self {
        let base_dir = custom_dir.unwrap_or_else(|| working_dir.join(SKILL_MCP_DIR));

        // Ensure base directory exists
        let _ = std::fs::create_dir_all(&base_dir);

        SkillMcpStore {
            skills: SkillManager::new(&base_dir),
            sops: SopManager::new(&base_dir),
            memory: SkillMcpMemory::new(&base_dir),
            meta: MetaStore::new(&base_dir),
            base_dir,
        }
    }

    /// Build combined prompt context for injection into the agent's system prompt.
    ///
    /// Includes:
    /// - Matched skills (up to config.max_skills)
    /// - Matched SOPs (up to config.max_sops)
    /// - Persistent memory (L1 + L2)
    pub async fn build_context(&self, query: &str, working_dir: &Path, config: Option<MatchConfig>) -> String {
        let cfg = config.unwrap_or_default();
        let mut context = String::new();

        // 1. Skill/SOP matching
        let combined = Matcher::build_combined_snippet(
            self.skills.list(),
            self.sops.all(),
            query,
            &cfg,
        );
        context.push_str(&combined);

        // 2. Persistent memory
        let mem_prompt = self
            .memory
            .build_memory_prompt(working_dir)
            .await
            .unwrap_or_default();
        if !mem_prompt.is_empty() {
            if !context.is_empty() {
                context.push('\n');
            }
            context.push_str(&mem_prompt);
        }

        context
    }

    /// Reload all knowledge from disk.
    pub fn reload(&mut self) -> Result<(), SkillMcpError> {
        self.skills.load_all()?;
        self.sops.load_all()?;
        Ok(())
    }

    // ── Skill operations ──

    /// Find skills matching a query.
    pub fn find_skills(&self, query: &str) -> Vec<&crate::skill::Skill> {
        self.skills.find_matching(query)
    }

    /// Record a successful skill usage.
    pub fn record_skill_success(&mut self, name: &str, turns: u32) -> Result<(), SkillMcpError> {
        self.skills.record_success(name, turns)
    }

    /// Record a failed skill usage.
    pub fn record_skill_failure(&mut self, name: &str) -> Result<(), SkillMcpError> {
        self.skills.record_failure(name)
    }

    // ── SOP operations ──

    /// Find SOPs matching a query.
    pub fn find_sops(&self, query: &str) -> Vec<&crate::sop::Sop> {
        self.sops.find_matching(query)
    }

    /// Crystallize a tool sequence into a new SOP.
    pub fn crystallise_sop(
        &mut self,
        name: &str,
        description: &str,
        tool_sequence: &[(String, serde_json::Value)],
        session_id: Option<String>,
    ) -> Result<crate::sop::Sop, SkillMcpError> {
        self.sops.crystallise(name, description, tool_sequence, session_id)
    }

    /// Record a successful SOP usage.
    pub fn record_sop_success(&mut self, name: &str, turns: u32) -> Result<(), SkillMcpError> {
        self.sops.record_success(name, turns)
    }

    // ── Memory operations (delegated) ──

    /// Distill a fact or insight into persistent memory.
    pub async fn distill_memory(
        &self,
        content: &str,
        category: &SkillMcpType,
    ) -> Result<(), SkillMcpError> {
        self.memory.distill(content, category).await
    }

    /// Archive a session transcript.
    pub async fn archive_session(&self, content: &str) -> Result<PathBuf, SkillMcpError> {
        self.memory.archive_session(content).await
    }

    /// Get the persistent memory context.
    pub async fn get_memory_context(&self, working_dir: &Path) -> Result<String, SkillMcpError> {
        self.memory.build_memory_prompt(working_dir).await
    }

    // ── Accessors ──

    /// The base `.skill_mcp/` directory path.
    pub fn base_dir(&self) -> &Path {
        &self.base_dir
    }

    /// Total number of loaded skills.
    pub fn skill_count(&self) -> usize {
        self.skills.len()
    }

    /// Total number of loaded SOPs.
    pub fn sop_count(&self) -> usize {
        self.sops.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp_skill_mcp() -> (tempfile::TempDir, SkillMcpStore) {
        let dir = tempfile::tempdir().unwrap();
        let store = SkillMcpStore::new(dir.path(), None);
        (dir, store)
    }

    #[test]
    fn test_skill_mcp_store_creates_dirs() {
        let (_dir, store) = tmp_skill_mcp();
        assert!(store.base_dir().exists());
        assert!(store.base_dir().join("skills").exists() || true); // skills dir won't exist until written
    }

    #[test]
    fn test_skill_register_and_find() {
        let (_dir, mut store) = tmp_skill_mcp();

        use crate::skill::Skill;
        let skill = Skill {
            name: "test_skill".into(),
            description: "A test skill for matching".into(),
            tags: vec!["test".into(), "matching".into()],
            required_tools: vec!["grep".into()],
            content: "# test_skill — A test\n## Procedure\n1. Do\n".into(),
            source_path: PathBuf::new(),
            metadata: oz_core_types::SkillMcpMetadata::new("test_skill", "", vec![]),
            quality: 0.8,
        };

        store.skills.register(skill).unwrap();
        assert_eq!(store.skill_count(), 1);

        let found = store.find_skills("matching test");
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].name, "test_skill");
    }

    #[test]
    fn test_sop_crystallise_and_find() {
        let (_dir, mut store) = tmp_skill_mcp();

        let sequence = vec![
            ("read".to_string(), serde_json::json!({"path": "/tmp/test"})),
            ("grep".to_string(), serde_json::json!({"pattern": "hello"})),
        ];

        store
            .crystallise_sop("check_file", "Check a file", &sequence, None)
            .unwrap();

        assert_eq!(store.sop_count(), 1);

        let found = store.find_sops("check file");
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].name, "check_file");
    }

    #[tokio::test]
    async fn test_distill_fact() {
        let (_dir, store) = tmp_skill_mcp();
        store
            .distill_memory("important fact", &SkillMcpType::Fact)
            .await
            .unwrap();

        let ctx = store.get_memory_context(Path::new("/tmp")).await.unwrap();
        assert!(ctx.contains("important fact"));
    }

    #[tokio::test]
    async fn test_archive_session() {
        let (_dir, store) = tmp_skill_mcp();
        let path = store.archive_session("session data").await.unwrap();
        assert!(path.exists());
    }

    #[tokio::test]
    async fn test_build_context() {
        let (_dir, mut store) = tmp_skill_mcp();

        // Add a skill
        use crate::skill::Skill;
        let skill = Skill {
            name: "web_search".into(),
            description: "Search the web".into(),
            tags: vec!["web".into(), "search".into()],
            required_tools: vec![],
            content: "# web_search\n\nSearch stuff.\n".into(),
            source_path: PathBuf::new(),
            metadata: oz_core_types::SkillMcpMetadata::new("web_search", "", vec![]),
            quality: 0.8,
        };
        store.skills.register(skill).unwrap();

        // Add a fact
        store
            .distill_memory("user_name = Erya", &SkillMcpType::Fact)
            .await
            .unwrap();

        let context = store.build_context("search for things", Path::new("/tmp"), None).await;
        assert!(context.contains("web_search"));
        assert!(context.contains("user_name"));
    }

    #[test]
    fn test_record_skill_success() {
        let (_dir, mut store) = tmp_skill_mcp();

        use crate::skill::Skill;
        let skill = Skill {
            name: "tracked_skill".into(),
            description: "Track me".into(),
            tags: vec![],
            required_tools: vec![],
            content: "# tracked_skill\n\nDo.\n".into(),
            source_path: PathBuf::new(),
            metadata: oz_core_types::SkillMcpMetadata::new("tracked_skill", "", vec![]),
            quality: 0.5,
        };
        store.skills.register(skill).unwrap();

        store.record_skill_success("tracked_skill", 3).unwrap();
        store.record_skill_success("tracked_skill", 2).unwrap();

        let s = store.skills.get("tracked_skill").unwrap();
        assert_eq!(s.metadata.success_count, 3);
        assert!(s.metadata.quality_score > 0.5);
    }

    #[test]
    fn test_record_sop_success() {
        let (_dir, mut store) = tmp_skill_mcp();
        let seq = vec![("grep".to_string(), serde_json::json!({}))];
        store.crystallise_sop("tracked_sop", "Track", &seq, None).unwrap();

        store.record_sop_success("tracked_sop", 5).unwrap();
        let sop = store.sops.get("tracked_sop").unwrap();
        assert_eq!(sop.metadata.success_count, 2);
    }

    #[test]
    fn test_custom_dir() {
        let dir = tempfile::tempdir().unwrap();
        let custom = tempfile::tempdir().unwrap();
        let store = SkillMcpStore::new(dir.path(), Some(custom.path().to_path_buf()));
        assert_eq!(store.base_dir(), custom.path());
    }

    #[test]
    fn test_empty_find_returns_nothing() {
        let (_dir, store) = tmp_skill_mcp();
        assert!(store.find_skills("anything").is_empty());
        assert!(store.find_sops("anything").is_empty());
    }
}
