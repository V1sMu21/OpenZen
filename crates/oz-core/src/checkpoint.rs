use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

use oz_core_types::Message;

// ── Direction C: mmap WAL checkpoint ──

/// Magic header for mmap WAL files.
const WAL_MAGIC: &[u8; 4] = b"GAWL";
const WAL_VERSION: u32 = 1;
const WAL_HEADER_SIZE: usize = 8; // magic(4) + version(4)
const WAL_INITIAL_SIZE: usize = 1 << 20; // 1 MB

/// A write-ahead log backed by a memory-mapped file.
/// Records are appended sequentially: [u32 LE length][JSON bytes].
/// The mmap provides zero-copy scanning for resume.
pub struct MmapWal {
    file: std::fs::File,
    map: memmap2::MmapMut,
    /// Current write position (byte offset into the map).
    cursor: usize,
    /// Total mapped capacity.
    capacity: usize,
    path: PathBuf,
}

impl MmapWal {
    /// Create or open a WAL for the given session.
    #[allow(clippy::suspicious_open_options)] // read-modify-write, not truncate
    pub fn open(dir: &Path, session_id: &str) -> std::io::Result<Self> {
        std::fs::create_dir_all(dir)?;
        let safe = sanitize_session_id(session_id);
        let path = dir.join(format!("wal_{safe}.mmap"));

        let file = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .open(&path)?;

        let file_len = file.metadata()?.len() as usize;
        let (map, cursor, capacity) = if file_len == 0 {
            // Fresh file — initialise with header
            file.set_len(WAL_INITIAL_SIZE as u64)?;
            let mut map = unsafe { memmap2::MmapMut::map_mut(&file)? };
            map[..4].copy_from_slice(WAL_MAGIC);
            map[4..8].copy_from_slice(&WAL_VERSION.to_le_bytes());
            (map, WAL_HEADER_SIZE, WAL_INITIAL_SIZE)
        } else {
            // Existing file — validate header, scan to end
            let cap = file_len.max(WAL_INITIAL_SIZE);
            if cap != file_len {
                file.set_len(cap as u64)?;
            }
            let map = unsafe { memmap2::MmapMut::map_mut(&file)? };
            // Validate magic
            if &map[..4] != WAL_MAGIC {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::InvalidData,
                    "invalid WAL magic bytes",
                ));
            }
            let cursor = scan_records(&map[..], WAL_HEADER_SIZE);
            (map, cursor, cap)
        };

        Ok(MmapWal {
            file,
            map,
            cursor,
            capacity,
            path,
        })
    }

    /// Append a checkpoint to the WAL.
    pub fn append(&mut self, cp: &LoopCheckpoint) -> std::io::Result<()> {
        let json = serde_json::to_vec(cp)
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
        self.append_bytes(&json)
    }

    /// Append pre-serialized checkpoint JSON (length-prefixed) to the WAL.
    pub fn append_bytes(&mut self, json: &[u8]) -> std::io::Result<()> {
        let record_len = 4 + json.len(); // length prefix + data

        // Grow if needed
        let needed = self.cursor + record_len;
        if needed > self.capacity {
            let new_cap = (needed.next_power_of_two()).max(self.capacity * 2);
            self.grow(new_cap)?;
        }

        // Write length prefix (little-endian u32)
        let len_bytes = (json.len() as u32).to_le_bytes();
        self.map[self.cursor..self.cursor + 4].copy_from_slice(&len_bytes);
        // Write JSON data
        self.map[self.cursor + 4..self.cursor + record_len].copy_from_slice(json);
        self.cursor += record_len;
        Ok(())
    }

    /// Scan all valid records and return the most recent checkpoint.
    pub fn latest(&self) -> Option<LoopCheckpoint> {
        let data = &self.map[..];
        let mut pos = WAL_HEADER_SIZE;
        let mut latest = None;
        while pos + 4 <= data.len() {
            let len = u32::from_le_bytes(data[pos..pos + 4].try_into().ok()?) as usize;
            pos += 4;
            if pos + len > data.len() {
                break; // incomplete record
            }
            if let Ok(cp) = serde_json::from_slice::<LoopCheckpoint>(&data[pos..pos + len]) {
                latest = Some(cp);
            }
            pos += len;
        }
        latest
    }

    /// Scan and return all valid checkpoints (oldest first).
    pub fn all(&self) -> Vec<LoopCheckpoint> {
        let data = &self.map[..];
        let mut pos = WAL_HEADER_SIZE;
        let mut result = Vec::new();
        while pos + 4 <= data.len() {
            let Ok(len_slice) = data[pos..pos + 4].try_into() else {
                break;
            };
            let len = u32::from_le_bytes(len_slice) as usize;
            pos += 4;
            if pos + len > data.len() {
                break;
            }
            if let Ok(cp) = serde_json::from_slice::<LoopCheckpoint>(&data[pos..pos + len]) {
                result.push(cp);
            }
            pos += len;
        }
        result
    }

    /// The file path of the WAL.
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Force sync the memory map to disk.
    pub fn flush(&self) -> std::io::Result<()> {
        self.map.flush()
    }

    fn grow(&mut self, new_cap: usize) -> std::io::Result<()> {
        // Write current content to disk, then resize
        self.map.flush()?;
        self.file.set_len(new_cap as u64)?;
        let map = unsafe { memmap2::MmapMut::map_mut(&self.file)? };
        self.map = map;
        self.capacity = new_cap;
        Ok(())
    }
}

// ── WAL integration ─────────────────────────────────────────────────
// The JSON-per-turn files remain the compatibility format; the mmap WAL
// is the fast authoritative copy for crash recovery. Handles are cached
// per (dir, session) so checkpoint writes don't reopen/remap per turn,
// and the WAL compacts itself past a record budget so a 7x24 session
// can't grow it without bound (each record is a full snapshot).

