//! Import sessions from other local coding agents into OpenZen.
//!
//! Supported sources:
//!
//! - **ZCode** (`~/.zcode/cli/db/db.sqlite`) — see [`zcode`].
//! - **DeepSeek Harness** (`~/.dsh/sessions/**/session.vN.jsonl[.zstd]`) —
//!   see [`dsh`].
//!
//! Both readers normalise their own event model into a neutral
//! [`convert::Exchange`] and then emit OpenZen's persisted message shape, so
//! the frontend restores an imported session through exactly the same path as
//! a natively recorded one.
//!
//! All source access is read-only: ZCode's SQLite database is opened with
//! `SQLITE_OPEN_READ_ONLY` (it may be held open by a live ZCode process) and
//! DSH logs are only ever opened for reading.

pub mod convert;
pub mod dsh;
pub mod ledger;
pub mod model;
pub mod zcode;

pub use ledger::{ledger_key, ImportLedger, LedgerEntry};
pub use model::{
    ImportError, ImportSource, ImportedSession, SessionSummary, SourcePaths, SourceStatus,
};

/// Describe both sources: whether each is present, and how many sessions it
/// holds. Never fails — discoverability problems are reported per source so
/// the UI can show a partial list.
pub fn scan_sources(paths: &SourcePaths) -> Vec<SourceStatus> {
    ImportSource::ALL
        .iter()
        .map(|&source| match list_sessions(source, paths) {
            Ok(sessions) => SourceStatus {
                id: source.id().to_string(),
                label: source.label().to_string(),
                available: true,
                detail: Some(source_detail(source, paths)),
                session_count: Some(sessions.len()),
                error: None,
            },
            Err(err) => SourceStatus {
                id: source.id().to_string(),
                label: source.label().to_string(),
                available: false,
                detail: Some(source_detail(source, paths)),
                session_count: None,
                error: Some(err.to_string()),
            },
        })
        .collect()
}

fn source_detail(source: ImportSource, paths: &SourcePaths) -> String {
    match source {
        ImportSource::Zcode => paths.zcode_db.display().to_string(),
        ImportSource::Dsh => paths.dsh_sessions_dir.display().to_string(),
    }
}

/// List importable sessions for one source.
pub fn list_sessions(
    source: ImportSource,
    paths: &SourcePaths,
) -> Result<Vec<SessionSummary>, ImportError> {
    match source {
        ImportSource::Zcode => zcode::list_sessions(paths),
        ImportSource::Dsh => dsh::list_sessions(paths),
    }
}

/// Parse one session into OpenZen messages.
pub fn read_session(
    source: ImportSource,
    source_id: &str,
    paths: &SourcePaths,
) -> Result<ImportedSession, ImportError> {
    match source {
        ImportSource::Zcode => zcode::read_session(source_id, paths),
        ImportSource::Dsh => dsh::read_session(source_id, paths),
    }
}

/// Parse a source id from the IPC boundary.
pub fn parse_source(id: &str) -> Result<ImportSource, ImportError> {
    ImportSource::parse(id).ok_or_else(|| ImportError::UnknownSource(id.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scan_reports_every_source_even_when_absent() {
        let paths = SourcePaths::from_home("/nonexistent-home-xyz");
        let statuses = scan_sources(&paths);
        assert_eq!(statuses.len(), 2);
        let ids: Vec<&str> = statuses.iter().map(|s| s.id.as_str()).collect();
        assert_eq!(ids, vec!["zcode", "dsh"]);
        assert!(statuses.iter().all(|s| !s.available));
        assert!(statuses.iter().all(|s| s.error.is_some()));
    }

    #[test]
    fn unknown_source_id_is_rejected() {
        assert!(parse_source("zcode").is_ok());
        assert!(parse_source("DSH").is_ok());
        assert!(matches!(
            parse_source("claude").unwrap_err(),
            ImportError::UnknownSource(_)
        ));
    }
}
