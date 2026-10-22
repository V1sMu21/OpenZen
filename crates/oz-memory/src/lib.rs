//! 4-level memory system for OpenZen.
//!
//! - **L1** — `global_mem_insight.txt` — distilled insights
//! - **L2** — `global_mem.txt` — persistent global facts
//! - **L3** — `memory/` directory — SOP documents
//! - **L4** — `memory/L4_raw_sessions/` — raw session history

use std::path::{Path, PathBuf};

pub mod fts;

pub use fts::{FtsHit, MemoryFts};

/// Error type for memory operations.
#[derive(Debug, thiserror::Error)]
pub enum MemoryError {
    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),
    #[error("Parse error: {0}")]
    Parse(String),
    #[error("Memory dir not found: {0}")]
    DirNotFound(PathBuf),
    #[error("{0}")]
    Other(String),
}

/// A single distilled memory item.
#[derive(Debug, Clone)]
pub struct MemoryItem {
    pub content: String,
    pub category: MemoryCategory,
    pub source: String,
}

#[derive(Debug, Clone, PartialEq)]
pub enum MemoryCategory {
    Fact,
    Sop,
    History,
}

/// 4-level memory system.
pub struct MemorySystem {
    base_dir: PathBuf,
    insight_path: PathBuf,
    global_path: PathBuf,
    memory_dir: PathBuf,
    l4_dir: PathBuf,
    _lang: String,
}

impl MemorySystem {
    /// Create a memory system rooted at `base_dir` (expects `memory/` subdirectory).
    pub fn new(base_dir: &Path, lang: &str) -> Self {
        let memory_dir = base_dir.join("memory");
        MemorySystem {
            base_dir: base_dir.to_path_buf(),
            insight_path: memory_dir.join("global_mem_insight.txt"),
            global_path: memory_dir.join("global_mem.txt"),
            memory_dir: memory_dir.clone(),
            l4_dir: memory_dir.join("L4_raw_sessions"),
            _lang: lang.to_string(),
        }
    }

    /// L1: Read the insight file.
    pub async fn read_insight(&self) -> Result<String, MemoryError> {
        if !self.insight_path.exists() {
            return Ok(String::new());
        }
        Ok(tokio::fs::read_to_string(&self.insight_path).await?)
    }

    /// L2: Read the global memory file.
    pub async fn read_global(&self) -> Result<String, MemoryError> {
        if !self.global_path.exists() {
            return Ok(String::new());
        }
        Ok(tokio::fs::read_to_string(&self.global_path).await?)
    }

    /// L3: List all SOP files in the memory directory.
    pub fn list_sops(&self) -> Result<Vec<PathBuf>, MemoryError> {
        if !self.memory_dir.exists() {
            return Ok(vec![]);
        }
        let mut sops = Vec::new();
        for entry in std::fs::read_dir(&self.memory_dir).map_err(MemoryError::Io)? {
            let entry = entry.map_err(MemoryError::Io)?;
            let path = entry.path();
            if path.extension().is_some_and(|e| e == "md" || e == "txt") {
                let fname = path.file_name().unwrap_or_default().to_string_lossy();
                if fname != "global_mem_insight.txt" && fname != "global_mem.txt" {
                    sops.push(path);
                }
            }
        }
        sops.sort();
        Ok(sops)
    }

    /// Build the full memory prompt string (L1 + L2).
    pub async fn get_global_memory(&self) -> Result<String, MemoryError> {
        let mut prompt = format!("cwd = {}\n", self.base_dir.display());

        let insight = self.read_insight().await?;
        let structure = self.read_insight_structure().await;
        prompt.push_str("\n[Memory] (../memory)\n");
        prompt.push_str(&structure);
        prompt.push('\n');
        prompt.push_str("../memory/global_mem_insight.txt:\n");
        prompt.push_str(&insight);

        Ok(prompt)
    }

    /// Read memory structure template.
    ///
    /// Sections that can no longer be trusted are withheld here (and logged):
    /// the real file's first section described the pre-rename `ga-*` crate
    /// layout, so every prompt carried a confidently wrong architecture
    /// description (round5 P1-11 ②).
    async fn read_insight_structure(&self) -> String {
        let global = self.read_global().await.unwrap_or_default();
        if global.is_empty() {
            return String::new();
        }
        let today = chrono::Utc::now().date_naive();
        let (kept, dropped) = filter_stale_fact_sections(&global, today);
        if !dropped.is_empty() {
            tracing::warn!(
                "[memory] withheld {} stale fact section(s) from the prompt: {}",
                dropped.len(),
                dropped.join("; ")
            );
        }
        if kept.trim().is_empty() {
            return String::new();
        }
        format!("../memory/global_mem.txt:\n{kept}\n")
    }

