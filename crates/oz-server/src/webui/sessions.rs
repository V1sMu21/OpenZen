use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{self, Sender};
use std::sync::Arc;

use serde::{Deserialize, Serialize};

/// Session status.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum SessionStatus {
    Idle,
    Running,
    /// The run is parked on an unanswered `ask_user` question. Distinct
    /// from Running so the sidebar/title bar can say "waiting for you"
    /// instead of pretending the model is still working.
    Waiting,
    Stopped,
}

impl SessionStatus {
    pub fn as_str(&self) -> &'static str {
        match self {
            SessionStatus::Idle => "idle",
            SessionStatus::Running => "running",
            SessionStatus::Waiting => "waiting",
            SessionStatus::Stopped => "stopped",
        }
    }
}

/// Public session info returned to clients.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionInfo {
    pub id: String,
    pub name: String,
    pub created_at: String,
    pub status: String,
    pub message_count: usize,
    #[serde(default)]
    pub project_id: Option<String>,
    #[serde(default)]
    pub project_name: Option<String>,
    /// Working directory for this session (resolved at creation time
    /// from project root_path or default). Used by agent loop.
    #[serde(default)]
    pub working_dir: Option<String>,
}

/// The question a waiting session is parked on. Persisted with the session
/// so a page reload (or an app restart) can restore the dialog instead of
/// showing a blank "running" row.
#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq)]
pub struct PendingAskUserRecord {
    pub tool_use_id: String,
    #[serde(default)]
    pub tool_call_id: String,
    pub question: String,
    #[serde(default)]
    pub candidates: Vec<String>,
    #[serde(default)]
    pub asked_at: String,
}

/// Full session entry (internal).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionEntry {
    pub info: SessionInfo,
    pub status: SessionStatus,
    pub messages: Vec<serde_json::Value>,
    pub created_at: chrono::DateTime<chrono::Utc>,
    #[serde(default)]
    pub project_id: Option<String>,
    #[serde(default)]
    pub working_dir: Option<String>,
    /// Todo items restored from the latest checkpoint (for UI recovery).
    #[serde(default)]
    pub todos: Vec<oz_core_types::TodoItem>,
    /// Open ask_user question when `status == Waiting`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pending_ask_user: Option<PendingAskUserRecord>,
    /// Bumped by every `get_mut` (i.e. every in-place mutation). The persist
    /// cache compares it so that a rewrite which changes a message's
    /// *content* without changing the message *count* — `/compact` folding
    /// tool output, a checkpoint rebuild, a todo status flip — is still
    /// written to disk. Counting alone silently persisted the pre-change
    /// snapshot, so the change came back after a restart (round5 P0-3).
    #[serde(default)]
    pub revision: u64,
}

/// In-memory session store with optional file persistence.
pub struct SessionStore {
    sessions: HashMap<String, SessionEntry>,
    path: Option<PathBuf>,
    max_sessions: usize,
    writer: Option<PersistWriter>,
    /// Last submitted per-session serializations with their fingerprints
    /// (interior-mutable: save() takes &self because read-guard callers
    /// persist through the tokio RwLock).
    persist_cache: std::sync::Mutex<HashMap<String, (EntryFingerprint, Arc<String>)>>,
}

/// Background persistence writer. `save()` snapshots the session map and
/// hands it to a dedicated thread which serialises and writes it with
/// "latest wins" coalescing: a burst of saves during a long agent run
/// collapses into a single disk write, and serialisation never blocks the
/// IPC thread. Stores built with `SessionStore::new()` (no path, e.g. in
/// tests) keep the old synchronous write path.
/// Per-session serialized JSON, shared between saves: only sessions whose
/// fingerprint changed since the last snapshot are re-serialized, so a
/// save costs O(dirty sessions) instead of deep-cloning and re-serializing
/// the whole ≤500-session map (the dominant CPU spike on every
/// send/stop/status flip in 7x24 runs).
type SessionSnapshot = HashMap<String, Arc<String>>;
type PersistPayload = (PathBuf, SessionSnapshot);

