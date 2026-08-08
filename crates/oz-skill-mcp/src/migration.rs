//! On-disk migration utilities for the skill/MCP registry.
//!
//! Two chained migrations:
//!
//! 1. [`migrate_memory_to_skill_mcp`] — converts the legacy `memory/` layout
//!    (from the pre-`ga-knowledge` `ga-memory` crate) into the new
//!    `.skill_mcp/` directory tree.
//! 2. [`migrate_knowledge_to_skill_mcp`] — moves an existing `.knowledge/`
//!    directory (the previous `ga-knowledge` default) to the new
//!    `.skill_mcp/` location, preserving all contents.
//!
//! [`run_all_migrations`] chains both steps idempotently. Either old layout
//! is detected on startup and migrated forward.

use std::path::Path;

use crate::{SkillMcpError, SKILL_MCP_DIR};

const LEGACY_MEMORY_DIR: &str = "memory";
const PREVIOUS_KNOWLEDGE_DIR: &str = ".knowledge";

#[derive(Debug, Clone, Default)]
pub struct MigrationReport {
    pub memory_migration: Option<MemoryMigrationReport>,
    pub knowledge_renamed: bool,
    pub errors: Vec<String>,
}

impl MigrationReport {
    pub fn total_migrated(&self) -> usize {
        let mut count = 0;
        if let Some(ref m) = self.memory_migration {
            count += m.total_migrated();
        }
        if self.knowledge_renamed {
            count += 1;
        }
        count
    }

    pub fn has_errors(&self) -> bool {
        !self.errors.is_empty()
    }
}

#[derive(Debug, Clone)]
pub struct MemoryMigrationReport {
    pub facts_migrated: bool,
    pub insights_migrated: bool,
    pub sops_migrated: usize,
    pub sessions_migrated: usize,
    pub errors: Vec<String>,
}

impl MemoryMigrationReport {
    pub fn total_migrated(&self) -> usize {
        let mut count = 0;
        if self.facts_migrated { count += 1; }
        if self.insights_migrated { count += 1; }
        count + self.sops_migrated + self.sessions_migrated
    }

    pub fn has_errors(&self) -> bool {
        !self.errors.is_empty()
    }
}

pub fn migrate_memory_to_skill_mcp(working_dir: &Path) -> Result<MemoryMigrationReport, SkillMcpError> {
    let memory_dir = working_dir.join(LEGACY_MEMORY_DIR);
    let skill_mcp_dir = working_dir.join(SKILL_MCP_DIR);
    let mut report = MemoryMigrationReport {
        facts_migrated: false,
        insights_migrated: false,
        sops_migrated: 0,
        sessions_migrated: 0,
        errors: Vec::new(),
    };

    if !memory_dir.exists() {
        return Ok(report);
    }

    std::fs::create_dir_all(&skill_mcp_dir).ok();

    let old_facts = memory_dir.join("global_mem.txt");
    if old_facts.exists() {
        let new_facts = skill_mcp_dir.join("facts").join("global_mem.txt");
        if !new_facts.exists() {
            std::fs::create_dir_all(new_facts.parent().unwrap()).ok();
            match std::fs::copy(&old_facts, &new_facts) {
                Ok(_) => report.facts_migrated = true,
                Err(e) => report.errors.push(format!("facts: {}", e)),
            }
        }
    }

    let old_insights = memory_dir.join("global_mem_insight.txt");
    if old_insights.exists() {
        let new_insights = skill_mcp_dir.join("insights").join("global_mem_insight.txt");
        if !new_insights.exists() {
            std::fs::create_dir_all(new_insights.parent().unwrap()).ok();
            match std::fs::copy(&old_insights, &new_insights) {
                Ok(_) => report.insights_migrated = true,
                Err(e) => report.errors.push(format!("insights: {}", e)),
            }
        }
    }

    let sops_dir = skill_mcp_dir.join("sops");
    std::fs::create_dir_all(&sops_dir).ok();
    if let Ok(entries) = std::fs::read_dir(&memory_dir) {
        for entry in entries.flatten() {
            let path = entry.path();
            let fname = path.file_name().and_then(|n| n.to_str()).unwrap_or("");
            if fname.ends_with("_sop.md")
                || (fname.ends_with(".md")
                    && fname != "global_mem.txt"
                    && fname != "global_mem_insight.txt")
            {
                let dest_name = if fname.ends_with("_sop.md") {
                    fname.replace("_sop.md", ".md")
                } else {
                    fname.to_string()
                };
                let dest = sops_dir.join(&dest_name);
                if !dest.exists() {
                    match std::fs::copy(&path, &dest) {
                        Ok(_) => report.sops_migrated += 1,
                        Err(e) => report.errors.push(format!("sop {}: {}", fname, e)),
                    }
                }
            }
        }
    }

    let old_sessions = memory_dir.join("L4_raw_sessions");
    if old_sessions.exists() && old_sessions.is_dir() {
        let new_sessions = skill_mcp_dir.join("sessions");
        std::fs::create_dir_all(&new_sessions).ok();
        if let Ok(entries) = std::fs::read_dir(&old_sessions) {
            for entry in entries.flatten() {
                let path = entry.path();
                if path.is_file() {
                    let fname = path.file_name().and_then(|n| n.to_str()).unwrap_or("unknown");
                    let dest = new_sessions.join(fname);
                    if !dest.exists() {
                        match std::fs::copy(&path, &dest) {
                            Ok(_) => report.sessions_migrated += 1,
                            Err(e) => report.errors.push(format!("session {}: {}", fname, e)),
                        }
                    }
                }
            }
        }
    }

    Ok(report)
}

