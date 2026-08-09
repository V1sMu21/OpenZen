use crate::core::types::{Memory, MemoryContent, MemoryMeta};
use crate::core::MemoryResult;
use std::fs;
use std::path::Path;

/// A JSON-serializable representation of a single memory for export/import.
#[derive(serde::Serialize, serde::Deserialize)]
pub struct MemoryExport {
    pub id: u64,
    pub content: MemoryContent,
    pub metadata: MemoryMeta,
    pub alias: Option<String>,
    pub tags: Vec<String>,
}

impl From<Memory> for MemoryExport {
    fn from(m: Memory) -> Self {
        Self {
            id: m.id,
            content: m.content,
            metadata: m.metadata,
            alias: m.alias,
            tags: m.tags,
        }
    }
}

impl From<MemoryExport> for Memory {
    fn from(e: MemoryExport) -> Self {
        Self {
            id: e.id,
            content: e.content,
            metadata: e.metadata,
            alias: e.alias,
            tags: e.tags,
        }
    }
}

/// Export memories to a JSON file.
pub fn export_to_json(memories: &[Memory], path: impl AsRef<Path>) -> MemoryResult<()> {
    let exports: Vec<MemoryExport> = memories.iter().cloned().map(MemoryExport::from).collect();
    let json = serde_json::to_string_pretty(&exports)
        .map_err(|e| crate::core::error::MemoryError::Serialization(e.to_string()))?;
    fs::write(path.as_ref(), json).map_err(|e| {
        crate::core::error::MemoryError::WalWrite(format!("cannot write export: {}", e))
    })?;
    Ok(())
}

/// Import memories from a JSON file.
pub fn import_from_json(path: impl AsRef<Path>) -> MemoryResult<Vec<Memory>> {
    let data = fs::read_to_string(path.as_ref()).map_err(|e| {
        crate::core::error::MemoryError::WalWrite(format!("cannot read import file: {}", e))
    })?;
    let exports: Vec<MemoryExport> = serde_json::from_str(&data)
        .map_err(|e| crate::core::error::MemoryError::Serialization(e.to_string()))?;
    Ok(exports.into_iter().map(Memory::from).collect())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::Fact;
    use tempfile::tempdir;

    #[test]
    fn test_export_roundtrip() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("export.json");

        let mem = Memory::new(MemoryContent::Fact(Fact::new("alice", "knows", "rust")))
            .with_importance(0.8);
        let mems = vec![mem];

        export_to_json(&mems, &path).unwrap();
        let imported = import_from_json(&path).unwrap();

        assert_eq!(imported.len(), 1);
        assert_eq!(imported[0].id, mems[0].id);
        assert_eq!(imported[0].content_text(), mems[0].content_text());
        assert!((imported[0].metadata.importance - 0.8).abs() < 0.01);
    }

    #[test]
    fn test_export_empty() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("empty.json");
        export_to_json(&[], &path).unwrap();
        let imported = import_from_json(&path).unwrap();
        assert!(imported.is_empty());
    }

    #[test]
    fn test_export_with_tags_and_alias() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("tags.json");

        let mut mem = Memory::new(MemoryContent::Fact(Fact::new("x", "y", "z")));
        mem.tags = vec!["important".to_string(), "work".to_string()];
        mem.alias = Some("my_alias".to_string());

        export_to_json(&[mem], &path).unwrap();
        let imported = import_from_json(&path).unwrap();

        assert_eq!(imported[0].tags.len(), 2);
        assert!(imported[0].tags.contains(&"important".to_string()));
        assert_eq!(imported[0].alias, Some("my_alias".to_string()));
    }
}