/// Cheap staleness fingerprint. `revision` is bumped by every in-place
/// mutation (see `SessionEntry::revision`), so this is no longer fooled by
/// "same count, different content" edits.
#[derive(Clone, PartialEq)]
struct EntryFingerprint {
    revision: u64,
    messages: usize,
    todos: usize,
    status: SessionStatus,
    info_name: String,
    info_status: String,
    info_message_count: usize,
    info_project_id: Option<String>,
    info_working_dir: Option<String>,
    pending_ask_user: Option<PendingAskUserRecord>,
}

fn entry_fingerprint(e: &SessionEntry) -> EntryFingerprint {
    EntryFingerprint {
        revision: e.revision,
        messages: e.messages.len(),
        todos: e.todos.len(),
        status: e.status.clone(),
        info_name: e.info.name.clone(),
        info_status: e.info.status.clone(),
        info_message_count: e.info.message_count,
        info_project_id: e.info.project_id.clone(),
        info_working_dir: e.info.working_dir.clone(),
        pending_ask_user: e.pending_ask_user.clone(),
    }
}

struct PersistWriter {
    tx: Sender<PersistPayload>,
    /// Snapshots submitted but not yet written (or discarded). `reload()`
    /// must skip while this is non-zero: the on-disk file is older than
    /// memory in that window and reloading would clobber newer state.
    pending: Arc<AtomicU64>,
    /// mtime of the file as of our own last write. Shared with the worker
    /// thread so `reload_if_stale` can tell "another process wrote" from
    /// "this is our own write coming back".
    last_write: Arc<std::sync::Mutex<Option<std::time::SystemTime>>>,
}

impl PersistWriter {
    fn spawn() -> Self {
        let (tx, rx) = mpsc::channel::<PersistPayload>();
        let pending = Arc::new(AtomicU64::new(0));
        let last_write = Arc::new(std::sync::Mutex::new(None));
        let worker_last_write = Arc::clone(&last_write);
        let worker_pending = Arc::clone(&pending);
        std::thread::Builder::new()
            .name("openzen-session-persist".into())
            .spawn(move || {
                // Coalesce: write only the newest snapshot of the drained
                // batch. The payload travels in the channel itself, so the
                // pending counter always matches the number of in-flight
                // payloads — the old slot+pending pair could drift out of
                // sync and wedge reload() forever.
                while let Ok(item) = rx.recv() {
                    let mut latest = Some(item);
                    while let Ok(item) = rx.try_recv() {
                        latest = Some(item);
                        worker_pending.fetch_sub(1, Ordering::SeqCst);
                    }
                    if let Some((path, snapshot)) = latest {
                        // Assemble the file from per-session JSON strings.
                        // Keys are sorted for stable, diff-friendly output.
                        let mut json = String::with_capacity(4096);
                        json.push('{');
                        let mut keys: Vec<&String> = snapshot.keys().collect();
                        keys.sort();
                        for (i, k) in keys.iter().enumerate() {
                            if i > 0 {
                                json.push(',');
                            }
                            json.push_str(&serde_json::to_string(k).unwrap_or_default());
                            json.push(':');
                            json.push_str(&snapshot[*k]);
                        }
                        json.push('}');
                        SessionStore::write_atomic(&path, &json);
                        // Record when WE last wrote, so a later staleness
                        // check does not mistake our own write for another
                        // process's change (and vice versa).
                        if let Ok(meta) = std::fs::metadata(&path) {
                            if let Ok(mtime) = meta.modified() {
                                *worker_last_write.lock().unwrap_or_else(|e| e.into_inner()) =
                                    Some(mtime);
                            }
                        }
                        worker_pending.fetch_sub(1, Ordering::SeqCst);
                    }
                }
            })
            .expect("failed to spawn session persist thread");
        PersistWriter {
            tx,
            pending,
            last_write,
        }
    }

    fn submit(&self, path: PathBuf, snapshot: SessionSnapshot) {
        self.pending.fetch_add(1, Ordering::SeqCst);
        if self.tx.send((path, snapshot)).is_err() {
            // Worker thread is gone; keep the counter honest so reload()
            // can still proceed.
            self.pending.fetch_sub(1, Ordering::SeqCst);
        }
    }
}

impl Default for SessionStore {
    fn default() -> Self {
        Self::new()
    }
}