const MAX_WAL_RECORDS: usize = 8;

fn wal_cache() -> &'static std::sync::Mutex<std::collections::HashMap<String, MmapWal>> {
    static CACHE: std::sync::OnceLock<
        std::sync::Mutex<std::collections::HashMap<String, MmapWal>>,
    > = std::sync::OnceLock::new();
    CACHE.get_or_init(|| std::sync::Mutex::new(std::collections::HashMap::new()))
}

fn wal_key(dir: &Path, session_id: &str) -> String {
    format!("{}::{}", dir.display(), session_id)
}

/// Append a checkpoint to the session's WAL (compacting first if the
/// record budget is exceeded). Best-effort: a WAL failure never blocks
/// the JSON path.
pub fn append_checkpoint_wal(dir: &Path, session_id: &str, cp: &LoopCheckpoint) {
    let Ok(json) = serde_json::to_vec(cp) else {
        return;
    };
    append_checkpoint_wal_bytes(dir, session_id, &json);
}

fn append_checkpoint_wal_bytes(dir: &Path, session_id: &str, json: &[u8]) {
    let key = wal_key(dir, session_id);
    let mut cache = wal_cache().lock().unwrap_or_else(|e| e.into_inner());
    // Health check the cached handle: a replaced/deleted file underneath
    // us (compaction from another path, user cleanup) invalidates the map.
    if let Some(wal) = cache.get(&key) {
        if !wal.path().exists() {
            cache.remove(&key);
        }
    }
    if cache.get(&key).is_none() {
        match MmapWal::open(dir, session_id) {
            Ok(wal) => {
                cache.insert(key.clone(), wal);
            }
            Err(e) => {
                tracing::warn!("checkpoint WAL open failed for {session_id}: {e}");
                return;
            }
        }
    }
    let needs_compact = cache
        .get(&key)
        .map(|wal| wal.all().len() >= MAX_WAL_RECORDS)
        .unwrap_or(false);
    if needs_compact {
        // Keep only the newest record: drop the handle, remove the file,
        // reopen fresh and re-append the latest snapshot.
        let latest = cache.get(&key).and_then(|wal| wal.latest());
        if let Some(latest) = latest {
            let path = cache.get(&key).map(|w| w.path().to_path_buf());
            cache.remove(&key);
            if let Some(p) = path {
                let _ = std::fs::remove_file(&p);
            }
            match MmapWal::open(dir, session_id) {
                Ok(mut wal) => {
                    if let Err(e) = wal.append(&latest) {
                        tracing::warn!("checkpoint WAL compact-append failed: {e}");
                    }
                    let _ = wal.flush();
                    cache.insert(key.clone(), wal);
                }
                Err(e) => {
                    tracing::warn!("checkpoint WAL reopen after compact failed: {e}");
                    return;
                }
            }
        }
    }
    if let Some(wal) = cache.get_mut(&key) {
        if let Err(e) = wal.append_bytes(json) {
            tracing::warn!("checkpoint WAL append failed for {session_id}: {e}");
            return;
        }
        let _ = wal.flush();
    }
}

/// Read the newest checkpoint from the session's WAL, if one exists.
pub fn read_latest_checkpoint_wal(dir: &Path, session_id: &str) -> Option<LoopCheckpoint> {
    // Reuse the cached handle when present, otherwise open read-only
    // (without inserting) to avoid pinning maps for read-only callers.
    let cache = wal_cache().lock().unwrap_or_else(|e| e.into_inner());
    if let Some(wal) = cache.get(&wal_key(dir, session_id)) {
        return wal.latest();
    }
    drop(cache);
    let wal = MmapWal::open(dir, session_id).ok()?;
    wal.latest()
}

/// Scan from `start` forward to find the first byte after the last
/// complete record (or `start` if no complete records exist).
fn scan_records(data: &[u8], start: usize) -> usize {
    let mut pos = start;
    loop {
        if pos + 4 > data.len() {
            break pos;
        }
        let len = u32::from_le_bytes(data[pos..pos + 4].try_into().unwrap()) as usize;
        pos += 4;
        if pos + len > data.len() {
            break pos - 4; // back to start of incomplete record
        }
        pos += len;
    }
}

/// A user intervention injected into a running agent loop.
/// This allows the user to change strategy, inject info, or reprioritize mid-execution.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InterventionEvent {
    pub id: String,
    pub timestamp: f64,
    /// The intervention type: new strategy, priority change, info injection, pause.
    pub kind: InterventionKind,
    /// The actual user message / instruction.
    pub content: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum InterventionKind {
    /// User provides a new strategy or approach for the remaining work.
    NewStrategy,
    /// User changes what to focus on.
    ChangePriority,
    /// User injects additional context/information.
    InjectInfo,
    /// User asks to pause (agent should checkpoint and stop).
    Pause,
    /// User asks to resume from a checkpoint.
    Resume,
}

impl std::fmt::Display for InterventionKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            InterventionKind::NewStrategy => write!(f, "new_strategy"),
            InterventionKind::ChangePriority => write!(f, "change_priority"),
            InterventionKind::InjectInfo => write!(f, "inject_info"),
            InterventionKind::Pause => write!(f, "pause"),
            InterventionKind::Resume => write!(f, "resume"),
        }
    }
}

/// Plan state stored in checkpoint — what the agent has done, is doing, and plans to do.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct CheckpointPlan {
    pub completed: Vec<String>,
    pub in_progress: Option<String>,
    pub pending: Vec<String>,
    pub accumulated_context: String,
    pub artifacts: Vec<String>,
}

