//! End-to-end check of the session-import **write** path.
//!
//! Lives under `src/` rather than `tests/` on purpose: `.gitignore` carries a
//! broad `tests/` rule (intended for the 3.2GB `tests/longtask/` fixtures),
//! which would silently exclude a new integration test there.
//!
//! `import_sessions` (commands.rs) does something subtle:
//! `SessionStore::create_with_project` persists a session with an empty
//! `messages` vector immediately, and only afterwards does the command attach
//! the parsed bodies and call `save()` again. Persistence is fingerprint-gated
//! on `messages.len()` (crates/oz-server/src/webui/sessions.rs
//! `entry_fingerprint`), so getting that order wrong would silently persist
//! empty sessions.
//!
//! This drives the real [`SessionStore::persisted`] against a real ZCode/DSH
//! session when one exists locally, reloads from disk, and asserts the bodies
//! round-trip. It self-skips when the sources are absent so CI (which has no
//! `~/.zcode` / `~/.dsh`) still passes.

use std::path::PathBuf;
use std::time::Duration;

use oz_import::{ImportSource, ImportedSession, SessionSummary, SourcePaths};
use oz_server::webui::sessions::SessionStore;

/// Isolated scratch directory (avoiding a tempfile dependency here).
fn scratch(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "openzen-import-test-{}-{name}",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("create scratch dir");
    dir
}

/// Mirrors the write sequence in `commands::import_sessions` for one parsed
/// session. Kept in sync with that command deliberately.
fn replicate_command_write(
    store: &mut SessionStore,
    session: &ImportedSession,
    working_dir: &str,
) -> String {
    let info = store.create_with_project(&session.title, None, None, Some(working_dir));
    let message_count = session.messages.len();
    {
        let entry = store.get_mut(&info.id).expect("session was just created");
        entry.messages = session.messages.clone();
        entry.info.message_count = message_count;
        if let Some(created) = session.created_at {
            entry.created_at = created;
            entry.info.created_at = created.to_rfc3339();
        }
    }
    store.save();
    store.wait_persisted(Duration::from_secs(10));
    info.id
}

/// The largest available session, to exercise a realistic message volume.
fn pick(source: ImportSource, paths: &SourcePaths) -> Option<SessionSummary> {
    oz_import::list_sessions(source, paths)
        .ok()?
        .into_iter()
        .max_by_key(|s| s.message_count)
}

#[test]
fn imported_session_survives_a_disk_reload() {
    let paths = SourcePaths::default();
    let mut checked = 0usize;

    for source in ImportSource::ALL {
        let Some(summary) = pick(source, &paths) else {
            continue;
        };
        let Ok(session) = oz_import::read_session(source, &summary.source_id, &paths) else {
            continue;
        };
        if session.messages.is_empty() {
            continue;
        }

        let dir = scratch(source.id());
        let path = dir.join("sessions.json");

        let mut store = SessionStore::persisted(path.clone());
        let id = replicate_command_write(&mut store, &session, "/tmp/imported");

        // Reload from disk — exactly what a fresh app launch sees.
        let reloaded = SessionStore::persisted(path.clone());
        let entry = reloaded
            .get(&id)
            .unwrap_or_else(|| panic!("{}: imported session vanished after reload", source.id()));

        assert_eq!(
            entry.messages,
            session.messages,
            "{}: message bodies did not round-trip through sessions.json",
            source.id()
        );
        assert_eq!(
            entry.messages.len(),
            session.messages.len(),
            "{}: message count drifted",
            source.id()
        );
        assert_eq!(entry.info.name, session.title);
        assert!(
            entry.messages.iter().all(|m| m.get("role").is_some()),
            "{}: every imported message must carry a role",
            source.id()
        );

        // The sidebar derives its count from the live vector, not from info.
        let listed = reloaded
            .list()
            .into_iter()
            .find(|s| s.id == id)
            .expect("session missing from list()");
        assert_eq!(listed.message_count, session.messages.len());

        checked += 1;
        let _ = std::fs::remove_dir_all(&dir);
    }

    if checked == 0 {
        eprintln!("import_write_path: skipped — no local ZCode/DSH sessions on this machine");
    }
}