impl SessionStore {
    pub fn new() -> Self {
        SessionStore {
            sessions: HashMap::new(),
            path: None,
            max_sessions: 0,
            persist_cache: std::sync::Mutex::new(HashMap::new()),
            writer: None,
        }
    }

    pub fn with_max(mut self, max: usize) -> Self {
        self.max_sessions = max;
        self.evict();
        self
    }

    pub fn set_max_sessions(&mut self, max: usize) {
        self.max_sessions = max;
        self.evict();
    }

    fn evict(&mut self) {
        if self.max_sessions == 0 || self.sessions.len() <= self.max_sessions {
            return;
        }
        let mut entries: Vec<(String, chrono::DateTime<chrono::Utc>)> = self
            .sessions
            .iter()
            .map(|(id, e)| (id.clone(), e.created_at))
            .collect();
        entries.sort_by_key(|(_, ts)| *ts);
        while self.sessions.len() > self.max_sessions {
            if let Some((id, _)) = entries.first() {
                if let Some(entry) = self.sessions.get(id) {
                    let json = serde_json::to_value(entry).ok();
                    self.archive_entry(id, json);
                }
                self.sessions.remove(id);
                entries.remove(0);
            } else {
                break;
            }
        }
    }

    fn archive_entry(&self, id: &str, json: Option<serde_json::Value>) {
        let archive_dir = if let Some(ref p) = self.path {
            if let Some(parent) = p.parent() {
                parent.join("sessions_archive")
            } else {
                return;
            }
        } else {
            return;
        };
        let _ = std::fs::create_dir_all(&archive_dir);
        let archive_path = archive_dir.join(format!("{}.json", id));
        if let Some(json_val) = json {
            if let Ok(pretty) = serde_json::to_string_pretty(&json_val) {
                let _ = std::fs::write(&archive_path, &pretty);
            }
        }
    }

    /// Remove sessions idle past `threshold`, archiving each first (same
    /// as capacity eviction). Runs in-process against the authoritative
    /// map — a disk-side cleanup would be resurrected by the next save.
    /// Returns the number removed.
    ///
    /// Staleness is measured from the LAST MESSAGE timestamp, falling back
    /// to `created_at` only for message-less sessions. Pruning by
    /// `created_at` once deleted an actively-used session that was merely
    /// old; measuring by last activity keeps every session the user still
    /// touches while still clearing truly stale ones.
    ///
    /// Also flips ghost runs: a session can stay marked `Running` forever
    /// when its run was killed by an app quit (the runner's "always mark
    /// idle" path never executed). Sessions Running with no activity for
    /// 30 minutes are reset to Stopped; the runner re-marks its own
    /// transitions, and a real long run is re-marked Running/Idle at its
    /// boundaries anyway.
    pub fn prune_expired(
        &mut self,
        threshold: chrono::DateTime<chrono::Utc>,
        live_running: &std::collections::HashSet<String>,
    ) -> usize {
        let now = chrono::Utc::now();
        fn last_activity(s: &SessionEntry) -> chrono::DateTime<chrono::Utc> {
            s.messages
                .iter()
                .filter_map(|m| m.get("timestamp").and_then(|v| v.as_str()))
                .filter_map(|ts| chrono::DateTime::parse_from_rfc3339(ts).ok())
                .map(|dt| dt.with_timezone(&chrono::Utc))
                .max()
                .unwrap_or(s.created_at)
        }
        let mut ghosts = 0;
        for (id, entry) in self.sessions.iter_mut() {
            // Ghost flip: a session whose last *persisted* message is older
            // than 30 minutes was assumed dead. That assumption is wrong for
            // any long run — the assistant message is only written when the
            // run ends — and flipping it made the next Stop a no-op
            // (`stop_session` early-returned "already_stopped"), so the user
            // lost control of a running task (round5 P0-8). Only flip when
            // the agent is genuinely not running.
            if entry.status == SessionStatus::Running
                && !live_running.contains(id)
                && (now - last_activity(entry)).num_seconds() > 30 * 60
            {
                entry.status = SessionStatus::Stopped;
                entry.info.status = "stopped".to_string();
                ghosts += 1;
            }
        }
        let ids: Vec<String> = self
            .sessions
            .iter()
            .filter(|(_, s)| {
                s.status != SessionStatus::Running
                    && s.status != SessionStatus::Waiting
                    && last_activity(s) < threshold
            })
            .map(|(id, _)| id.clone())
            .collect();
        for id in &ids {
            let json = self
                .sessions
                .get(id)
                .and_then(|e| serde_json::to_value(e).ok());
            self.archive_entry(id, json);
            self.sessions.remove(id);
        }
        if !ids.is_empty() || ghosts > 0 {
            self.save();
        }
        ids.len()
    }

