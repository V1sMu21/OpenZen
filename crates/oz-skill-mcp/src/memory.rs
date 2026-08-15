//! Persistent memory — manages L1 (insights), L2 (facts), and L4 (session archives).
//!
//! Migrated from `ga-memory` crate. Uses the `.skill_mcp/` directory layout.
//! L3 (SOPs) are managed by `sop.rs` in this crate for unified access.

use std::path::{Path, PathBuf};

use oz_core_types::SkillMcpType;

use crate::SkillMcpError;

/// Manages persistent memory: insights (L1), facts (L2), session archives (L4).
///
/// L3 SOPs are managed by [`crate::SopManager`] but share the same `.skill_mcp/` root.
pub struct SkillMcpMemory {
    #[allow(dead_code)]
    base_dir: PathBuf,
    insight_path: PathBuf,
    fact_path: PathBuf,
    sessions_dir: PathBuf,
    auto_insight_dir: PathBuf,
    fts: Option<oz_memory::MemoryFts>,
}

impl SkillMcpMemory {
    /// Create a memory system rooted at `base_dir`.
    /// Expects `.skill_mcp/` to exist (or will create on first write).
    pub fn new(base_dir: &Path) -> Self {
        let insight_path = base_dir
            .join(SkillMcpType::Insight.as_dir())
            .join("global_mem_insight.txt");
        let fact_path = base_dir
            .join(SkillMcpType::Fact.as_dir())
            .join("global_mem.txt");
        let sessions_dir = base_dir.join("sessions");
        let auto_insight_dir = base_dir.join(SkillMcpType::Insight.as_dir()).join("auto");

        // P2-7: attach the FTS5 index (fail-open — memory still works without it).
        let fts = oz_memory::MemoryFts::open(&base_dir.join("memory_fts.sqlite")).ok();

        SkillMcpMemory {
            base_dir: base_dir.to_path_buf(),
            insight_path,
            fact_path,
            sessions_dir,
            auto_insight_dir,
            fts,
        }
    }

    /// Index a memory line in FTS5 (P2-7). Failures are logged, never fatal.
    fn index_line(&self, category: &str, line: &str) {
        if let Some(fts) = &self.fts {
            if let Err(e) = fts.insert("", category, line) {
                tracing::warn!("[memory_fts] index failed ({category}): {e}");
            }
        }
    }

    // ── L1: Insights ──

    /// Read the global insight file (L1 — distilled insights).
    pub async fn read_insight(&self) -> Result<String, SkillMcpError> {
        if !self.insight_path.exists() {
            return Ok(String::new());
        }
        Ok(tokio::fs::read_to_string(&self.insight_path).await?)
    }

    /// Write/replace the insight file.
    pub async fn write_insight(&self, content: &str) -> Result<(), SkillMcpError> {
        if let Some(parent) = self.insight_path.parent() {
            tokio::fs::create_dir_all(parent).await?;
        }
        tokio::fs::write(&self.insight_path, content).await?;
        Ok(())
    }

    /// Append to the insight file.
    pub async fn append_insight(&self, line: &str) -> Result<(), SkillMcpError> {
        let mut current = self.read_insight().await?;
        if !current.is_empty() && !current.ends_with('\n') {
            current.push('\n');
        }
        current.push_str(line);
        current.push('\n');
        self.write_insight(&current).await?;
        self.index_line("insight", line);
        Ok(())
    }

    // ── L1a: Auto insights (session-derived, weekly archived) ──

    /// Append an auto-derived experience line to this week's auto-insight
    /// file (`insights/auto/YYYY-Www.md`), deduplicated against existing
    /// content by session-topic prefix (`[topic] ...`). Auto insights are
    /// separate from manual insights so the manual
    /// `global_mem_insight.txt` stays curated.
    pub async fn append_auto_insight(&self, line: &str) -> Result<(), SkillMcpError> {
        let line = line.trim();
        if line.is_empty() {
            return Ok(());
        }
        let topic_prefix = line
            .split(']')
            .next()
            .map(|s| s.to_string())
            .unwrap_or_default();
        let week = chrono::Utc::now().format("%Y-W%V");
        let path = self.auto_insight_dir.join(format!("{week}.md"));
        let mut current = if path.exists() {
            tokio::fs::read_to_string(&path).await?
        } else {
            String::new()
        };
        let dedup_key = if topic_prefix.is_empty() || !line.starts_with('[') {
            line.to_string()
        } else {
            topic_prefix
        };
        let exists = current.lines().any(|l| l.contains(&dedup_key));
        if exists {
            return Ok(());
        }
        if !current.is_empty() && !current.ends_with('\n') {
            current.push('\n');
        }
        if current.is_empty() {
            current.push_str(&format!("# Auto Insights {week}\n\n"));
        }
        current.push_str(&format!("- {line}\n"));
        tokio::fs::create_dir_all(&self.auto_insight_dir).await?;
        tokio::fs::write(&path, current).await?;
        self.index_line("auto_insight", line);
        Ok(())
    }