    /// Distill a discovered memory item into L2 or L3.
    pub async fn distill_memory(&self, items: &[MemoryItem]) -> Result<(), MemoryError> {
        for item in items {
            match item.category {
                MemoryCategory::Fact => {
                    let mut current = self.read_global().await?;
                    if !current.contains(&item.content) {
                        current.push_str(&format!("- {}\n", item.content));
                        tokio::fs::write(&self.global_path, &current).await?;
                    }
                }
                MemoryCategory::Sop => {
                    let name = sanitize_filename(&item.content);
                    let path = self.memory_dir.join(format!("{}_sop.md", name));
                    if !path.exists() {
                        let content = format!(
                            "# {}\n\n{}\n\n---\nSource: {}\n",
                            item.content, item.content, item.source
                        );
                        tokio::fs::write(&path, &content).await?;
                    }
                }
                MemoryCategory::History => {
                    tokio::fs::create_dir_all(&self.l4_dir).await?;
                    let ts = chrono::Utc::now().format("%Y%m%d_%H%M%S");
                    let path = self.l4_dir.join(format!("session_{ts}.md"));
                    let content = format!(
                        "# Session {ts}\n\nSource: {}\n\n{}\n",
                        item.source, item.content
                    );
                    tokio::fs::write(&path, &content).await?;
                }
            }
        }
        Ok(())
    }

    /// Write a raw session transcript to L4.
    pub async fn archive_session(&self, content: &str) -> Result<PathBuf, MemoryError> {
        tokio::fs::create_dir_all(&self.l4_dir).await?;
        let ts = chrono::Utc::now().format("%Y%m%d_%H%M%S");
        let path = self.l4_dir.join(format!("session_{ts}.md"));
        tokio::fs::write(&path, content).await?;
        Ok(path)
    }
}

fn sanitize_filename(s: &str) -> String {
    s.chars()
        .map(|c| {
            if c.is_alphanumeric() || c == '-' || c == '_' {
                c
            } else {
                '_'
            }
        })
        .collect::<String>()
        .trim_matches('_')
        .to_lowercase()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp_memory() -> (tempfile::TempDir, MemorySystem) {
        let dir = tempfile::tempdir().unwrap();
        let mem_dir = dir.path().join("memory");
        std::fs::create_dir_all(&mem_dir).unwrap();
        std::fs::write(mem_dir.join("global_mem_insight.txt"), "key insight").unwrap();
        std::fs::write(mem_dir.join("global_mem.txt"), "global fact").unwrap();
        std::fs::write(mem_dir.join("test_sop.md"), "# Test SOP").unwrap();
        let ms = MemorySystem::new(dir.path(), "en");
        (dir, ms)
    }

    #[tokio::test]
    async fn test_read_insight() {
        let (_dir, ms) = tmp_memory();
        let insight = ms.read_insight().await.unwrap();
        assert_eq!(insight.trim(), "key insight");
    }

    #[tokio::test]
    async fn test_read_global() {
        let (_dir, ms) = tmp_memory();
        let global = ms.read_global().await.unwrap();
        assert_eq!(global.trim(), "global fact");
    }

    #[test]
    fn test_list_sops() {
        let (_dir, ms) = tmp_memory();
        let sops = ms.list_sops().unwrap();
        assert!(sops.iter().any(|p| p.file_name().unwrap() == "test_sop.md"));
    }

    #[tokio::test]
    async fn test_get_global_memory() {
        let (_dir, ms) = tmp_memory();
        let prompt = ms.get_global_memory().await.unwrap();
        assert!(prompt.contains("key insight"));
    }

    #[tokio::test]
    async fn test_distill_fact() {
        let (_dir, ms) = tmp_memory();
        let items = vec![MemoryItem {
            content: "new fact".into(),
            category: MemoryCategory::Fact,
            source: "test".into(),
        }];
        ms.distill_memory(&items).await.unwrap();
        let global = ms.read_global().await.unwrap();
        assert!(global.contains("new fact"));
    }

    #[tokio::test]
    async fn test_distill_sop() {
        let (_dir, ms) = tmp_memory();
        let items = vec![MemoryItem {
            content: "My Cool Procedure".into(),
            category: MemoryCategory::Sop,
            source: "test".into(),
        }];
        ms.distill_memory(&items).await.unwrap();
        let sops = ms.list_sops().unwrap();
        assert!(sops
            .iter()
            .any(|p| p.to_string_lossy().contains("my_cool_procedure")));
    }

    #[tokio::test]
    async fn test_archive_session() {
        let (_dir, ms) = tmp_memory();
        let path = ms.archive_session("session content").await.unwrap();
        assert!(path.exists());
        let content = tokio::fs::read_to_string(&path).await.unwrap();
        assert!(content.contains("session content"));
    }

    #[test]
    fn test_sanitize_filename() {
        assert_eq!(sanitize_filename("Hello World!"), "hello_world");
        assert_eq!(sanitize_filename("test-123"), "test-123");
        assert_eq!(sanitize_filename(""), "");
    }

    #[tokio::test]
    async fn test_read_missing_insight() {
        let dir = tempfile::tempdir().unwrap();
        let ms = MemorySystem::new(dir.path(), "en");
        let insight = ms.read_insight().await.unwrap();
        assert!(insight.is_empty());
    }
}