    /// Create a store that auto-loads from and saves to the given file path.
    pub fn persisted(path: PathBuf) -> Self {
        let mut store = SessionStore {
            sessions: HashMap::new(),
            path: Some(path.clone()),
            max_sessions: 0,
            persist_cache: std::sync::Mutex::new(HashMap::new()),
            writer: Some(PersistWriter::spawn()),
        };
        if let Ok(data) = std::fs::read_to_string(&path) {
            if let Ok(sessions) = serde_json::from_str::<HashMap<String, SessionEntry>>(&data) {
                store.sessions = sessions;
            }
        }
        // Recover: reset any Running sessions left from a previous crash
        let mut recovered = 0;
        for entry in store.sessions.values_mut() {
            if entry.status == SessionStatus::Running {
                entry.status = SessionStatus::Stopped;
                entry.info.status = "stopped".to_string();
                recovered += 1;
            }
        }
        if recovered > 0 {
            store.save_to_disk();
        }
        store
    }

    /// Persist to disk. Call this after external mutations (e.g. pushing messages).
    pub fn save(&self) {
        self.save_to_disk();
    }

    /// Block until the background persist worker has drained (bounded by
    /// `timeout`). Used on graceful exit so the final session snapshot is
    /// on disk before the process terminates.
    pub fn wait_persisted(&self, timeout: std::time::Duration) {
        if let Some(ref w) = self.writer {
            let deadline = std::time::Instant::now() + timeout;
            while w.pending.load(Ordering::SeqCst) > 0 && std::time::Instant::now() < deadline {
                std::thread::sleep(std::time::Duration::from_millis(20));
            }
        }
    }

    /// Reload only when the file on disk is newer than our own last
    /// read/write.
    ///
    /// The desktop app writes the whole file from its in-memory map, and
    /// several processes can share one data root (webui, TUI, CLI, the
    /// platform bridge). `list_sessions` used to call `reload()`
    /// unconditionally, which re-parsed the entire file on every sidebar
    /// refresh; removing it entirely would have made a concurrently running
    /// webui's new sessions invisible AND let the next full-file write erase
    /// them (review finding A-F5). One `stat` per refresh is the cheap middle
    /// ground.
    pub fn reload_if_stale(&mut self) -> bool {
        let Some(ref path) = self.path else {
            return false;
        };
        if let Some(ref w) = self.writer {
            if w.pending.load(Ordering::SeqCst) > 0 {
                return false;
            }
        }
        let Ok(meta) = std::fs::metadata(path) else {
            return false;
        };
        let Ok(mtime) = meta.modified() else {
            return false;
        };
        let known = self
            .writer
            .as_ref()
            .and_then(|w| *w.last_write.lock().unwrap_or_else(|e| e.into_inner()));
        if known.is_some_and(|k| mtime <= k) {
            return false;
        }
        self.reload();
        true
    }

    /// Reload sessions from disk. Useful when another process or adapter
    /// (e.g. platform bridge) has written to the same persistence file.
    /// Skipped while a background write is in flight — the on-disk file is
    /// older than memory in that window and reloading would clobber the
    /// newer in-memory state.
    pub fn reload(&mut self) {
        if let Some(ref w) = self.writer {
            if w.pending.load(Ordering::SeqCst) > 0 {
                return;
            }
        }
        if let Some(ref path) = self.path {
            if let Ok(meta) = std::fs::metadata(path) {
                if let Ok(mtime) = meta.modified() {
                    if let Some(ref w) = self.writer {
                        *w.last_write.lock().unwrap_or_else(|e| e.into_inner()) = Some(mtime);
                    }
                }
            }
            if let Ok(data) = std::fs::read_to_string(path) {
                if let Ok(sessions) = serde_json::from_str::<HashMap<String, SessionEntry>>(&data) {
                    self.sessions = sessions;
                    // The cache holds serializations of the *previous*
                    // in-memory state. Keeping them could write that older
                    // content back over what we just loaded (same revision,
                    // different bytes) — drop it.
                    self.persist_cache
                        .lock()
                        .unwrap_or_else(|e| e.into_inner())
                        .clear();
                }
            }
        }
    }

