//! 4-level memory system for OpenZen.
//!
//! - **L1** — `global_mem_insight.txt` — distilled insights
//! - **L2** — `global_mem.txt` — persistent global facts
//! - **L3** — `memory/` directory — SOP documents
//! - **L4** — `memory/L4_raw_sessions/` — raw session history

use std::path::{Path, PathBuf};

/// Error type for memory operations.
#[derive(Debug, thiserror::Error)]
pub enum MemoryError {
    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),
    #[error("Parse error: {0}")]
    Parse(String),
    #[error("Memory dir not found: {0}")]
    DirNotFound(PathBuf),
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
            if path.extension().map_or(false, |e| e == "md" || e == "txt") {
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
    async fn read_insight_structure(&self) -> String {
        let global = self.read_global().await.unwrap_or_default();
        if !global.is_empty() {
            format!("../memory/global_mem.txt:\n{global}\n")
        } else {
            String::new()
        }
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
                        let content = format!("# {}\n\n{}\n\n---\nSource: {}\n", item.content, item.content, item.source);
                        tokio::fs::write(&path, &content).await?;
                    }
                }
                MemoryCategory::History => {
                    tokio::fs::create_dir_all(&self.l4_dir).await?;
                    let ts = chrono::Utc::now().format("%Y%m%d_%H%M%S");
                    let path = self.l4_dir.join(format!("session_{ts}.md"));
                    let content = format!("# Session {ts}\n\nSource: {}\n\n{}\n", item.source, item.content);
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
        .map(|c| if c.is_alphanumeric() || c == '-' || c == '_' { c } else { '_' })
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
        assert!(sops.iter().any(|p| p.to_string_lossy().contains("my_cool_procedure")));
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