pub fn migrate_knowledge_to_skill_mcp(working_dir: &Path) -> Result<bool, SkillMcpError> {
    let old = working_dir.join(PREVIOUS_KNOWLEDGE_DIR);
    let new = working_dir.join(SKILL_MCP_DIR);

    if !old.exists() {
        return Ok(false);
    }

    if new.exists() {
        merge_dir_into(&old, &new)?;
        let _ = std::fs::remove_dir_all(&old);
        return Ok(true);
    }

    match std::fs::rename(&old, &new) {
        Ok(_) => Ok(true),
        Err(_) => {
            std::fs::create_dir_all(&new)?;
            merge_dir_into(&old, &new)?;
            let _ = std::fs::remove_dir_all(&old);
            Ok(true)
        }
    }
}

fn merge_dir_into(src: &Path, dst: &Path) -> Result<(), SkillMcpError> {
    std::fs::create_dir_all(dst)?;
    for entry in std::fs::read_dir(src)?.flatten() {
        let from = entry.path();
        let to = dst.join(entry.file_name());
        if from.is_dir() {
            merge_dir_into(&from, &to)?;
        } else if !to.exists() {
            if let Some(parent) = to.parent() {
                std::fs::create_dir_all(parent).ok();
            }
            std::fs::copy(&from, &to)?;
        }
    }
    Ok(())
}

pub fn run_all_migrations(working_dir: &Path) -> Result<MigrationReport, SkillMcpError> {
    let mut report = MigrationReport::default();

    match migrate_memory_to_skill_mcp(working_dir) {
        Ok(m) => {
            if m.total_migrated() > 0 {
                report.memory_migration = Some(m);
            }
        }
        Err(e) => report.errors.push(format!("memory migration: {}", e)),
    }

    match migrate_knowledge_to_skill_mcp(working_dir) {
        Ok(renamed) => report.knowledge_renamed = renamed,
        Err(e) => report.errors.push(format!("knowledge rename: {}", e)),
    }

    Ok(report)
}