/// Full agent loop state for checkpoint/resume.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LoopCheckpoint {
    pub turn: u32,
    pub timestamp: f64,
    pub messages: Vec<Message>,
    pub history_info: Vec<String>,
    pub full_response: String,
    pub exit_reason: Option<String>,
    pub session_id: Option<String>,
    /// Plan state at checkpoint time.
    #[serde(default)]
    pub plan: CheckpointPlan,
    /// Todo items at checkpoint time (for UI state recovery).
    #[serde(default)]
    pub todos: Vec<oz_core_types::TodoItem>,
    /// Interventions applied since the last checkpoint.
    #[serde(default)]
    pub interventions: Vec<InterventionEvent>,
    /// Agent's thinking output up to this checkpoint (for UI recovery after crash).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub full_thinking: Option<String>,
    /// Git snapshot of the working tree at checkpoint time (for replay/debug).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub git_sha: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub git_branch: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub git_origin_url: Option<String>,
}

/// Borrowed mirror of [`LoopCheckpoint`] for the every-3-turns periodic
/// save: field names and serde attributes match exactly so the produced
/// JSON is byte-identical, but nothing is cloned (round3 P1-f — cloning a
/// 170K-token conversation every few turns was pure memcpy waste).
#[derive(Serialize)]
pub struct LoopCheckpointRef<'a> {
    pub turn: u32,
    pub timestamp: f64,
    pub messages: &'a [Message],
    pub history_info: &'a [String],
    pub full_response: &'a str,
    pub exit_reason: &'a Option<String>,
    pub session_id: Option<&'a str>,
    #[serde(default)]
    pub plan: &'a CheckpointPlan,
    #[serde(default)]
    pub todos: &'a [oz_core_types::TodoItem],
    #[serde(default)]
    pub interventions: &'a [InterventionEvent],
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub full_thinking: Option<&'a str>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub git_sha: Option<&'a str>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub git_branch: Option<&'a str>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub git_origin_url: Option<&'a str>,
}

/// Metadata about a saved checkpoint (used for listing without loading full data).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CheckpointMeta {
    pub session_id: String,
    pub turn: u32,
    pub timestamp: f64,
    pub filename: String,
    pub message_count: usize,
    pub exit_reason: Option<String>,
}

const CHECKPOINT_DIR: &str = "openzen/checkpoints";

pub fn checkpoint_dir(base_dir: &Path) -> PathBuf {
    base_dir.join(CHECKPOINT_DIR)
}

/// Capture the git state of the working tree, if it is a git repo.
pub fn git_snapshot(base_dir: &Path) -> (Option<String>, Option<String>, Option<String>) {
    let sha = std::process::Command::new("git")
        .args(["rev-parse", "HEAD"])
        .current_dir(base_dir)
        .output()
        .ok()
        .filter(|o| o.status.success())
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string());
    if sha.is_none() {
        return (None, None, None);
    }
    let branch = std::process::Command::new("git")
        .args(["branch", "--show-current"])
        .current_dir(base_dir)
        .output()
        .ok()
        .filter(|o| o.status.success())
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string());
    let origin = std::process::Command::new("git")
        .args(["config", "--get", "remote.origin.url"])
        .current_dir(base_dir)
        .output()
        .ok()
        .filter(|o| o.status.success())
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string());
    (sha, branch, origin)
}

/// Derive a CheckpointPlan from the current todo list.
pub fn plan_from_todos(todos: &[oz_core_types::TodoItem]) -> CheckpointPlan {
    let mut completed = Vec::new();
    let mut pending = Vec::new();
    let mut in_progress = None;
    for t in todos {
        match t.status.as_str() {
            "completed" => completed.push(t.content.clone()),
            "in_progress" => in_progress = Some(t.content.clone()),
            _ => pending.push(t.content.clone()),
        }
    }
    CheckpointPlan {
        completed,
        in_progress,
        pending,
        ..Default::default()
    }
}

/// Save a full loop checkpoint that can be used for resume.
pub fn save_loop_checkpoint(dir: &Path, session_id: &str, cp: &LoopCheckpoint) {
    save_loop_checkpoint_impl(dir, session_id, cp, true)
}

fn save_loop_checkpoint_impl(dir: &Path, session_id: &str, cp: &LoopCheckpoint, cleanup: bool) {
    if let Err(e) = std::fs::create_dir_all(dir) {
        tracing::warn!("Failed to create checkpoint dir: {e}");
        return;
    }
    let safe_session = sanitize_session_id(session_id);
    let path = dir.join(format!("loop_{}_{:03}.json", safe_session, cp.turn));
    // Compact serialization: a 170K-token context snapshots every 3 turns
    // and pretty-printing inflated the payload ~30% for zero benefit on a
    // machine-written recovery file.
    if let Ok(json) = serde_json::to_string(cp) {
        if let Err(e) = std::fs::write(&path, &json) {
            tracing::warn!("Failed to save loop checkpoint: {e}");
        }
    }
    // Fast-path copy for crash recovery: an mmap append instead of a
    // fresh file write + directory scan on resume.
    append_checkpoint_wal(dir, session_id, cp);
    if cleanup {
        cleanup_session_checkpoints(dir, &safe_session, 5);
    }
}

/// Save a checkpoint but keep all history (for intervention pauses).
pub fn save_checkpoint_persist(dir: &Path, session_id: &str, cp: &LoopCheckpoint) {
    save_loop_checkpoint_impl(dir, session_id, cp, false);
}

/// Async wrapper for the git metadata snapshot: spawns git subprocesses on
/// the blocking pool instead of stalling the runtime thread (and with it
/// every parallel tool dispatch and stream event).
pub async fn git_snapshot_async(
    base_dir: &Path,
) -> (Option<String>, Option<String>, Option<String>) {
    let dir = base_dir.to_path_buf();
    tokio::task::spawn_blocking(move || git_snapshot(&dir))
        .await
        .unwrap_or((None, None, None))
}

