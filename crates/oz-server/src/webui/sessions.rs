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
    Stopped,
}

impl SessionStatus {
    pub fn as_str(&self) -> &'static str {
        match self {
            SessionStatus::Idle => "idle",
            SessionStatus::Running => "running",
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
}

/// In-memory session store with optional file persistence.
pub struct SessionStore {
    sessions: HashMap<String, SessionEntry>,
    path: Option<PathBuf>,
    max_sessions: usize,
    writer: Option<PersistWriter>,
}

/// Background persistence writer. `save()` snapshots the session map and
/// hands it to a dedicated thread which serialises and writes it with
/// "latest wins" coalescing: a burst of saves during a long agent run
/// collapses into a single disk write, and serialisation never blocks the
/// IPC thread. Stores built with `SessionStore::new()` (no path, e.g. in
/// tests) keep the old synchronous write path.
type PersistPayload = (PathBuf, HashMap<String, SessionEntry>);

struct PersistWriter {
    tx: Sender<PersistPayload>,
    /// Snapshots submitted but not yet written (or discarded). `reload()`
    /// must skip while this is non-zero: the on-disk file is older than
    /// memory in that window and reloading would clobber newer state.
    pending: Arc<AtomicU64>,
}

impl PersistWriter {
    fn spawn() -> Self {
        let (tx, rx) = mpsc::channel::<PersistPayload>();
        let pending = Arc::new(AtomicU64::new(0));
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
                    if let Some((path, sessions)) = latest {
                        if let Ok(json) = serde_json::to_string(&sessions) {
                            SessionStore::write_atomic(&path, &json);
                        }
                        worker_pending.fetch_sub(1, Ordering::SeqCst);
                    }
                }
            })
            .expect("failed to spawn session persist thread");
        PersistWriter { tx, pending }
    }

    fn submit(&self, path: PathBuf, sessions: HashMap<String, SessionEntry>) {
        self.pending.fetch_add(1, Ordering::SeqCst);
        if self.tx.send((path, sessions)).is_err() {
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

    /// Create a store that auto-loads from and saves to the given file path.
    pub fn persisted(path: PathBuf) -> Self {
        let mut store = SessionStore {
            sessions: HashMap::new(),
            path: Some(path.clone()),
            max_sessions: 0,
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
            if let Ok(data) = std::fs::read_to_string(path) {
                if let Ok(sessions) = serde_json::from_str::<HashMap<String, SessionEntry>>(&data) {
                    self.sessions = sessions;
                }
            }
        }
    }

    /// Save all sessions to disk atomically (write temp file, then rename).
    /// When a background writer is present the snapshot is submitted to it
    /// and the disk write happens off the caller's thread; otherwise the
    /// write is synchronous (stores without a path, e.g. in tests).
    fn save_to_disk(&self) {
        if let Some(ref path) = self.path {
            if let Some(ref w) = self.writer {
                w.submit(path.clone(), self.sessions.clone());
            } else if let Ok(json) = serde_json::to_string(&self.sessions) {
                Self::write_atomic(path, &json);
            }
        }
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

    pub fn get_mut(&mut self, id: &str) -> Option<&mut SessionEntry> {
        self.sessions.get_mut(id)
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