// ── Fact freshness (round5 P1-11 ②) ──────────────────────────────────────
//
// `global_mem.txt` is injected into every system prompt. On the author's
// machine its first section was `## OpenZen Crate 架构清单 (2026-06-13)`,
// listing `ga-agent` / `ga-core` / `ga-memory` — crates that no longer exist
// after the `ga-*` → `oz-*` rename. The agent was fed a confidently wrong
// architecture description every single turn: not "no memory" but "wrong
// memory, treated as fact".

/// Crate-name prefixes that a section may reference.
const LEGACY_CRATE_PREFIX: &str = "ga-";

/// How old a dated section may be before it needs re-verification.
const FACT_STALE_DAYS: i64 = 120;

/// Extract `key: value` or `(YYYY-MM-DD)` verification dates from a section.
fn section_verified_date(section: &str) -> Option<chrono::NaiveDate> {
    for marker in ["verified_at:", "verified_at =", "verified:"] {
        if let Some(pos) = section.find(marker) {
            let rest = &section[pos + marker.len()..];
            let token: String = rest
                .trim_start()
                .chars()
                .take_while(|c| c.is_ascii_digit() || *c == '-')
                .collect();
            if let Ok(d) = chrono::NaiveDate::parse_from_str(token.trim(), "%Y-%m-%d") {
                return Some(d);
            }
        }
    }
    // Titles commonly carry the date they were written: `## X (2026-06-13)`.
    let bytes = section.as_bytes();
    let mut i = 0;
    // "(YYYY-MM-DD)" is 11 bytes; the date itself is [i+1, i+11).
    while i + 11 <= bytes.len() {
        if bytes[i] == b'('
            && bytes[i + 5] == b'-'
            && bytes[i + 8] == b'-'
            && bytes[i + 1..i + 5].iter().all(|b| b.is_ascii_digit())
        {
            let token = &section[i + 1..i + 11];
            if let Ok(d) = chrono::NaiveDate::parse_from_str(token, "%Y-%m-%d") {
                return Some(d);
            }
        }
        i += 1;
    }
    None
}

/// Split a facts document into `## ` sections (the preamble is section 0).
fn split_sections(text: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    let mut current = String::new();
    for line in text.lines() {
        if is_section_heading(line) && !current.trim().is_empty() {
            out.push(std::mem::take(&mut current));
        }
        current.push_str(line);
        current.push('\n');
    }
    if !current.trim().is_empty() {
        out.push(current);
    }
    out
}

/// Is this line a section heading?
///
/// `## Title` is the documented shape, but the real legacy file on disk starts
/// with `- ## OpenZen Crate 架构清单 (2026-06-13)` — a markdown LIST ITEM whose
/// text is a heading. Matching only `## ` made that whole file look like one
/// unstructured blob, so it could never be quarantined (and the safety rule
/// below then correctly refused to touch it).
fn is_section_heading(line: &str) -> bool {
    let t = line.trim_start();
    let t = t
        .strip_prefix("- ")
        .or_else(|| t.strip_prefix("* "))
        .unwrap_or(t);
    t.starts_with("## ")
}