/// Async wrapper: checkpoint serialization + file write run off the
/// runtime thread.
pub async fn save_checkpoint_persist_async(dir: &Path, session_id: &str, cp: LoopCheckpoint) {
    let dir = dir.to_path_buf();
    let session_id = session_id.to_string();
    let _ = tokio::task::spawn_blocking(move || {
        save_checkpoint_persist(&dir, &session_id, &cp);
    })
    .await;
}

/// Async wrapper for the periodic loop checkpoint write.
pub async fn save_loop_checkpoint_async(dir: &Path, session_id: &str, cp: LoopCheckpoint) {
    let dir = dir.to_path_buf();
    let session_id = session_id.to_string();
    let _ = tokio::task::spawn_blocking(move || {
        save_loop_checkpoint(&dir, &session_id, &cp);
    })
    .await;
}

/// Borrowed periodic-save: serializes ONCE (no conversation clone), then
/// hands the raw bytes to the blocking pool for file + WAL writes.
/// `spawn_blocking` requires 'static, so the borrow ends at this
/// serialization boundary — that single compact to_vec is the only copy.
pub async fn save_loop_checkpoint_borrowed_async(
    dir: &Path,
    session_id: &str,
    cp: LoopCheckpointRef<'_>,
) {
    let turn = cp.turn;
    let Ok(json) = serde_json::to_vec(&cp) else {
        return;
    };
    let dir = dir.to_path_buf();
    let session_id = session_id.to_string();
    let _ = tokio::task::spawn_blocking(move || {
        write_loop_checkpoint_bytes(&dir, &session_id, turn, &json);
    })
    .await;
}

fn write_loop_checkpoint_bytes(dir: &Path, session_id: &str, turn: u32, json: &[u8]) {
    if let Err(e) = std::fs::create_dir_all(dir) {
        tracing::warn!("Failed to create checkpoint dir: {e}");
        return;
    }
    let safe_session = sanitize_session_id(session_id);
    let path = dir.join(format!("loop_{safe_session}_{turn:03}.json"));
    if let Err(e) = std::fs::write(&path, json) {
        tracing::warn!("Failed to save loop checkpoint: {e}");
    }
    append_checkpoint_wal_bytes(dir, session_id, json);
    cleanup_session_checkpoints(dir, &safe_session, 5);
}

/// Load the latest loop checkpoint for a given session.
pub fn load_latest_loop_checkpoint(dir: &Path, session_id: &str) -> Option<LoopCheckpoint> {
    let safe_session = sanitize_session_id(session_id);
    let prefix = format!("loop_{}_", safe_session);

    let checkpoints = collect_checkpoints(dir, Some(&prefix));
    let file_latest = checkpoints
        .last()
        .and_then(|latest| std::fs::read_to_string(latest).ok())
        .and_then(|data| serde_json::from_str::<LoopCheckpoint>(&data).ok());
    match (file_latest, read_latest_checkpoint_wal(dir, session_id)) {
        (Some(f), Some(w)) if w.turn >= f.turn => Some(w),
        (Some(f), _) => Some(f),
        (None, Some(w)) => Some(w),
        (None, None) => None,
    }
}

/// Load the best checkpoint for resume: the one with the highest turn
/// (latest progress). Ties broken by message count (richer context).
/// A later turn is authoritative even if compression shrank its message
/// list — resuming from an older, larger checkpoint silently rewinds the
/// task (todos revert, completed work re-runs).
pub fn load_best_loop_checkpoint(dir: &Path, session_id: &str) -> Option<LoopCheckpoint> {
    let safe_session = sanitize_session_id(session_id);
    let prefix = format!("loop_{}_", safe_session);
    let paths = collect_checkpoints(dir, Some(&prefix));
    if paths.is_empty() {
        // No JSON files (cleaned up?): the WAL may still hold the newest
        // snapshot.
        return read_latest_checkpoint_wal(dir, session_id);
    }
    let mut best: Option<(u32, usize, PathBuf)> = None;
    for path in &paths {
        if let Ok(data) = std::fs::read_to_string(path) {
            if let Ok(cp) = serde_json::from_str::<LoopCheckpoint>(&data) {
                let n = cp.messages.len();
                let turn = cp.turn;
                let replace = match &best {
                    Some((best_turn, best_msgs, _)) => {
                        turn > *best_turn || (turn == *best_turn && n > *best_msgs)
                    }
                    None => true,
                };
                if replace {
                    best = Some((turn, n, path.clone()));
                }
            }
        }
    }
    let file_best = best.and_then(|(_, _, path)| {
        let data = std::fs::read_to_string(&path).ok()?;
        serde_json::from_str::<LoopCheckpoint>(&data).ok()
    });
    // The WAL is written after the JSON file in the same save call, so a
    // WAL record with a >= turn is at least as recent (crash between the
    // two writes leaves the WAL one snapshot ahead).
    match (file_best, read_latest_checkpoint_wal(dir, session_id)) {
        (Some(f), Some(w)) => {
            if w.turn >= f.turn {
                Some(w)
            } else {
                Some(f)
            }
        }
        (Some(f), None) => Some(f),
        (None, Some(w)) => Some(w),
        (None, None) => None,
    }
}

/// Load a specific checkpoint by turn number.
pub fn load_checkpoint_at_turn(dir: &Path, session_id: &str, turn: u32) -> Option<LoopCheckpoint> {
    let safe_session = sanitize_session_id(session_id);
    let path = dir.join(format!("loop_{}_{:03}.json", safe_session, turn));
    if !path.exists() {
        return None;
    }
    let data = std::fs::read_to_string(&path).ok()?;
    serde_json::from_str(&data).ok()
}

