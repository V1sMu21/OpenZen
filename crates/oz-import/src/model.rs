//! Shared types for the foreign-session importer.

use std::fmt;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

/// Which foreign agent produced a session store.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ImportSource {
    /// z.ai official CLI — `~/.zcode/cli/db/db.sqlite`.
    Zcode,
    /// DeepSeek Harness — `~/.dsh/sessions/<cwd-slug>/<session-id>/session.vN.jsonl[.zstd]`.
    Dsh,
}

impl ImportSource {
    /// Stable identifier used across the Tauri IPC boundary.
    pub fn id(self) -> &'static str {
        match self {
            ImportSource::Zcode => "zcode",
            ImportSource::Dsh => "dsh",
        }
    }

    /// Human-facing name (brand names are not translated).
    pub fn label(self) -> &'static str {
        match self {
            ImportSource::Zcode => "ZCode",
            ImportSource::Dsh => "DeepSeek Harness",
        }
    }

    /// Parse an IPC-supplied source id.
    pub fn parse(s: &str) -> Option<Self> {
        match s.trim().to_ascii_lowercase().as_str() {
            "zcode" => Some(ImportSource::Zcode),
            "dsh" => Some(ImportSource::Dsh),
            _ => None,
        }
    }

    pub const ALL: [ImportSource; 2] = [ImportSource::Zcode, ImportSource::Dsh];
}

impl fmt::Display for ImportSource {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.id())
    }
}

/// Where the importer looks for each source. Kept as a struct so tests can
/// point at fixtures instead of the real user data root.
#[derive(Debug, Clone)]
pub struct SourcePaths {
    /// `~/.zcode/cli/db/db.sqlite`
    pub zcode_db: PathBuf,
    /// `~/.dsh/sessions`
    pub dsh_sessions_dir: PathBuf,
}

impl Default for SourcePaths {
    fn default() -> Self {
        Self::from_home(std::env::var("HOME").unwrap_or_default())
    }
}

impl SourcePaths {
    pub fn from_home(home: impl AsRef<std::path::Path>) -> Self {
        let home = home.as_ref();
        Self {
            zcode_db: home.join(".zcode/cli/db/db.sqlite"),
            dsh_sessions_dir: home.join(".dsh/sessions"),
        }
    }
}

/// One row in the source picker.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SourceStatus {
    pub id: String,
    pub label: String,
    pub available: bool,
    /// The path (or other location hint) the importer reads.
    pub detail: Option<String>,
    /// `None` when discovery failed before it could count anything.
    pub session_count: Option<usize>,
    pub error: Option<String>,
}

/// A session that can be imported — metadata only, no bodies.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionSummary {
    /// The session's identifier inside the foreign store.
    pub source_id: String,
    pub title: String,
    pub directory: Option<String>,
    /// RFC 3339.
    pub created_at: Option<String>,
    pub message_count: usize,
    /// True when this source session was already imported before.
    pub already_imported: bool,
}

/// A fully parsed session, ready to be written into OpenZen.
#[derive(Debug, Clone)]
pub struct ImportedSession {
    pub source_id: String,
    pub title: String,
    pub directory: Option<String>,
    pub created_at: Option<chrono::DateTime<chrono::Utc>>,
    /// OpenZen-shaped message JSON (`role`/`content`/`timestamp`/`streamEvents`).
    pub messages: Vec<serde_json::Value>,
}

#[derive(Debug, thiserror::Error)]
pub enum ImportError {
    #[error("unknown import source '{0}'")]
    UnknownSource(String),
    #[error("source '{source_id}' is not available: {reason}")]
    SourceUnavailable {
        /// Named `source_id` rather than `source`: thiserror 2 auto-detects a
        /// field called `source` as the error cause and requires `StdError`.
        source_id: &'static str,
        reason: String,
    },
    #[error("session '{0}' was not found in the source store")]
    SessionNotFound(String),
    #[error("storage error: {0}")]
    Storage(String),
    #[error("malformed source data: {0}")]
    Malformed(String),
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
}

impl From<rusqlite::Error> for ImportError {
    fn from(e: rusqlite::Error) -> Self {
        ImportError::Storage(e.to_string())
    }
}