/// Keep only the fact sections that can still be trusted.
///
/// A section is dropped when either:
///  * it references only `ga-*` crates (the pre-rename names — nothing in the
///    current tree matches them), or
///  * it carries a date older than [`FACT_STALE_DAYS`] and no fresher
///    `verified_at:` marker.
///
/// Returns `(kept_text, dropped_titles)` so the caller can log what was
/// withheld instead of silently shrinking the prompt.
pub fn filter_stale_fact_sections(text: &str, today: chrono::NaiveDate) -> (String, Vec<String>) {
    let mut kept = String::new();
    let mut dropped: Vec<String> = Vec::new();
    for section in split_sections(text) {
        let title = section_title(&section);
        if is_stale_section_text(&section, today) {
            dropped.push(title);
        } else {
            kept.push_str(&section);
        }
    }
    (kept, dropped)
}

/// Title of a section (its `## ` header, or `(preamble)`).
fn section_title(section: &str) -> String {
    section
        .lines()
        .find(|l| is_section_heading(l))
        .map(|l| {
            let t = l.trim_start();
            let t = t
                .strip_prefix("- ")
                .or_else(|| t.strip_prefix("* "))
                .unwrap_or(t);
            t.trim_start_matches("## ").trim().to_string()
        })
        .unwrap_or_else(|| "(preamble)".to_string())
}

/// The unambiguous legacy shape: it talks about `ga-*` crates and mentions
/// none of the current names. Deterministic, unlike the date heuristic.
fn is_legacy_crate_list(section: &str) -> bool {
    section.contains(LEGACY_CRATE_PREFIX)
        && !section.contains("oz-")
        && !section.contains("openzen")
}

/// Is this section untrustworthy? One judgment, shared by the prompt filter
/// and the quarantine splitter so they can never disagree.
fn is_stale_section_text(section: &str, today: chrono::NaiveDate) -> bool {
    let is_legacy = is_legacy_crate_list(section);
    let too_old = section_verified_date(section)
        .map(|d| (today - d).num_days() > FACT_STALE_DAYS)
        .unwrap_or(false);
    is_legacy || too_old
}

/// Split `text` into (kept, dropped) as raw text, preserving every byte of
/// the dropped sections so they can be archived verbatim.
///
/// `filter_stale_fact_sections` returns only the dropped *titles* (it feeds a
/// prompt); this one keeps the bodies. A file with no `## ` headers is treated
/// as a single section and is never dropped — there is no structure to judge,
/// and losing a header-less file wholesale is how 2564 bytes disappeared.
pub fn split_stale_sections(text: &str, today: chrono::NaiveDate) -> (String, String) {
    let sections = split_sections(text);
    if sections.len() <= 1 {
        // One section (or none) = no structure to judge, so leave it alone…
        // …unless that single section is the DETERMINISTIC legacy shape
        // (only `ga-*`, no current names). The real
        // `workspace/facts/global_mem.txt` is exactly that: 41 lines, one
        // `- ## …` heading, 19 `ga-` references — a whole stale file that the
        // structure rule was protecting from a cleanup it actually needed.
        // Archiving it verbatim is lossless.
        if let Some(only) = sections.first() {
            if is_legacy_crate_list(only) {
                return (String::new(), only.clone());
            }
        }
        return (text.to_string(), String::new());
    }
    let mut kept = String::new();
    let mut dropped = String::new();
    for section in sections {
        if is_stale_section_text(&section, today) {
            dropped.push_str(&section);
        } else {
            kept.push_str(&section);
        }
    }
    (kept, dropped)
}