    /// Save all sessions to disk atomically (write temp file, then rename).
    /// When a background writer is present the snapshot is submitted to it
    /// and the disk write happens off the caller's thread; otherwise the
    /// write is synchronous (stores without a path, e.g. in tests).
    fn save_to_disk(&self) {
        let Some(ref path) = self.path else { return };
        let Some(ref w) = self.writer else {
            // No background writer (tests): synchronous full snapshot.
            if let Ok(json) = serde_json::to_string(&self.sessions) {
                Self::write_atomic(path, &json);
            }
            return;
        };
        // Incremental snapshot: re-serialize only entries whose fingerprint
        // changed; everything else shares its cached JSON via Arc. Removed
        // sessions simply drop out of the snapshot/cache.
        let mut cache = self.persist_cache.lock().unwrap_or_else(|e| e.into_inner());
        let mut snapshot: SessionSnapshot = HashMap::with_capacity(self.sessions.len());
        for (id, entry) in &self.sessions {
            let fp = entry_fingerprint(entry);
            let cached: Option<Arc<String>> = cache.get(id).and_then(|(cfp, json)| {
                if *cfp == fp {
                    Some(Arc::clone(json))
                } else {
                    None
                }
            });
            let json = match cached {
                Some(json) => json,
                None => {
                    Arc::new(serde_json::to_string(entry).unwrap_or_else(|_| "null".to_string()))
                }
            };
            snapshot.insert(id.clone(), json);
        }
        *cache = snapshot
            .iter()
            .map(|(k, v)| {
                (
                    k.clone(),
                    (entry_fingerprint(&self.sessions[k]), Arc::clone(v)),
                )
            })
            .collect();
        w.submit(path.clone(), snapshot);
    }

    /// Atomic tmp-file + rename write. Errors are best-effort by design:
    /// a failed persist must never crash the app mid-conversation.
    fn write_atomic(path: &std::path::Path, json: &str) {
        if let Some(parent) = path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        let tmp = path.with_extension("tmp");
        if std::fs::write(&tmp, json).is_ok() {
            let _ = std::fs::rename(&tmp, path);
        }
    }

    pub fn create(&mut self, name: &str) -> SessionInfo {
        self.create_with_project(name, None, None, None)
    }

    pub fn create_with_project(
        &mut self,
        name: &str,
        project_id: Option<&str>,
        project_name: Option<&str>,
        working_dir: Option<&str>,
    ) -> SessionInfo {
        let id = uuid::Uuid::new_v4().to_string();
        self.create_with_id_project(&id, name, project_id, project_name, working_dir);
        let info = self.sessions.get(&id).unwrap().info.clone();
        self.save_to_disk();
        info
    }

    pub fn create_with_id(&mut self, id: &str, name: &str) {
        self.create_with_id_project(id, name, None, None, None);
    }

    pub fn create_with_id_project(
        &mut self,
        id: &str,
        name: &str,
        project_id: Option<&str>,
        project_name: Option<&str>,
        working_dir: Option<&str>,
    ) {
        let now = chrono::Utc::now();
        let info = SessionInfo {
            id: id.to_string(),
            name: name.to_string(),
            created_at: now.to_rfc3339(),
            status: "idle".to_string(),
            message_count: 0,
            project_id: project_id.map(|s| s.to_string()),
            project_name: project_name.map(|s| s.to_string()),
            working_dir: working_dir.map(|s| s.to_string()),
        };
        self.sessions.insert(
            id.to_string(),
            SessionEntry {
                info,
                status: SessionStatus::Idle,
                messages: Vec::new(),
                created_at: now,
                project_id: project_id.map(|s| s.to_string()),
                working_dir: working_dir.map(|s| s.to_string()),
                todos: Vec::new(),
                pending_ask_user: None,
                revision: 0,
            },
        );
    }

