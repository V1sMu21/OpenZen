//! Metadata persistence — reads/writes `meta.toml` for knowledge artifacts.
//!
//! Each knowledge artifact (skill, SOP) has a companion `meta.toml` file
//! that tracks usage statistics, quality scores, and lifecycle state.

use std::path::{Path, PathBuf};

use oz_core_types::SkillMcpMetadata;

use crate::SkillMcpError;

/// Manages `meta.toml` persistence for knowledge artifacts.
pub struct MetaStore {
    base_dir: PathBuf,
}

impl MetaStore {
    /// Create a MetaStore rooted at the knowledge base directory.
    pub fn new(base_dir: &Path) -> Self {
        MetaStore {
            base_dir: base_dir.to_path_buf(),
        }
    }

    /// Build the path to meta.toml for a given category and artifact name.
    pub fn meta_path(&self, category: &str, name: &str) -> PathBuf {
        self.base_dir.join(category).join(name).join("meta.toml")
    }

    /// Load metadata for an artifact.
    /// Returns `None` if the file does not exist.
    pub fn load(&self, category: &str, name: &str) -> Result<Option<SkillMcpMetadata>, SkillMcpError> {
        let path = self.meta_path(category, name);
        if !path.exists() {
            return Ok(None);
        }
        let content = std::fs::read_to_string(&path).map_err(SkillMcpError::Io)?;
        let meta = toml::from_str(&content)
            .map_err(|e| SkillMcpError::Parse(format!("Failed to parse {}: {e}", path.display())))?;
        Ok(Some(meta))
    }

    /// Save metadata for an artifact.
    /// Creates parent directories if needed.
    pub fn save(&self, category: &str, name: &str, meta: &SkillMcpMetadata) -> Result<(), SkillMcpError> {
        let path = self.meta_path(category, name);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).map_err(SkillMcpError::Io)?;
        }
        let content = toml::to_string_pretty(meta)
            .map_err(|e| SkillMcpError::Serialize(e.to_string()))?;
        std::fs::write(&path, &content).map_err(SkillMcpError::Io)?;
        Ok(())
    }

    /// Delete metadata for an artifact.
    pub fn delete(&self, category: &str, name: &str) -> Result<(), SkillMcpError> {
        let path = self.meta_path(category, name);
        if path.exists() {
            std::fs::remove_file(&path).map_err(SkillMcpError::Io)?;
        }
        Ok(())
    }

    /// List all artifact names in a category that have meta.toml files.
    pub fn list_category(&self, category: &str) -> Result<Vec<SkillMcpMetadata>, SkillMcpError> {
        let dir = self.base_dir.join(category);
        if !dir.exists() {
            return Ok(vec!());
        }

        let mut metas = Vec::new();
        for entry in std::fs::read_dir(&dir).map_err(SkillMcpError::Io)? {
            let entry = entry.map_err(SkillMcpError::Io)?;
            let meta_path = entry.path().join("meta.toml");
            if meta_path.exists() {
                if let Some(meta) = self.load_meta_file(&meta_path)? {
                    metas.push(meta);
                }
            }
        }
        Ok(metas)
    }

    /// Load metadata from a specific path (used internally).
    fn load_meta_file(&self, path: &Path) -> Result<Option<SkillMcpMetadata>, SkillMcpError> {
        let content = std::fs::read_to_string(path).map_err(SkillMcpError::Io)?;
        let meta: SkillMcpMetadata = toml::from_str(&content)
            .map_err(|e| SkillMcpError::Parse(format!("{}: {e}", path.display())))?;
        Ok(Some(meta))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_meta() -> SkillMcpMetadata {
        SkillMcpMetadata::new("test_id", "A test artifact", vec!["test".into()])
    }

    #[test]
    fn test_save_and_load() {
        let dir = tempfile::tempdir().unwrap();
        let store = MetaStore::new(dir.path());

        let meta = test_meta();
        store.save("skills", "my_skill", &meta).unwrap();

        let loaded = store.load("skills", "my_skill").unwrap().unwrap();
        assert_eq!(loaded.id, "test_id");
        assert_eq!(loaded.version, 1);
    }

    #[test]
    fn test_load_missing_returns_none() {
        let dir = tempfile::tempdir().unwrap();
        let store = MetaStore::new(dir.path());
        assert!(store.load("skills", "nope").unwrap().is_none());
    }

    #[test]
    fn test_delete() {
        let dir = tempfile::tempdir().unwrap();
        let store = MetaStore::new(dir.path());
        store.save("sops", "del_me", &test_meta()).unwrap();
        assert!(store.load("sops", "del_me").unwrap().is_some());
        store.delete("sops", "del_me").unwrap();
        assert!(store.load("sops", "del_me").unwrap().is_none());
    }

    #[test]
    fn test_list_category() {
        let dir = tempfile::tempdir().unwrap();
        let store = MetaStore::new(dir.path());

        for name in &["a", "b", "c"] {
            let mut meta = SkillMcpMetadata::new(name, "", vec![]);
            meta.id = name.to_string();
            store.save("skills", name, &meta).unwrap();
        }

        let list = store.list_category("skills").unwrap();
        assert_eq!(list.len(), 3);
    }

    #[test]
    fn test_empty_category_returns_empty_vec() {
        let dir = tempfile::tempdir().unwrap();
        let store = MetaStore::new(dir.path());
        let list = store.list_category("nonexistent_dir").unwrap();
        assert!(list.is_empty());
    }

    #[test]
    fn test_update_via_save() {
        let dir = tempfile::tempdir().unwrap();
        let store = MetaStore::new(dir.path());

        let mut meta = test_meta();
        meta.quality_score = 0.9;
        store.save("skills", "updated", &meta).unwrap();

        // Re-save with different data
        meta.quality_score = 0.7;
        meta.record_success(3);
        store.save("skills", "updated", &meta).unwrap();

        let loaded = store.load("skills", "updated").unwrap().unwrap();
        assert_eq!(loaded.quality_score, 0.85);
        assert_eq!(loaded.success_count, 1);
    }
}