/// Move stale sections out of `global_mem.txt` into `global_mem.stale.txt`.
///
/// The read path only *filters* (no side effects); this is the one-time
/// maintenance action that actually cleans the file. The stale text is kept
/// on disk so nothing the user wrote is lost.
pub fn quarantine_stale_fact_sections(
    global_mem_path: &Path,
    today: chrono::NaiveDate,
) -> Result<usize, MemoryError> {
    let text = match std::fs::read_to_string(global_mem_path) {
        Ok(t) => t,
        Err(_) => return Ok(0),
    };
    let dropped = filter_stale_fact_sections(&text, today).1;
    if dropped.is_empty() {
        return Ok(0);
    }
    // Never destroy text: the stale file gets the WHOLE dropped section, not
    // just its title. The previous version wrote `- {title}` and then
    // overwrote the original — on the real 2649-byte facts file that silently
    // deleted 2564 bytes of body while the doc comment promised nothing was
    // lost (review A5).
    let (kept_text, dropped_text) = split_stale_sections(&text, today);
    if dropped_text.is_empty() {
        return Ok(0);
    }
    // Safety net: the split must be lossless. If it is not, leave the file
    // alone and report the inconsistency instead of truncating it.
    if kept_text.len() + dropped_text.len() != text.len() {
        return Err(MemoryError::Other(format!(
            "quarantine refused: split is not lossless ({} + {} != {})",
            kept_text.len(),
            dropped_text.len(),
            text.len()
        )));
    }
    let stale_path = global_mem_path.with_file_name("global_mem.stale.txt");
    let mut stale = std::fs::read_to_string(&stale_path).unwrap_or_default();
    stale.push_str(&format!(
        "\n# Quarantined on {} — sections that no longer match the tree\n",
        today
    ));
    stale.push_str(&dropped_text);
    if !stale.ends_with('\n') {
        stale.push('\n');
    }
    // Write the quarantine copy FIRST, then rewrite the source.
    std::fs::write(&stale_path, stale)?;
    std::fs::write(global_mem_path, kept_text)?;
    Ok(dropped.len())
}

#[cfg(test)]
mod fact_freshness_tests {
    use super::*;

    fn day(s: &str) -> chrono::NaiveDate {
        chrono::NaiveDate::parse_from_str(s, "%Y-%m-%d").unwrap()
    }

    /// round5 P1-11: the real file's first section listed `ga-agent` /
    /// `ga-core` / `ga-memory` — crates that no longer exist. It was injected
    /// into every system prompt as fact.
    #[test]
    fn legacy_crate_sections_are_withheld() {
        let text = "## OpenZen Crate 架构清单 (2026-06-13)\n- ga-agent: 主循环\n- ga-core: 内核\n\n## 用户偏好\n- 喜欢 Rust\n";
        let (kept, dropped) = filter_stale_fact_sections(text, day("2026-10-21"));
        assert!(
            !kept.contains("ga-agent"),
            "the stale architecture list must not reach the prompt"
        );
        assert!(kept.contains("喜欢 Rust"), "fresh sections stay");
        assert_eq!(dropped.len(), 1);
        assert!(dropped[0].contains("架构清单"));
    }

    #[test]
    fn dated_sections_expire_but_verified_ones_do_not() {
        let old = "## 部署流程 (2026-01-01)\n- 用 scripts/install-macos.sh\n";
        let (kept, dropped) = filter_stale_fact_sections(old, day("2026-10-21"));
        assert!(
            kept.is_empty(),
            "a 290-day-old section needs re-verification"
        );
        assert_eq!(dropped.len(), 1);

        let re_verified =
            "## 部署流程 (2026-01-01)\nverified_at: 2026-10-01\n- 用 scripts/install-macos.sh\n";
        let (kept2, dropped2) = filter_stale_fact_sections(re_verified, day("2026-10-21"));
        assert!(!kept2.is_empty(), "an explicit verified_at keeps it");
        assert!(dropped2.is_empty());
    }

