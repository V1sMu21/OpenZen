//! Bookkeeping for "have I already imported this foreign session?".
//!
//! Kept in a small JSON file under the OpenZen data root rather than inside
//! `sessions.json` so that re-scanning a source can mark rows as already
//! imported without loading every stored session.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::model::ImportSource;

/// One recorded import.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LedgerEntry {
    /// The OpenZen session id this foreign session was imported as.
    pub session_id: String,
    pub title: String,
    pub message_count: usize,
    /// RFC 3339.
    pub imported_at: String,
    /// Re-importing creates a fresh copy; bump on each import.
    #[serde(default)]
    pub import_count: u32,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
struct LedgerFile {
    #[serde(default = "default_version")]
    version: u32,
    #[serde(default)]
    imports: BTreeMap<String, LedgerEntry>,
}

fn default_version() -> u32 {
    1
}

/// Read/write helper for `imported_sessions.json`.
#[derive(Debug, Clone)]
pub struct ImportLedger {
    path: PathBuf,
    file: LedgerFile,
}

/// Stable key for a (source, source-session) pair.
pub fn ledger_key(source: ImportSource, source_id: &str) -> String {
    format!("{}::{}", source.id(), source_id)
}

impl ImportLedger {
    /// Load the ledger at `path`, treating a missing or unreadable file as
    /// empty — a corrupt ledger must never block an import.
    pub fn load(path: impl Into<PathBuf>) -> Self {
        let path = path.into();
        let file = std::fs::read_to_string(&path)
            .ok()
            .and_then(|raw| serde_json::from_str::<LedgerFile>(&raw).ok())
            .unwrap_or_default();
        Self { path, file }
    }

    /// `<data_root>/openzen/imported_sessions.json`.
    pub fn default_path(data_root: &Path) -> PathBuf {
        data_root.join("openzen").join("imported_sessions.json")
    }

    pub fn get(&self, source: ImportSource, source_id: &str) -> Option<&LedgerEntry> {
        self.file.imports.get(&ledger_key(source, source_id))
    }

    pub fn is_imported(&self, source: ImportSource, source_id: &str) -> bool {
        self.get(source, source_id).is_some()
    }

    /// Record an import, incrementing the counter on repeat imports.
    pub fn record(
        &mut self,
        source: ImportSource,
        source_id: &str,
        session_id: impl Into<String>,
        title: impl Into<String>,
        message_count: usize,
    ) {
        let key = ledger_key(source, source_id);
        let previous = self.file.imports.get(&key).map(|e| e.import_count).unwrap_or(0);
        self.file.imports.insert(
            key,
            LedgerEntry {
                session_id: session_id.into(),
                title: title.into(),
                message_count,
                imported_at: chrono::Utc::now().to_rfc3339(),
                import_count: previous + 1,
            },
        );
    }

    /// Persist to disk, creating the parent directory as needed.
    pub fn save(&self) -> std::io::Result<()> {
        if let Some(parent) = self.path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let body = serde_json::to_string_pretty(&self.file)
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
        std::fs::write(&self.path, body)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn missing_ledger_is_empty() {
        let ledger = ImportLedger::load("/nonexistent-xyz/imported.json");
        assert!(!ledger.is_imported(ImportSource::Zcode, "sess_1"));
    }

    #[test]
    fn records_and_persists_imports() {
        let dir = tempfile::tempdir().unwrap();
        let path = ImportLedger::default_path(dir.path());
        let mut ledger = ImportLedger::load(&path);
        ledger.record(ImportSource::Dsh, "abc", "uuid-1", "Title", 12);
        assert!(ledger.is_imported(ImportSource::Dsh, "abc"));
        assert!(!ledger.is_imported(ImportSource::Zcode, "abc"));
        ledger.save().unwrap();

        let reloaded = ImportLedger::load(&path);
        let entry = reloaded.get(ImportSource::Dsh, "abc").unwrap();
        assert_eq!(entry.session_id, "uuid-1");
        assert_eq!(entry.message_count, 12);
        assert_eq!(entry.import_count, 1);

        let mut again = ImportLedger::load(&path);
        again.record(ImportSource::Dsh, "abc", "uuid-2", "Title", 12);
        assert_eq!(again.get(ImportSource::Dsh, "abc").unwrap().import_count, 2);
    }

    #[test]
    fn corrupt_ledger_degrades_to_empty() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("imported.json");
        std::fs::write(&path, "not json at all").unwrap();
        let ledger = ImportLedger::load(&path);
        assert!(!ledger.is_imported(ImportSource::Zcode, "x"));
    }
}