pub fn is_migrated(working_dir: &Path) -> bool {
    let skill_mcp_dir = working_dir.join(SKILL_MCP_DIR);
    skill_mcp_dir.join("facts").exists()
        || skill_mcp_dir.join("sops").exists()
        || skill_mcp_dir.join("insights").exists()
        || skill_mcp_dir.join("skills").exists()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn setup_old_memory(dir: &Path) {
        let mem = dir.join(LEGACY_MEMORY_DIR);
        std::fs::create_dir_all(&mem).unwrap();
        std::fs::write(mem.join("global_mem.txt"), "fact content\n").unwrap();
        std::fs::write(mem.join("global_mem_insight.txt"), "insight content\n").unwrap();
        std::fs::write(mem.join("test_sop.md"), "# Test SOP\n\nContent.\n").unwrap();
        std::fs::write(mem.join("check_hosts_sop.md"), "# Check Hosts\n\nSteps.\n").unwrap();

        let l4 = mem.join("L4_raw_sessions");
        std::fs::create_dir_all(&l4).unwrap();
        std::fs::write(l4.join("session_001.md"), "session 1\n").unwrap();
        std::fs::write(l4.join("session_002.md"), "session 2\n").unwrap();
    }

    fn setup_old_knowledge(dir: &Path) {
        let k = dir.join(PREVIOUS_KNOWLEDGE_DIR);
        std::fs::create_dir_all(k.join("facts")).unwrap();
        std::fs::create_dir_all(k.join("sops")).unwrap();
        std::fs::write(k.join("facts/global_mem.txt"), "old fact\n").unwrap();
        std::fs::write(k.join("sops/test_sop.md"), "# Old SOP\n").unwrap();
    }

    #[test]
    fn test_migrate_memory_full() {
        let dir = tempfile::tempdir().unwrap();
        setup_old_memory(dir.path());

        let report = migrate_memory_to_skill_mcp(dir.path()).unwrap();
        assert!(report.facts_migrated);
        assert!(report.insights_migrated);
        assert_eq!(report.sops_migrated, 2);
        assert_eq!(report.sessions_migrated, 2);
        assert_eq!(report.total_migrated(), 6);
        assert!(!report.has_errors());

        let k = dir.path().join(SKILL_MCP_DIR);
        assert!(k.join("facts/global_mem.txt").exists());
        assert!(k.join("insights/global_mem_insight.txt").exists());
        assert!(k.join("sops/test.md").exists());
        assert!(k.join("sops/check_hosts.md").exists());
        assert!(k.join("sessions/session_001.md").exists());
        assert!(k.join("sessions/session_002.md").exists());
    }

    #[test]
    fn test_migrate_memory_no_memory_dir() {
        let dir = tempfile::tempdir().unwrap();
        let report = migrate_memory_to_skill_mcp(dir.path()).unwrap();
        assert_eq!(report.total_migrated(), 0);
    }

    #[test]
    fn test_rename_knowledge_only() {
        let dir = tempfile::tempdir().unwrap();
        setup_old_knowledge(dir.path());

        let renamed = migrate_knowledge_to_skill_mcp(dir.path()).unwrap();
        assert!(renamed);

        let k = dir.path().join(SKILL_MCP_DIR);
        assert!(k.join("facts/global_mem.txt").exists());
        assert!(k.join("sops/test_sop.md").exists());
        assert!(!dir.path().join(PREVIOUS_KNOWLEDGE_DIR).exists());
    }

    #[test]
    fn test_rename_knowledge_merge_when_new_exists() {
        let dir = tempfile::tempdir().unwrap();
        setup_old_knowledge(dir.path());

        let new = dir.path().join(SKILL_MCP_DIR);
        std::fs::create_dir_all(new.join("facts")).unwrap();
        std::fs::write(new.join("facts/global_mem.txt"), "newer fact\n").unwrap();
        std::fs::create_dir_all(new.join("skills")).unwrap();

        let renamed = migrate_knowledge_to_skill_mcp(dir.path()).unwrap();
        assert!(renamed);

        assert_eq!(
            std::fs::read_to_string(new.join("facts/global_mem.txt")).unwrap(),
            "newer fact\n"
        );
        assert!(new.join("sops/test_sop.md").exists());
        assert!(new.join("skills").exists());
    }

    #[test]
    fn test_rename_knowledge_no_old() {
        let dir = tempfile::tempdir().unwrap();
        let renamed = migrate_knowledge_to_skill_mcp(dir.path()).unwrap();
        assert!(!renamed);
    }

    #[test]
    fn test_run_all_migrations_idempotent() {
        let dir = tempfile::tempdir().unwrap();
        setup_old_memory(dir.path());
        setup_old_knowledge(dir.path());

        let r1 = run_all_migrations(dir.path()).unwrap();
        assert!(r1.memory_migration.is_some());
        assert!(r1.knowledge_renamed);
        assert!(!r1.has_errors());

        let r2 = run_all_migrations(dir.path()).unwrap();
        assert!(r2.memory_migration.is_none());
        assert!(!r2.knowledge_renamed);
    }

    #[test]
    fn test_is_migrated() {
        let dir = tempfile::tempdir().unwrap();
        assert!(!is_migrated(dir.path()));

        let k = dir.path().join(SKILL_MCP_DIR);
        std::fs::create_dir_all(k.join("sops")).unwrap();
        assert!(is_migrated(dir.path()));
    }
}