/// List all checkpoints for a session with metadata.
pub fn list_session_checkpoints(dir: &Path, session_id: &str) -> Vec<CheckpointMeta> {
    let safe_session = sanitize_session_id(session_id);
    let prefix = format!("loop_{}_", safe_session);
    let paths = collect_checkpoints(dir, Some(&prefix));

    let mut metas: Vec<CheckpointMeta> = Vec::new();
    for path in &paths {
        if let Ok(data) = std::fs::read_to_string(path) {
            if let Ok(cp) = serde_json::from_str::<LoopCheckpoint>(&data) {
                let filename = path
                    .file_name()
                    .and_then(|n| n.to_str())
                    .unwrap_or("")
                    .to_string();
                metas.push(CheckpointMeta {
                    session_id: cp.session_id.unwrap_or_default(),
                    turn: cp.turn,
                    timestamp: cp.timestamp,
                    filename,
                    message_count: cp.messages.len(),
                    exit_reason: cp.exit_reason,
                });
            }
        }
    }

    metas.sort_by_key(|a| a.turn);
    metas
}

/// List all checkpoints across all sessions.
pub fn list_all_checkpoints(dir: &Path) -> Vec<CheckpointMeta> {
    let paths = collect_checkpoints(dir, Some("loop_"));
    let mut metas: Vec<CheckpointMeta> = Vec::new();

    for path in &paths {
        if let Ok(data) = std::fs::read_to_string(path) {
            if let Ok(cp) = serde_json::from_str::<LoopCheckpoint>(&data) {
                let filename = path
                    .file_name()
                    .and_then(|n| n.to_str())
                    .unwrap_or("")
                    .to_string();
                metas.push(CheckpointMeta {
                    session_id: cp.session_id.unwrap_or_default(),
                    turn: cp.turn,
                    timestamp: cp.timestamp,
                    filename,
                    message_count: cp.messages.len(),
                    exit_reason: cp.exit_reason,
                });
            }
        }
    }

    metas.sort_by_key(|a| a.turn);
    metas
}

fn sanitize_session_id(session_id: &str) -> String {
    session_id
        .chars()
        .map(|c| {
            if c.is_alphanumeric() || c == '_' || c == '-' {
                c
            } else {
                '_'
            }
        })
        .collect()
}

fn collect_checkpoints(dir: &Path, prefix: Option<&str>) -> Vec<PathBuf> {
    match std::fs::read_dir(dir) {
        Ok(entries) => {
            let mut paths: Vec<PathBuf> = entries
                .filter_map(|e| e.ok())
                .map(|e| e.path())
                .filter(|p| {
                    if let Some(pref) = prefix {
                        p.file_name()
                            .and_then(|n| n.to_str())
                            .map(|n| n.starts_with(pref) && n.ends_with(".json"))
                            .unwrap_or(false)
                    } else {
                        p.extension().map(|ext| ext == "json").unwrap_or(false)
                    }
                })
                .collect();
            paths.sort();
            paths
        }
        Err(_) => Vec::new(),
    }
}

fn cleanup_session_checkpoints(dir: &Path, safe_session: &str, keep: usize) {
    let prefix = format!("loop_{}_", safe_session);
    let entries = collect_checkpoints(dir, Some(&prefix));
    if entries.len() > keep {
        for entry in entries.iter().take(entries.len() - keep) {
            let _ = std::fs::remove_file(entry);
        }
    }
}

/// Create an InterventionEvent from user input.
pub fn make_intervention(kind: InterventionKind, content: &str) -> InterventionEvent {
    use std::time::{SystemTime, UNIX_EPOCH};
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    let id = format!("iv_{}_{}", nanos, rand_simple_id());
    InterventionEvent {
        id,
        timestamp: chrono::Utc::now().timestamp_millis() as f64 / 1000.0,
        kind,
        content: content.to_string(),
    }
}

fn rand_simple_id() -> u64 {
    // Simple random-ish ID without external crate dependency
    use std::time::{SystemTime, UNIX_EPOCH};
    let d = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default();
    (d.as_nanos() as u64)
        .wrapping_mul(6364136223846793005)
        .wrapping_add(1442695040888963407)
}