    /// Read auto insights from the last `days` days (weekly files are whole
    /// weeks, so this returns any weekly file overlapping the window).
    pub async fn read_recent_auto_insights(&self, days: i64) -> Result<String, SkillMcpError> {
        self.read_recent_auto_insights_sync(days)
    }

    /// Sync core of [`Self::read_recent_auto_insights`] — used by the
    /// (sync) index builder so recent insights can ride along in the
    /// startup injection without an async boundary.
    pub fn read_recent_auto_insights_sync(&self, days: i64) -> Result<String, SkillMcpError> {
        let cutoff = (chrono::Utc::now() - chrono::Duration::days(days)).date_naive();
        if !self.auto_insight_dir.exists() {
            return Ok(String::new());
        }
        let mut out = String::new();
        let entries = std::fs::read_dir(&self.auto_insight_dir)?;
        for entry in entries.flatten() {
            let name = entry.file_name().to_string_lossy().to_string();
            if !name.ends_with(".md") {
                continue;
            }
            // "2026-W32.md" — ISO week; keep files whose week overlaps the window.
            if let Some(stem) = name.strip_suffix(".md") {
                if let Some(monday) = iso_week_monday(stem) {
                    if monday >= cutoff {
                        if let Ok(content) = std::fs::read_to_string(entry.path()) {
                            out.push_str(&content);
                            if !out.ends_with('\n') {
                                out.push('\n');
                            }
                        }
                    }
                }
            }
        }
        Ok(out)
    }

    // ── L2: Facts ──

    /// Read the global fact file (L2 — persistent global facts).
    pub async fn read_facts(&self) -> Result<String, SkillMcpError> {
        if !self.fact_path.exists() {
            return Ok(String::new());
        }
        Ok(tokio::fs::read_to_string(&self.fact_path).await?)
    }

    /// Write/replace the fact file.
    pub async fn write_facts(&self, content: &str) -> Result<(), SkillMcpError> {
        if let Some(parent) = self.fact_path.parent() {
            tokio::fs::create_dir_all(parent).await?;
        }
        tokio::fs::write(&self.fact_path, content).await?;
        Ok(())
    }

    /// Append a fact line (deduplicated — only if not already present).
    pub async fn append_fact(&self, fact: &str) -> Result<(), SkillMcpError> {
        let mut current = self.read_facts().await?;
        if !current.contains(fact) {
            if !current.is_empty() && !current.ends_with('\n') {
                current.push('\n');
            }
            current.push_str(&format!("- {fact}"));
            current.push('\n');
            self.write_facts(&current).await?;
            self.index_line("fact", fact);
        }
        Ok(())
    }

    // ── L4: Session Archives ──

    /// Archive a raw session transcript into L4.
    /// Returns the path of the archived file.
    pub async fn archive_session(&self, content: &str) -> Result<PathBuf, SkillMcpError> {
        tokio::fs::create_dir_all(&self.sessions_dir).await?;
        let ts = chrono::Utc::now().format("%Y%m%d_%H%M%S");
        let path = self.sessions_dir.join(format!("session_{ts}.md"));
        tokio::fs::write(&path, content).await?;
        Ok(path)
    }

    // ── Composite ──

    /// Build the full memory prompt (L1 + L2).
    /// Compatible with the old `get_global_memory()` output format.
    pub async fn build_memory_prompt(&self, working_dir: &Path) -> Result<String, SkillMcpError> {
        let mut prompt = format!("cwd = {}\n", working_dir.join("temp").display());

        let facts = self.read_facts().await?;
        if !facts.is_empty() {
            prompt.push_str("\n[Memory] (../.skill_mcp)\n");
            prompt.push_str("../.skill_mcp/facts/global_mem.txt:\n");
            prompt.push_str(&facts);
            prompt.push('\n');
        }

        let insight = self.read_insight().await?;
        if !insight.is_empty() {
            prompt.push_str("../.skill_mcp/insights/global_mem_insight.txt:\n");
            prompt.push_str(&insight);
        }

        Ok(prompt)
    }

    /// Distill discovered memory into the appropriate level.
    pub async fn distill(
        &self,
        content: &str,
        category: &SkillMcpType,
    ) -> Result<(), SkillMcpError> {
        match category {
            SkillMcpType::Fact => self.append_fact(content).await,
            SkillMcpType::Insight => self.append_insight(content).await,
            SkillMcpType::Sop => {
                // SOPs are handled by SopManager; here we store as a fallback
                Err(SkillMcpError::InvalidFormat(
                    "Use SopManager for SOPs; distill only handles Fact/Insight".into(),
                ))
            }
            SkillMcpType::Skill => Err(SkillMcpError::InvalidFormat(
                "Use SkillManager for Skills; distill only handles Fact/Insight".into(),
            )),
        }
    }
}