    #[test]
    fn quarantine_moves_sections_without_losing_them() {
        let dir = std::env::temp_dir().join(format!("oz-facts-{}", std::process::id()));
        let _ = std::fs::create_dir_all(&dir);
        let path = dir.join("global_mem.txt");
        std::fs::write(
            &path,
            "## Old (2026-06-13)\n- ga-core: x\n\n## Keep\n- 喜欢 Rust\n",
        )
        .unwrap();
        let n = quarantine_stale_fact_sections(&path, day("2026-10-21")).unwrap();
        assert_eq!(n, 1);
        let kept = std::fs::read_to_string(&path).unwrap();
        assert!(kept.contains("喜欢 Rust") && !kept.contains("ga-core"));
        let stale = std::fs::read_to_string(dir.join("global_mem.stale.txt")).unwrap();
        assert!(
            stale.contains("Old"),
            "the removed section must be recoverable"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Review A5: quarantine used to archive only the section TITLE and then
    /// overwrite the source, so a real 2649-byte facts file lost 2564 bytes of
    /// body. The archive must now be lossless.
    #[test]
    fn quarantine_keeps_the_whole_dropped_section() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("global_mem.txt");
        let legacy = "## Crate 架构清单 (2026-06-13)\n\n- ga-agent: 智能体循环\n- ga-core: 核心\n- ga-memory: 记忆层\n\n这里是正文，必须被完整保留。\n";
        let good = "## 当前架构\n\n- oz-core: 核心\n";
        std::fs::write(&path, format!("{legacy}{good}")).unwrap();
        let before = std::fs::read_to_string(&path).unwrap();

        let dropped = quarantine_stale_fact_sections(
            &path,
            chrono::NaiveDate::from_ymd_opt(2026, 10, 21).unwrap(),
        )
        .unwrap();
        assert_eq!(dropped, 1);

        let kept = std::fs::read_to_string(&path).unwrap();
        let stale = std::fs::read_to_string(dir.path().join("global_mem.stale.txt")).unwrap();
        assert!(kept.contains("当前架构"));
        assert!(!kept.contains("ga-agent"));
        // Every byte of the original is still on disk, in one file or the other.
        assert!(
            stale.contains("这里是正文，必须被完整保留。"),
            "body lost: {stale}"
        );
        assert!(stale.contains("ga-memory"));
        // Nothing was invented either: the kept file is exactly the good part.
        assert_eq!(kept, good);
    }

    /// The real legacy file on disk starts with `- ## Title` (a markdown list
    /// item whose text is a heading) and holds the pre-rename `ga-*` list.
    /// Matching only `## ` made it one unstructured blob that the safety rule
    /// then refused to touch, so it survived every hygiene pass.
    #[test]
    fn a_list_item_heading_is_recognised_as_a_section() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("global_mem.txt");
        // First 6 lines of the real ~/.openzen/workspace/facts/global_mem.txt.
        let text = "- ## OpenZen Crate 架构清单 (2026-06-13)\n\n### crates 目录完整列表 (18个 crate):\n1. ga-agent — Agent 编排层\n2. ga-core — Agent 核心运行时\n9. ga-memory — 持久化记忆管理\n\n## 当前架构\n\n- oz-core: 核心\n";
        std::fs::write(&path, text).unwrap();
        let n = quarantine_stale_fact_sections(
            &path,
            chrono::NaiveDate::from_ymd_opt(2026, 10, 22).unwrap(),
        )
        .unwrap();
        assert_eq!(n, 1, "the list-item-heading section must be quarantined");
        let kept = std::fs::read_to_string(&path).unwrap();
        let stale = std::fs::read_to_string(dir.path().join("global_mem.stale.txt")).unwrap();
        assert!(kept.contains("当前架构") && !kept.contains("ga-agent"));
        assert!(stale.contains("ga-memory"), "whole body archived: {stale}");
        assert!(stale.contains("持久化记忆管理"));
    }

    /// A header-less facts file has no structure to judge — it must survive.
    #[test]
    fn a_headerless_file_is_never_quarantined() {
        // Ordinary content: no structure to judge, so it must survive.
        let text = "- 用户喜欢 Rust\n- 项目在 ~/Documents/apps\n";
        let (kept, dropped) =
            split_stale_sections(text, chrono::NaiveDate::from_ymd_opt(2026, 10, 21).unwrap());
        assert_eq!(kept, text);
        assert!(dropped.is_empty());
    }

    /// …but a header-less file that IS the legacy list (only `ga-*`, no current
    /// names) is stale as a whole. This is the real
    /// `workspace/facts/global_mem.txt` on the author's machine (41 lines,
    /// one `- ## …` heading, 19 `ga-` references).
    #[test]
    fn a_single_section_legacy_file_is_archived_whole() {
        let text = "- ## OpenZen Crate 架构清单 (2026-06-13)\n\n### crates 列表:\n1. ga-agent — 编排层\n2. ga-memory — 记忆\n";
        let (kept, dropped) =
            split_stale_sections(text, chrono::NaiveDate::from_ymd_opt(2026, 10, 22).unwrap());
        assert!(kept.is_empty(), "the stale file must be emptied");
        assert_eq!(dropped, text, "…and archived byte-for-byte");
    }
}