    pub fn has_session(&self, id: &str) -> bool {
        self.sessions.contains_key(id)
    }

    pub fn rename(&mut self, id: &str, new_name: &str) -> bool {
        if let Some(entry) = self.sessions.get_mut(id) {
            entry.info.name = new_name.to_string();
            self.save_to_disk();
            true
        } else {
            false
        }
    }

    pub fn get(&self, id: &str) -> Option<&SessionEntry> {
        self.sessions.get(id)
    }

    /// Mutable access. Hands out `&mut` and therefore records a revision
    /// bump: any caller that mutates the entry in place (compaction folding
    /// content, checkpoint recovery, todo updates) invalidates the persist
    /// cache even when no message was added or removed.
    pub fn get_mut(&mut self, id: &str) -> Option<&mut SessionEntry> {
        // `#[allow]`: clippy suggests `inspect`, which only hands out `&`.
        #[allow(clippy::manual_inspect)]
        self.sessions.get_mut(id).map(|e| {
            e.revision = e.revision.wrapping_add(1);
            e
        })
    }

    pub fn delete(&mut self, id: &str) -> bool {
        let removed = self.sessions.remove(id).is_some();
        if removed {
            self.save_to_disk();
        }
        removed
    }

    pub fn list(&self) -> Vec<SessionInfo> {
        let mut list: Vec<SessionInfo> = self
            .sessions
            .values()
            .map(|entry| SessionInfo {
                id: entry.info.id.clone(),
                name: entry.info.name.clone(),
                created_at: entry.info.created_at.clone(),
                status: entry.status.as_str().to_string(),
                message_count: entry.messages.len(),
                project_id: entry.info.project_id.clone(),
                project_name: entry.info.project_name.clone(),
                working_dir: entry.info.working_dir.clone(),
            })
            .collect();
        list.sort_by(|a, b| b.created_at.cmp(&a.created_at));
        list
    }

    pub fn set_project_id(&mut self, id: &str, project_id: Option<&str>) -> bool {
        if let Some(entry) = self.sessions.get_mut(id) {
            entry.project_id = project_id.map(|s| s.to_string());
            entry.info.project_id = project_id.map(|s| s.to_string());
            self.save_to_disk();
            true
        } else {
            false
        }
    }

    /// Move a session to a project, updating both project_id and working_dir.
    /// The runner prefers `session.working_dir` over `project.root_path`, so
    /// moving a session without updating working_dir leaves the agent in the
    /// old project directory.
    pub fn move_to_project(&mut self, id: &str, project_id: &str, working_dir: &str) -> bool {
        if let Some(entry) = self.sessions.get_mut(id) {
            entry.project_id = Some(project_id.to_string());
            entry.info.project_id = Some(project_id.to_string());
            entry.working_dir = Some(working_dir.to_string());
            entry.info.working_dir = Some(working_dir.to_string());
            self.save_to_disk();
            true
        } else {
            false
        }
    }

    pub fn clear_project_sessions(&mut self, project_id: &str) {
        let mut changed = false;
        for entry in self.sessions.values_mut() {
            if entry.project_id.as_deref() == Some(project_id) {
                entry.project_id = None;
                entry.info.project_id = None;
                entry.info.project_name = None;
                changed = true;
            }
        }
        if changed {
            self.save_to_disk();
        }
    }

    pub fn count_by_project_id(&self, project_id: &str) -> usize {
        self.sessions
            .values()
            .filter(|e| e.project_id.as_deref() == Some(project_id))
            .count()
    }
}

#[cfg(test)]
mod incremental_persist_tests {
    use super::*;

    #[test]
    fn waiting_status_and_pending_question_roundtrip() {
        let mut store = SessionStore::new();
        let info = store.create("waiting");
        {
            let entry = store.get_mut(&info.id).expect("entry");
            entry.status = SessionStatus::Waiting;
            entry.pending_ask_user = Some(PendingAskUserRecord {
                tool_use_id: "call_ask".into(),
                tool_call_id: "tc_ask".into(),
                question: "Pick one".into(),
                candidates: vec!["a".into(), "b".into()],
                asked_at: chrono::Utc::now().to_rfc3339(),
            });
        }
        assert_eq!(store.list()[0].status, "waiting");
        let json = serde_json::to_string(store.get(&info.id).unwrap()).unwrap();
        let back: SessionEntry = serde_json::from_str(&json).unwrap();
        assert_eq!(back.status, SessionStatus::Waiting);
        let pending = back.pending_ask_user.expect("pending question persisted");
        assert_eq!(pending.question, "Pick one");
        assert_eq!(pending.candidates.len(), 2);
        assert_eq!(back.status.as_str(), "waiting");
    }