/// Monday of ISO week `Www` in `year`, from a `"2026-W32"`-style stem.
/// ISO 8601: week 1 contains Jan 4; its Monday is Jan 4 minus its weekday offset.
fn iso_week_monday(stem: &str) -> Option<chrono::NaiveDate> {
    let (year, week) = stem.split_once("-W")?;
    let year: i32 = year.parse().ok()?;
    let week: u32 = week.parse().ok()?;
    if !(1..=53).contains(&week) {
        return None;
    }
    let jan4 = chrono::NaiveDate::from_ymd_opt(year, 1, 4)?;
    let offset = chrono::Datelike::weekday(&jan4).num_days_from_monday() as i64;
    let week1_monday = jan4 - chrono::Duration::days(offset);
    Some(week1_monday + chrono::Duration::weeks(week as i64 - 1))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp_dir() -> tempfile::TempDir {
        tempfile::tempdir().unwrap()
    }

    #[tokio::test]
    async fn test_read_write_insight() {
        let dir = tmp_dir();
        let mem = SkillMcpMemory::new(dir.path());

        mem.write_insight("key insight").await.unwrap();
        assert_eq!(mem.read_insight().await.unwrap().trim(), "key insight");
    }

    #[tokio::test]
    async fn test_read_empty_insight() {
        let dir = tmp_dir();
        let mem = SkillMcpMemory::new(dir.path());
        assert!(mem.read_insight().await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn test_append_insight() {
        let dir = tmp_dir();
        let mem = SkillMcpMemory::new(dir.path());
        mem.append_insight("first").await.unwrap();
        mem.append_insight("second").await.unwrap();
        let content = mem.read_insight().await.unwrap();
        assert!(content.contains("first"));
        assert!(content.contains("second"));
    }

    #[tokio::test]
    async fn test_read_write_facts() {
        let dir = tmp_dir();
        let mem = SkillMcpMemory::new(dir.path());

        mem.append_fact("fact one").await.unwrap();
        mem.append_fact("fact two").await.unwrap();
        // Duplicate should be skipped
        mem.append_fact("fact one").await.unwrap();

        let facts = mem.read_facts().await.unwrap();
        assert!(facts.contains("fact one"));
        assert!(facts.contains("fact two"));
        // Count occurrences of "fact one"
        let count = facts.matches("fact one").count();
        assert_eq!(count, 1, "duplicate should be skipped");
    }

    #[tokio::test]
    async fn test_archive_session() {
        let dir = tmp_dir();
        let mem = SkillMcpMemory::new(dir.path());

        let path = mem.archive_session("session content").await.unwrap();
        assert!(path.exists());
        let content = tokio::fs::read_to_string(&path).await.unwrap();
        assert!(content.contains("session content"));
    }

    #[tokio::test]
    async fn test_build_memory_prompt() {
        let dir = tmp_dir();
        let mem = SkillMcpMemory::new(dir.path());

        mem.append_fact("user_name = Erya").await.unwrap();
        mem.write_insight("key insight").await.unwrap();

        let prompt = mem
            .build_memory_prompt(Path::new("/tmp/proj"))
            .await
            .unwrap();
        assert!(prompt.contains("user_name = Erya"));
        assert!(prompt.contains("key insight"));
    }

    #[tokio::test]
    async fn test_distill_fact() {
        let dir = tmp_dir();
        let mem = SkillMcpMemory::new(dir.path());
        mem.distill("hello world", &SkillMcpType::Fact)
            .await
            .unwrap();
        let facts = mem.read_facts().await.unwrap();
        assert!(facts.contains("hello world"));
    }

    #[tokio::test]
    async fn test_distill_sop_rejected() {
        let dir = tmp_dir();
        let mem = SkillMcpMemory::new(dir.path());
        let result = mem.distill("some sop", &SkillMcpType::Sop).await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn test_append_auto_insight_dedup() {
        let dir = tmp_dir();
        let mem = SkillMcpMemory::new(dir.path());
        mem.append_auto_insight("retry with backoff on 429")
            .await
            .unwrap();
        mem.append_auto_insight("retry with backoff on 429")
            .await
            .unwrap();
        mem.append_auto_insight("use CGEvent for WKWebView typing")
            .await
            .unwrap();

        let recent = mem.read_recent_auto_insights(7).await.unwrap();
        assert_eq!(recent.matches("retry with backoff on 429").count(), 1);
        assert!(recent.contains("use CGEvent for WKWebView typing"));
    }

    #[tokio::test]
    async fn test_read_auto_insights_empty_dir() {
        let dir = tmp_dir();
        let mem = SkillMcpMemory::new(dir.path());
        let recent = mem.read_recent_auto_insights(7).await.unwrap();
        assert!(recent.is_empty());
    }

    #[test]
    fn test_iso_week_monday() {
        // 2026-W32: week of Aug 9 2026 (Sunday) — Monday is Aug 3.
        assert_eq!(
            iso_week_monday("2026-W32"),
            Some(chrono::NaiveDate::from_ymd_opt(2026, 8, 3).unwrap())
        );
        // 2025-W01: Jan 1 2025 is a Wednesday → week 1 Monday is Dec 30 2024.
        assert_eq!(
            iso_week_monday("2025-W01"),
            Some(chrono::NaiveDate::from_ymd_opt(2024, 12, 30).unwrap())
        );
        assert_eq!(iso_week_monday("2026-W00"), None);
        assert_eq!(iso_week_monday("2026-W54"), None);
        assert_eq!(iso_week_monday("garbage"), None);
    }
}