/// Apply an intervention to the agent's message list.
/// This inserts a system-style message with the intervention content at the current position,
/// allowing the LLM to see and respond to the new instructions on the next turn.
pub fn apply_intervention(messages: &mut Vec<Message>, intervention: &InterventionEvent) {
    let intervention_msg = format!(
        "[USER INTERVENTION - {}]\n{}",
        intervention.kind, intervention.content
    );
    // Inject as user message so the LLM sees it naturally in context
    messages.push(Message::user(&intervention_msg));
    tracing::info!(
        "Applied intervention '{}': {}",
        intervention.kind,
        &intervention.content[..intervention.content.len().min(100)]
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn loop_checkpoint_serialization_roundtrip() {
        let cp = LoopCheckpoint {
            turn: 3,
            timestamp: 1234.0,
            messages: vec![Message::user("hello"), Message::assistant("hi there")],
            history_info: vec!["step 1".into()],
            full_response: "hi there".into(),
            exit_reason: Some("done".into()),
            session_id: Some("test-session".into()),
            plan: CheckpointPlan::default(),
            todos: vec![],
            interventions: vec![],
            full_thinking: None,
            git_sha: None,
            git_branch: None,
            git_origin_url: None,
        };

        let json = serde_json::to_string_pretty(&cp).unwrap();
        let deserialized: LoopCheckpoint = serde_json::from_str(&json).unwrap();
        assert_eq!(deserialized.turn, 3);
        assert_eq!(deserialized.messages.len(), 2);
    }

    #[test]
    fn intervention_serialization() {
        let evt = make_intervention(
            InterventionKind::NewStrategy,
            "Let's try a different approach: use vector search first.",
        );
        let json = serde_json::to_string_pretty(&evt).unwrap();
        assert!(json.contains("new_strategy"));
        let deserialized: InterventionEvent = serde_json::from_str(&json).unwrap();
        assert_eq!(
            deserialized.content,
            "Let's try a different approach: use vector search first."
        );
    }

    #[test]
    fn test_intervention_all_kinds() {
        // Test all intervention kinds produce correct display output
        let kinds = [
            (InterventionKind::NewStrategy, "new_strategy"),
            (InterventionKind::ChangePriority, "change_priority"),
            (InterventionKind::InjectInfo, "inject_info"),
            (InterventionKind::Pause, "pause"),
            (InterventionKind::Resume, "resume"),
        ];
        for (kind, expected_str) in &kinds {
            assert_eq!(format!("{}", kind), *expected_str);
            let evt = make_intervention(kind.clone(), "test content");
            assert_eq!(evt.kind.to_string(), *expected_str);
            assert_eq!(evt.content, "test content");
        }
    }

    #[test]
    fn test_intervention_applies_correctly() {
        let mut messages = vec![Message::user("hello"), Message::assistant("hi there")];
        let intervention = make_intervention(
            InterventionKind::InjectInfo,
            "The database contains 1M records.",
        );
        apply_intervention(&mut messages, &intervention);
        assert_eq!(messages.len(), 3);
        let text = messages[2].content_text();
        assert!(text.contains("USER INTERVENTION"));
        assert!(text.contains("1M records"));
        assert!(text.contains("inject_info"));
    }

    #[test]
    fn test_intervention_pause_stops_loop() {
        let mut messages = vec![Message::user("continue working")];
        let intervention = make_intervention(InterventionKind::Pause, "Stop and save progress");
        apply_intervention(&mut messages, &intervention);
        assert_eq!(messages.len(), 2);
        let text = messages[1].content_text();
        assert!(text.contains("USER INTERVENTION"));
        assert!(text.contains("pause"));
        assert!(text.contains("Stop and save progress"));
    }

    #[test]
    fn test_checkpoint_plan_default() {
        let plan = CheckpointPlan::default();
        assert!(plan.completed.is_empty());
        assert!(plan.in_progress.is_none());
        assert!(plan.pending.is_empty());
        assert!(plan.accumulated_context.is_empty());
        assert!(plan.artifacts.is_empty());
    }

    #[test]
    fn test_checkpoint_plan_serialization() {
        let plan = CheckpointPlan {
            completed: vec!["step1".into(), "step2".into()],
            in_progress: Some("step3".into()),
            pending: vec!["step4".into()],
            accumulated_context: "some context".into(),
            artifacts: vec!["file1.txt".into()],
        };
        let json = serde_json::to_string_pretty(&plan).unwrap();
        let deserialized: CheckpointPlan = serde_json::from_str(&json).unwrap();
        assert_eq!(deserialized.completed.len(), 2);
        assert_eq!(deserialized.in_progress, Some("step3".into()));
        assert_eq!(deserialized.pending.len(), 1);
        assert_eq!(deserialized.accumulated_context, "some context");
    }

    #[test]
    fn test_multiple_checkpoints_save_and_list() {
        let dir = tempfile::tempdir().unwrap();
        let session_id = "multi-test-session";

        for i in 1..=3 {
            let cp = LoopCheckpoint {
                turn: i,
                timestamp: 1000.0 + i as f64,
                messages: vec![Message::user(format!("message {}", i))],
                history_info: vec![],
                full_response: format!("response {}", i),
                exit_reason: if i < 3 {
                    None
                } else {
                    Some("completed".into())
                },
                session_id: Some(session_id.into()),
                plan: CheckpointPlan::default(),

                todos: vec![],
                interventions: vec![],
                full_thinking: None,
                git_sha: None,
                git_branch: None,
                git_origin_url: None,
            };
            save_loop_checkpoint(dir.path(), session_id, &cp);
        }

        // List checkpoints
        let metas = list_session_checkpoints(dir.path(), session_id);
        assert_eq!(metas.len(), 3);
        assert_eq!(metas[0].turn, 1);
        assert_eq!(metas[2].turn, 3);
        assert_eq!(metas[2].exit_reason, Some("completed".into()));

        // Load latest
        let latest = load_latest_loop_checkpoint(dir.path(), session_id);
        assert!(latest.is_some());
        assert_eq!(latest.unwrap().turn, 3);

        // Load specific turn
        let cp2 = load_checkpoint_at_turn(dir.path(), session_id, 2);
        assert!(cp2.is_some());
        assert_eq!(cp2.unwrap().turn, 2);
    }

    #[test]
    fn test_resume_from_checkpoint_restores_messages() {
        let dir = tempfile::tempdir().unwrap();
        let session_id = "resume-test";

        let original_messages = vec![
            Message::user("first question"),
            Message::assistant("first answer"),
            Message::user("second question"),
        ];
        let cp = LoopCheckpoint {
            turn: 2,
            timestamp: 2000.0,
            messages: original_messages.clone(),
            history_info: vec!["history info".into()],
            full_response: "second answer".into(),
            exit_reason: Some("paused_by_user".into()),
            session_id: Some(session_id.into()),
            plan: CheckpointPlan::default(),
            todos: vec![],
            interventions: vec![],
            full_thinking: None,
            git_sha: None,
            git_branch: None,
            git_origin_url: None,
        };
        save_loop_checkpoint(dir.path(), session_id, &cp);

        // Load and verify
        let loaded = load_latest_loop_checkpoint(dir.path(), session_id).unwrap();
        assert_eq!(loaded.turn, 2);
        assert_eq!(loaded.messages.len(), 3);
        assert_eq!(loaded.messages[0].content_text(), "first question");
        assert_eq!(loaded.messages[2].content_text(), "second question");
        assert_eq!(loaded.exit_reason, Some("paused_by_user".into()));
    }

    #[test]
    fn test_list_all_checkpoints_multiple_sessions() {
        let dir = tempfile::tempdir().unwrap();
        let sessions = ["session-a", "session-b", "session-c"];

        for (i, sid) in sessions.iter().enumerate() {
            let cp = LoopCheckpoint {
                turn: 1,
                timestamp: 1000.0 + i as f64,
                messages: vec![Message::user(format!("hello from {}", sid))],
                history_info: vec![],
                full_response: String::new(),
                exit_reason: None,

                session_id: Some(sid.to_string()),
                plan: CheckpointPlan::default(),
                todos: vec![],
                interventions: vec![],
                full_thinking: None,
                git_sha: None,
                git_branch: None,
                git_origin_url: None,
            };
            save_loop_checkpoint(dir.path(), sid, &cp);
        }

        let all = list_all_checkpoints(dir.path());
        assert_eq!(all.len(), 3);
        assert!(all.iter().any(|m| m.session_id == "session-a"));
        assert!(all.iter().any(|m| m.session_id == "session-b"));
        assert!(all.iter().any(|m| m.session_id == "session-c"));
    }

    #[test]
    fn test_checkpoint_cleanup_keeps_only_latest() {
        let dir = tempfile::tempdir().unwrap();
        let session_id = "cleanup-test";

        // Save 7 checkpoints (only 5 should be kept)
        for i in 1..=7 {
            let cp = LoopCheckpoint {
                turn: i,
                timestamp: 1000.0 + i as f64,
                messages: vec![Message::user(format!("msg {}", i))],
                history_info: vec![],

                full_response: String::new(),
                exit_reason: None,
                session_id: Some(session_id.into()),
                plan: CheckpointPlan::default(),
                todos: vec![],
                interventions: vec![],
                full_thinking: None,
                git_sha: None,
                git_branch: None,
                git_origin_url: None,
            };
            save_loop_checkpoint(dir.path(), session_id, &cp);
        }

        let metas = list_session_checkpoints(dir.path(), session_id);
        assert_eq!(metas.len(), 5);
        // Should be turns 3,4,5,6,7 (oldest 2 removed)
        assert_eq!(metas[0].turn, 3);
        assert_eq!(metas[4].turn, 7);
    }

    #[test]
    fn test_save_and_load_loop_checkpoint() {
        let dir = tempfile::tempdir().unwrap();
        let cp = LoopCheckpoint {
            turn: 1,
            timestamp: 1000.0,
            messages: vec![Message::user("test")],
            history_info: vec![],
            full_response: String::new(),
            exit_reason: None,
            session_id: Some("session-1".into()),
            plan: CheckpointPlan::default(),
            todos: vec![],
            interventions: vec![],
            full_thinking: None,
            git_sha: None,
            git_branch: None,
            git_origin_url: None,
        };

        save_loop_checkpoint(dir.path(), "session-1", &cp);
        let loaded = load_latest_loop_checkpoint(dir.path(), "session-1");
        assert!(loaded.is_some());
        assert_eq!(loaded.unwrap().turn, 1);
    }

    #[test]
    fn list_checkpoints_empty_dir() {
        let dir = tempfile::tempdir().unwrap();
        let metas = list_session_checkpoints(dir.path(), "nonexistent");
        assert!(metas.is_empty());
    }

    #[test]
    fn intervention_applies_correctly() {
        let mut messages = vec![Message::user("hello")];
        let intervention = make_intervention(
            InterventionKind::InjectInfo,
            "The database contains 1M records.",
        );
        apply_intervention(&mut messages, &intervention);
        assert_eq!(messages.len(), 2);
        let text = messages[1].content_text();
        assert!(text.contains("USER INTERVENTION"));
        assert!(text.contains("1M records"));
    }

    // ── MmapWal tests ──

    #[test]
    fn mmap_wal_write_and_read_latest() {
        let dir = tempfile::tempdir().unwrap();
        let mut wal = MmapWal::open(dir.path(), "test-session").unwrap();
        assert_eq!(wal.cursor, WAL_HEADER_SIZE);

        let cp = LoopCheckpoint {
            turn: 1,
            timestamp: 1000.0,
            messages: vec![Message::user("hello from wal")],

            history_info: vec![],
            full_response: "response".into(),
            exit_reason: None,
            session_id: Some("test-session".into()),
            plan: CheckpointPlan::default(),
            todos: vec![],
            interventions: vec![],
            full_thinking: None,
            git_sha: None,
            git_branch: None,
            git_origin_url: None,
        };
        wal.append(&cp).unwrap();
        wal.flush().unwrap();

        // Re-open and read latest
        let wal2 = MmapWal::open(dir.path(), "test-session").unwrap();
        let latest = wal2.latest();
        assert!(latest.is_some());
        assert_eq!(latest.unwrap().turn, 1);
    }

    #[test]
    fn mmap_wal_multiple_checkpoints() {
        let dir = tempfile::tempdir().unwrap();
        let mut wal = MmapWal::open(dir.path(), "multi-test").unwrap();

        for i in 1..=5 {
            let cp = LoopCheckpoint {
                turn: i,
                timestamp: 1000.0 + i as f64,
                messages: vec![Message::user(format!("turn {}", i))],
                history_info: vec![],
                full_response: format!("resp {}", i),
                exit_reason: if i == 5 { Some("done".into()) } else { None },
                session_id: Some("multi-test".into()),
                plan: CheckpointPlan::default(),
                todos: vec![],
                interventions: vec![],
                full_thinking: None,
                git_sha: None,
                git_branch: None,
                git_origin_url: None,
            };
            wal.append(&cp).unwrap();
        }
        wal.flush().unwrap();

        // Verify latest
        let latest = wal.latest();
        assert_eq!(latest.unwrap().turn, 5);

        // Verify all
        let all = wal.all();
        assert_eq!(all.len(), 5);
        assert_eq!(all[0].turn, 1);
        assert_eq!(all[4].turn, 5);
    }

    #[test]
    fn mmap_wal_reopen_and_recover() {
        let dir = tempfile::tempdir().unwrap();

        // Write in one session
        {
            let mut wal = MmapWal::open(dir.path(), "recover-test").unwrap();
            let cp = LoopCheckpoint {
                turn: 3,
                timestamp: 3000.0,
                messages: vec![Message::user("saved state")],
                history_info: vec!["step1".into()],
                full_response: "final".into(),
                exit_reason: Some("paused".into()),
                session_id: Some("recover-test".into()),
                plan: CheckpointPlan::default(),
                todos: vec![],
                interventions: vec![],
                full_thinking: None,
                git_sha: None,
                git_branch: None,
                git_origin_url: None,
            };
            wal.append(&cp).unwrap();
            wal.flush().unwrap();
        }

        // Recover in a new session
        let wal2 = MmapWal::open(dir.path(), "recover-test").unwrap();
        let recovered = wal2.latest().unwrap();
        assert_eq!(recovered.turn, 3);
        assert_eq!(recovered.messages[0].content_text(), "saved state");
        assert_eq!(recovered.exit_reason, Some("paused".into()));
    }

    #[test]
    fn mmap_wal_empty_wal_returns_none() {
        let dir = tempfile::tempdir().unwrap();
        let wal = MmapWal::open(dir.path(), "empty-test").unwrap();
        assert!(wal.latest().is_none());
        assert!(wal.all().is_empty());
    }

    #[test]
    fn mmap_wal_grows_beyond_initial_capacity() {
        let dir = tempfile::tempdir().unwrap();
        let mut wal = MmapWal::open(dir.path(), "grow-test").unwrap();

        // Write many large checkpoints to force growth past initial 1 MB
        for i in 0..20 {
            let big_msg = format!("turn {i}: {}", "data".repeat(5000));
            let cp = LoopCheckpoint {
                turn: i,
                timestamp: i as f64,
                messages: vec![Message::user(&big_msg)],
                history_info: vec![],
                full_response: big_msg,
                exit_reason: None,
                session_id: Some("grow-test".into()),
                plan: CheckpointPlan::default(),
                todos: vec![],
                interventions: vec![],
                full_thinking: None,
                git_sha: None,
                git_branch: None,
                git_origin_url: None,
            };
            wal.append(&cp).unwrap();
        }
        wal.flush().unwrap();

        // Verify all 20 records are readable
        let all = wal.all();
        assert_eq!(all.len(), 20);
        assert_eq!(all[19].turn, 19);
        // Cursor advanced past header
        assert!(
            wal.cursor > WAL_HEADER_SIZE,
            "WAL cursor should have advanced past header, cursor={}",
            wal.cursor
        );
    }
}

#[cfg(test)]
mod wal_integration_tests {
    use super::*;

    fn tmp_dir(tag: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("oz-wal-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    fn cp_for(turn: u32, session: &str) -> LoopCheckpoint {
        LoopCheckpoint {
            turn,
            timestamp: turn as f64,
            messages: vec![Message::user(format!("turn {turn}"))],
            history_info: vec![],
            full_response: format!("resp {turn}"),
            exit_reason: None,
            session_id: Some(session.to_string()),
            plan: Default::default(),
            todos: vec![],
            interventions: vec![],
            full_thinking: None,
            git_sha: None,
            git_branch: None,
            git_origin_url: None,
        }
    }

    #[test]
    fn save_then_load_prefers_wal_latest() {
        let dir = tmp_dir("roundtrip");
        let sid = "wal-roundtrip";
        save_loop_checkpoint(&dir, sid, &cp_for(1, sid));
        save_loop_checkpoint(&dir, sid, &cp_for(4, sid));
        let best = load_best_loop_checkpoint(&dir, sid).expect("checkpoint must load");
        assert_eq!(best.turn, 4);
        let latest = load_latest_loop_checkpoint(&dir, sid).expect("latest must load");
        assert_eq!(latest.turn, 4);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn wal_compacts_after_record_budget() {
        let dir = tmp_dir("compact");
        let sid = "wal-compact";
        for turn in 1..=20u32 {
            save_loop_checkpoint(&dir, sid, &cp_for(turn, sid));
        }
        // The WAL must not carry all 20 records: compaction keeps it small.
        let wal = MmapWal::open(&dir, sid).expect("wal opens");
        assert!(
            wal.all().len() <= 10,
            "expected compacted WAL, got {} records",
            wal.all().len()
        );
        // And the newest snapshot survives.
        assert_eq!(wal.latest().unwrap().turn, 20);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn wal_only_survives_json_cleanup() {
        let dir = tmp_dir("walonly");
        let sid = "wal-only";
        save_loop_checkpoint(&dir, sid, &cp_for(7, sid));
        // Simulate JSON files being cleaned externally.
        for entry in std::fs::read_dir(&dir).unwrap().flatten() {
            let p = entry.path();
            if p.extension().and_then(|e| e.to_str()) == Some("json") {
                std::fs::remove_file(&p).unwrap();
            }
        }
        let best = load_best_loop_checkpoint(&dir, sid).expect("wal fallback loads");
        assert_eq!(best.turn, 7);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