    #[test]
    fn snapshot_reserializes_only_changed_entries() {
        let dir = std::env::temp_dir().join(format!("oz-sess-incr-{}", std::process::id()));
        let _ = std::fs::create_dir_all(&dir);
        let path = dir.join("sessions.json");
        let mut store = SessionStore::persisted(path.clone());
        let mut e1 = SessionEntry {
            info: SessionInfo {
                id: "s1".into(),
                name: "one".into(),
                created_at: chrono::Utc::now().to_rfc3339(),
                status: "idle".into(),
                message_count: 1,
                project_id: None,
                project_name: None,
                working_dir: None,
            },
            status: SessionStatus::Idle,
            messages: vec![serde_json::json!({"role": "user", "content": "hi"})],
            created_at: chrono::Utc::now(),
            project_id: None,
            working_dir: None,
            todos: vec![],
            pending_ask_user: None,
            revision: 0,
        };
        let e2 = SessionEntry {
            info: SessionInfo {
                id: "s2".into(),
                name: "two".into(),
                ..e1.info.clone()
            },
            ..e1.clone()
        };
        store.sessions.insert("s1".into(), e1.clone());
        store.sessions.insert("s2".into(), e2.clone());
        store.save();
        store.wait_persisted(std::time::Duration::from_secs(2));

        // cache holds both, sharing Arcs
        {
            let cache = store
                .persist_cache
                .lock()
                .unwrap_or_else(|e| e.into_inner());
            assert_eq!(cache.len(), 2);
        }
        let arc_s1_before = {
            let cache = store
                .persist_cache
                .lock()
                .unwrap_or_else(|e| e.into_inner());
            Arc::clone(&cache.get("s1").unwrap().1)
        };

        // mutate s1 only: bump message count
        e1.messages.push(serde_json::json!({"role": "assistant"}));
        e1.info.message_count = 2;
        store.sessions.insert("s1".into(), e1);
        store.save();
        store.wait_persisted(std::time::Duration::from_secs(2));

        let arc_s1_after = {
            let cache = store
                .persist_cache
                .lock()
                .unwrap_or_else(|e| e.into_inner());
            Arc::clone(&cache.get("s1").unwrap().1)
        };
        assert!(
            !Arc::ptr_eq(&arc_s1_before, &arc_s1_after),
            "changed entry must be re-serialized"
        );

        // persisted file parses and contains both sessions
        let data = std::fs::read_to_string(&path).unwrap();
        let parsed: serde_json::Value = serde_json::from_str(&data).unwrap();
        assert!(parsed.get("s1").is_some() && parsed.get("s2").is_some());
        assert_eq!(
            parsed["s1"]["messages"].as_array().unwrap().len(),
            2,
            "file must reflect the mutated entry"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }
}

#[cfg(test)]
mod fingerprint_regression_tests {
    use super::*;

    fn entry(id: &str, content: &str) -> SessionEntry {
        SessionEntry {
            info: SessionInfo {
                id: id.into(),
                name: id.into(),
                created_at: chrono::Utc::now().to_rfc3339(),
                status: "idle".into(),
                message_count: 1,
                project_id: None,
                project_name: None,
                working_dir: None,
            },
            status: SessionStatus::Idle,
            messages: vec![serde_json::json!({"role": "user", "content": content})],
            created_at: chrono::Utc::now(),
            project_id: None,
            working_dir: None,
            todos: vec![],
            pending_ask_user: None,
            revision: 0,
        }
    }

