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
    /// Worker by default; `Butler` sessions own a task graph.
    #[serde(default)]
    pub role: oz_core_types::session_role::SessionRole,
    /// How many leading messages are outside the model's context window.
    #[serde(default)]
    pub archived_before: usize,
    /// One-line handoff describing the archived epoch.
    #[serde(default)]
    pub epoch_summary: Option<String>,
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
    /// Worker by default; `Butler` sessions own a task graph.
    #[serde(default)]
    pub role: oz_core_types::session_role::SessionRole,
    /// Model actually used by the last run (for worker inheritance).
    #[serde(default)]
    pub model: Option<String>,
    /// Whether that model was the user's choice or a resolved default.
    #[serde(default)]
    pub model_source: oz_core_types::session_role::ModelSource,
    /// Large, idle sessions are moved out of sessions.json into one file per
    /// session under sessions-archive/. They stay in memory (UI list works)
    /// but are not rewritten on every save.
    #[serde(default)]
    pub archived: bool,
    #[serde(default)]
    pub archived_at: Option<chrono::DateTime<chrono::Utc>>,
}

/// One archive directory. The old `sessions_archive` spelling is read for
/// migration, but all new writes use this one.
const SESSIONS_ARCHIVE_DIR: &str = "sessions-archive";

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
    /// Runtime read/parse failure: do not overwrite a file we could not read.
    degraded: bool,
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
                        if let Err(e) = SessionStore::write_atomic(&path, &json) {
                            tracing::error!("[sessions] persist failed {}: {e}", path.display());
                        }
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
            degraded: false,
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
                parent.join(SESSIONS_ARCHIVE_DIR)
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
            // Pruning is a deletion, not an archive: keeping a copy here made
            // the next load_archives resurrect the pruned session.
            self.delete(id);
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
            degraded: false,
        };
        match std::fs::read_to_string(&path) {
            Ok(data) => match serde_json::from_str::<HashMap<String, SessionEntry>>(&data) {
                Ok(sessions) => store.sessions = sessions,
                Err(e) => {
                    // NEVER start with an empty map and then overwrite the
                    // file: one unknown SessionStatus or corrupt entry would
                    // permanently erase every conversation. Move the bad file
                    // aside and let the new store start empty.
                    // Millisecond + pid suffix: two corrupt files in the same
                    // second must not overwrite each other (the old name had
                    // second granularity and silently destroyed the first
                    // backup — E2E-019/E2E-020).
                    let backup = path.with_extension(format!(
                        "corrupt-{}-{}",
                        chrono::Utc::now().format("%Y%m%dT%H%M%S%3fZ"),
                        std::process::id()
                    ));
                    let moved = std::fs::rename(&path, &backup).is_ok();
                    if !moved {
                        store.degraded = true;
                    }
                    tracing::error!(
                        "[sessions] parse failed ({e}); preserved={} backup={}",
                        moved,
                        backup.display()
                    );
                }
            },
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => {
                let backup = path.with_extension(format!(
                    "corrupt-{}-{}",
                    chrono::Utc::now().format("%Y%m%dT%H%M%S%3fZ"),
                    std::process::id()
                ));
                let moved = std::fs::rename(&path, &backup).is_ok();
                if !moved {
                    store.degraded = true;
                }
                tracing::error!(
                    "[sessions] cannot read {}: {e}; preserved={} backup={}",
                    path.display(),
                    moved,
                    backup.display()
                );
            }
        }
        // Archived sessions live one-per-file; merge them so history and the
        // session list survive the main-file compaction.
        store.load_archives();
        // Recover: reset any Running sessions left from a previous crash
        let mut recovered = 0;
        for entry in store.sessions.values_mut() {
            if entry.status == SessionStatus::Running {
                entry.status = SessionStatus::Stopped;
                entry.info.status = "stopped".to_string();
                recovered += 1;
            }
        }
        let archived_now = store.archive_inactive(16 * 1024 * 1024);
        if recovered > 0 || archived_now > 0 {
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
        let Some(path) = self.path.clone() else {
            return;
        };
        let data = match std::fs::read_to_string(&path) {
            Ok(d) => d,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return,
            Err(e) => {
                self.degraded = true;
                tracing::error!("[sessions] reload failed ({}): {e}", path.display());
                return;
            }
        };
        match serde_json::from_str::<HashMap<String, SessionEntry>>(&data) {
            Ok(sessions) => {
                self.sessions = sessions;
                self.load_archives();
                self.degraded = false;
                if let Ok(meta) = std::fs::metadata(&path) {
                    if let Ok(mtime) = meta.modified() {
                        if let Some(ref w) = self.writer {
                            *w.last_write.lock().unwrap_or_else(|e| e.into_inner()) = Some(mtime);
                        }
                    }
                }
                self.persist_cache
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .clear();
            }
            Err(e) => {
                self.degraded = true;
                tracing::error!("[sessions] reload parse failed ({}): {e}", path.display());
            }
        }
    }

    fn archive_dir(&self) -> Option<PathBuf> {
        let path = self.path.as_ref()?;
        Some(path.parent()?.join(SESSIONS_ARCHIVE_DIR))
    }

    /// Merge one-file-per-session archives into memory. Main-file entries win:
    /// an archive copy is stale once the session was unarchived.
    fn load_archives(&mut self) {
        let Some(dir) = self.archive_dir() else {
            return;
        };
        let Ok(entries) = std::fs::read_dir(&dir) else {
            return;
        };
        for e in entries.flatten() {
            let path = e.path();
            if path.extension().and_then(|x| x.to_str()) != Some("json") {
                continue;
            }
            let Ok(raw) = std::fs::read_to_string(&path) else {
                continue;
            };
            let Ok(mut entry) = serde_json::from_str::<SessionEntry>(&raw) else {
                tracing::warn!("[sessions] unreadable archive {}", path.display());
                continue;
            };
            entry.archived = true;
            self.sessions.entry(entry.info.id.clone()).or_insert(entry);
        }
    }

    /// Split the oldest idle sessions out of the monolithic sessions.json once
    /// it exceeds `max_bytes`. They remain in memory, so the UI still lists
    /// them; only the hot file stops carrying every old message.
    pub fn archive_inactive(&mut self, max_bytes: usize) -> usize {
        let Some(dir) = self.archive_dir() else {
            return 0;
        };
        let active: HashMap<_, _> = self
            .sessions
            .iter()
            .filter(|(_, e)| !e.archived)
            .map(|(k, v)| (k.clone(), v))
            .collect();
        let Ok(initial) = serde_json::to_vec(&active) else {
            return 0;
        };
        let mut active_size = initial.len();
        if active_size <= max_bytes {
            return 0;
        }
        let _ = std::fs::create_dir_all(&dir);
        let mut candidates: Vec<(String, String)> = self
            .sessions
            .iter()
            .filter(|(_, e)| {
                !e.archived
                    && !matches!(e.status, SessionStatus::Running | SessionStatus::Waiting)
                    && e.pending_ask_user.is_none()
            })
            .map(|(id, e)| (id.clone(), e.created_at.to_rfc3339()))
            .collect();
        candidates.sort_by(|a, b| a.1.cmp(&b.1));
        let mut written = 0;
        for (id, _) in candidates {
            if active_size <= max_bytes {
                break;
            }
            let Some(entry) = self.sessions.get_mut(&id) else {
                continue;
            };
            entry.archived = true;
            entry.archived_at = Some(chrono::Utc::now());
            let Ok(json) = serde_json::to_string(entry) else {
                entry.archived = false;
                entry.archived_at = None;
                continue;
            };
            let path = dir.join(format!("{id}.json"));
            if let Err(e) = Self::write_atomic(&path, &json) {
                entry.archived = false;
                entry.archived_at = None;
                tracing::warn!("[sessions] archive write failed {}: {e}", path.display());
                continue;
            }
            active_size = active_size.saturating_sub(json.len());
            written += 1;
        }
        if written > 0 {
            self.save_to_disk();
            tracing::info!(
                "[sessions] archived {written} idle session(s); main file is no longer {} bytes",
                initial.len()
            );
        }
        written
    }

    /// Save all sessions to disk atomically (write temp file, then rename).
    /// When a background writer is present the snapshot is submitted to it
    /// and the disk write happens off the caller's thread; otherwise the
    /// write is synchronous (stores without a path, e.g. in tests).
    fn save_to_disk(&self) {
        if self.degraded {
            tracing::error!("[sessions] persist suppressed: store is degraded (unreadable file)");
            return;
        }
        let Some(ref path) = self.path else { return };
        let Some(ref w) = self.writer else {
            // No background writer (tests): synchronous snapshot of active
            // sessions only; archived sessions have their own files.
            let active: HashMap<_, _> = self
                .sessions
                .iter()
                .filter(|(_, e)| !e.archived)
                .map(|(k, v)| (k.clone(), v))
                .collect();
            if let Ok(json) = serde_json::to_string(&active) {
                if let Err(e) = Self::write_atomic(path, &json) {
                    tracing::error!("[sessions] persist failed {}: {e}", path.display());
                }
            }
            return;
        };
        // Incremental snapshot: re-serialize only entries whose fingerprint
        // changed; everything else shares its cached JSON via Arc. Removed
        // sessions simply drop out of the snapshot/cache.
        let mut cache = self.persist_cache.lock().unwrap_or_else(|e| e.into_inner());
        let mut snapshot: SessionSnapshot = HashMap::with_capacity(self.sessions.len());
        for (id, entry) in self.sessions.iter().filter(|(_, e)| !e.archived) {
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

    /// Atomic tmp-file + rename write. A failed persist must not crash the
    /// app mid-conversation, but it must be visible: silently returning made
    /// the UI look saved and lost the messages on restart.
    fn write_atomic(path: &std::path::Path, json: &str) -> std::io::Result<()> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        // Unique temp name: two processes sharing a data root must not write
        // the same sessions.tmp and rename a spliced file.
        let tmp = path.with_extension(format!(
            "tmp-{}-{}",
            std::process::id(),
            uuid::Uuid::new_v4()
        ));
        std::fs::write(&tmp, json)?;
        if let Err(e) = std::fs::rename(&tmp, path) {
            let _ = std::fs::remove_file(&tmp);
            return Err(e);
        }
        Ok(())
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
            role: oz_core_types::session_role::SessionRole::Worker,
            archived_before: 0,
            epoch_summary: None,
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
                role: oz_core_types::session_role::SessionRole::Worker,
                model: None,
                model_source: oz_core_types::session_role::ModelSource::Default,
                archived: false,
                archived_at: None,
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
            if e.archived {
                // Do NOT unlink the archive here: it is the only durable copy
                // until the next successful main-file save. Main entries win
                // over a stale archive on load, and delete() removes both.
                e.archived = false;
                e.archived_at = None;
            }
            e.revision = e.revision.wrapping_add(1);
            e
        })
    }

    pub fn delete(&mut self, id: &str) -> bool {
        if let Some(dir) = self.archive_dir() {
            let _ = std::fs::remove_file(dir.join(format!("{id}.json")));
            let _ = std::fs::remove_file(
                dir.with_file_name("sessions_archive")
                    .join(format!("{id}.json")),
            );
        }
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
                role: entry.role,
                archived_before: entry.info.archived_before,
                epoch_summary: entry.info.epoch_summary.clone(),
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
                role: oz_core_types::session_role::SessionRole::Worker,
                archived_before: 0,
                epoch_summary: None,
            },
            status: SessionStatus::Idle,
            messages: vec![serde_json::json!({"role": "user", "content": "hi"})],
            created_at: chrono::Utc::now(),
            project_id: None,
            working_dir: None,
            todos: vec![],
            pending_ask_user: None,
            revision: 0,
            role: oz_core_types::session_role::SessionRole::Worker,
            model: None,
            model_source: oz_core_types::session_role::ModelSource::Default,
            archived: false,
            archived_at: None,
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
                role: oz_core_types::session_role::SessionRole::Worker,
                archived_before: 0,
                epoch_summary: None,
            },
            status: SessionStatus::Idle,
            messages: vec![serde_json::json!({"role": "user", "content": content})],
            created_at: chrono::Utc::now(),
            project_id: None,
            working_dir: None,
            todos: vec![],
            pending_ask_user: None,
            revision: 0,
            role: oz_core_types::session_role::SessionRole::Worker,
            model: None,
            model_source: oz_core_types::session_role::ModelSource::Default,
            archived: false,
            archived_at: None,
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

    #[test]
    fn corrupt_sessions_file_is_preserved_not_overwritten() {
        let dir = std::env::temp_dir().join(format!("oz-fp-corrupt-{}", std::process::id()));
        let _ = std::fs::create_dir_all(&dir);
        let path = dir.join("sessions.json");
        std::fs::write(&path, "{not json").unwrap();
        let store = SessionStore::persisted(path.clone());
        assert!(store.sessions.is_empty());
        assert!(!path.exists(), "bad file must not stay at the live path");
        let backups: Vec<_> = std::fs::read_dir(&dir)
            .unwrap()
            .filter_map(|e| e.ok())
            .filter(|e| e.file_name().to_string_lossy().contains("corrupt-"))
            .collect();
        assert_eq!(backups.len(), 1, "corrupt file must be preserved");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn runtime_reload_parse_failure_blocks_overwrite() {
        let dir = std::env::temp_dir().join(format!("oz-fp-degraded-{}", std::process::id()));
        let _ = std::fs::create_dir_all(&dir);
        let path = dir.join("sessions.json");
        let mut store = SessionStore::persisted(path.clone());
        store.sessions.insert("s1".into(), entry("s1", "good"));
        store.save();
        store.wait_persisted(std::time::Duration::from_secs(2));
        std::thread::sleep(std::time::Duration::from_millis(20));
        std::fs::write(&path, "{broken").unwrap();
        store.reload();
        assert!(store.get("s1").is_some(), "in-memory data must survive");
        assert!(store.degraded, "store must stop writing after a bad reload");
        store.save();
        store.wait_persisted(std::time::Duration::from_millis(300));
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "{broken");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn idle_sessions_archive_and_reload_without_losing_history() {
        let dir = std::env::temp_dir().join(format!("oz-fp-archive-{}", std::process::id()));
        let _ = std::fs::create_dir_all(&dir);
        let path = dir.join("sessions.json");
        let mut store = SessionStore::persisted(path.clone());
        for i in 0..3 {
            let id = format!("s{i}");
            store.sessions.insert(id.clone(), entry(&id, "old message"));
        }
        let archived = store.archive_inactive(10);
        assert_eq!(archived, 3, "tiny budget must archive all idle sessions");
        store.wait_persisted(std::time::Duration::from_secs(2));
        let main: HashMap<String, SessionEntry> =
            serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        assert!(main.is_empty(), "archived entries must leave the main file");
        drop(store);
        let reloaded = SessionStore::persisted(path.clone());
        assert_eq!(reloaded.sessions.len(), 3);
        assert!(reloaded
            .sessions
            .values()
            .all(|e| e.archived && e.messages.len() == 1));
        let _ = std::fs::remove_dir_all(&dir);
    }
}