    /// round5 P0-3 regression: `/compact` folds content IN PLACE — the
    /// message count does not change, so the old count-only fingerprint
    /// decided the cached (pre-compaction) JSON was still valid and wrote it
    /// back. The user saw "compressed" and got the old context after a
    /// restart. Mutating through `get_mut` must invalidate the cache.
    #[test]
    fn in_place_content_change_is_persisted() {
        let dir = std::env::temp_dir().join(format!("oz-fp-inplace-{}", std::process::id()));
        let _ = std::fs::create_dir_all(&dir);
        let path = dir.join("sessions.json");
        let mut store = SessionStore::persisted(path.clone());
        store.sessions.insert("s1".into(), entry("s1", "BEFORE"));
        store.save();
        store.wait_persisted(std::time::Duration::from_secs(2));

        // Same message count, different content — exactly what compaction does.
        {
            let e = store.get_mut("s1").expect("session present");
            e.messages[0]["content"] = serde_json::json!("AFTER");
        }
        store.save();
        store.wait_persisted(std::time::Duration::from_secs(2));

        let parsed: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(
            parsed["s1"]["messages"][0]["content"], "AFTER",
            "in-place content change was not persisted (stale cache written back)"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A pure read must not force a rewrite of the cached JSON.
    #[test]
    fn read_only_access_does_not_invalidate_the_cache() {
        let dir = std::env::temp_dir().join(format!("oz-fp-read-{}", std::process::id()));
        let _ = std::fs::create_dir_all(&dir);
        let path = dir.join("sessions.json");
        let mut store = SessionStore::persisted(path.clone());
        store.sessions.insert("s1".into(), entry("s1", "same"));
        store.save();
        store.wait_persisted(std::time::Duration::from_secs(2));
        let before = {
            let cache = store.persist_cache.lock().unwrap();
            Arc::clone(&cache.get("s1").unwrap().1)
        };
        assert!(store.get("s1").is_some());
        store.save();
        store.wait_persisted(std::time::Duration::from_secs(2));
        let after = {
            let cache = store.persist_cache.lock().unwrap();
            Arc::clone(&cache.get("s1").unwrap().1)
        };
        assert!(
            Arc::ptr_eq(&before, &after),
            "a read must not invalidate the persist cache"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Review finding A-F5: removing the unconditional `reload()` from the
    /// stats/list commands must not make another process's writes invisible
    /// (and then erasable by our next full-file save). `reload_if_stale()`
    /// picks them up with a single `stat`.
    #[test]
    fn reload_if_stale_picks_up_another_process_write() {
        let dir = std::env::temp_dir().join(format!("oz-fp-stale-{}", std::process::id()));
        let _ = std::fs::create_dir_all(&dir);
        let path = dir.join("sessions.json");
        let mut store = SessionStore::persisted(path.clone());
        store.sessions.insert("s1".into(), entry("s1", "mine"));
        store.save();
        store.wait_persisted(std::time::Duration::from_secs(2));
        // Our own write must NOT look like a foreign change.
        assert!(
            !store.reload_if_stale(),
            "our own write must not trigger a reload"
        );

        // Another process (webui/TUI/CLI) appends a session.
        std::thread::sleep(std::time::Duration::from_millis(20));
        let mut foreign: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        foreign["s2"] = serde_json::to_value(entry("s2", "from another process")).unwrap();
        std::fs::write(&path, serde_json::to_string(&foreign).unwrap()).unwrap();

        assert!(store.reload_if_stale(), "a foreign write must be detected");
        assert!(
            store.get("s2").is_some(),
            "the other process's session must be visible (otherwise the next save erases it)"
        );
        // And it must be idempotent: nothing changed since.
        assert!(!store.reload_if_stale());
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// `reload()` must drop the persist cache: it holds serializations of the
    /// previous in-memory state and could otherwise overwrite what was just
    /// loaded from disk.
    #[test]
    fn reload_drops_the_persist_cache() {
        let dir = std::env::temp_dir().join(format!("oz-fp-reload-{}", std::process::id()));
        let _ = std::fs::create_dir_all(&dir);
        let path = dir.join("sessions.json");
        let mut store = SessionStore::persisted(path.clone());
        store.sessions.insert("s1".into(), entry("s1", "disk"));
        store.save();
        store.wait_persisted(std::time::Duration::from_secs(2));
        assert!(!store.persist_cache.lock().unwrap().is_empty());
        store.reload();
        assert!(
            store.persist_cache.lock().unwrap().is_empty(),
            "reload must clear the persist cache"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }
}
