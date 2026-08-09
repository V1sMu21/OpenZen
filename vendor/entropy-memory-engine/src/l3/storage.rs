use std::collections::HashMap;
use std::fs;
use std::io::{Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Mutex, RwLock};

use serde::{Deserialize, Serialize};

use crate::core::error::MemoryError;
use crate::core::types::Memory;
use crate::core::MemoryContent;
use crate::core::MemoryResult;

/// Append-only write-ahead log file name.
const LOG_EXT: &str = "log";

/// Tunable compaction policy for [`L3Storage`].
#[derive(Debug, Clone)]
pub struct StorageConfig {
    /// Compaction fires every `compact_interval` mutations.
    pub compact_interval: u64,
    /// Compaction also fires when the log exceeds this many bytes.
    pub compact_log_bytes: u64,
}

impl Default for StorageConfig {
    fn default() -> Self {
        Self {
            compact_interval: 256,
            compact_log_bytes: 4 * 1024 * 1024,
        }
    }
}

/// One framed entry in the write-ahead log.
#[derive(Debug, Clone, Serialize, Deserialize)]
enum LogEntry {
    Put(Memory),
    Remove(u64),
    Clear,
}

/// Append-only log writer.
///
/// The file handle is only `None` if the log could not be opened at
/// construction time; in that case mutations fail loudly instead of
/// silently pretending to persist.
struct LogWriter {
    file: Option<fs::File>,
    bytes: u64,
}

impl LogWriter {
    fn open(path: &Path) -> Self {
        if let Some(parent) = path.parent() {
            let _ = fs::create_dir_all(parent);
        }
        let file = fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)
            .ok();
        let bytes = file
            .as_ref()
            .and_then(|f| f.metadata().ok())
            .map(|m| m.len())
            .unwrap_or(0);
        Self { file, bytes }
    }

    fn append(&mut self, entry: &LogEntry) -> MemoryResult<()> {
        let bytes =
            bincode::serialize(entry).map_err(|e| MemoryError::Serialization(e.to_string()))?;
        let file = self
            .file
            .as_mut()
            .ok_or_else(|| MemoryError::WalWrite("log file unavailable".to_string()))?;
        file.write_all(&(bytes.len() as u64).to_le_bytes())
            .map_err(|e| MemoryError::WalWrite(format!("log append failed: {}", e)))?;
        file.write_all(&bytes)
            .map_err(|e| MemoryError::WalWrite(format!("log append failed: {}", e)))?;
        self.bytes += 8 + bytes.len() as u64;
        Ok(())
    }

    fn truncate(&mut self, path: &Path) {
        match self.file.as_mut() {
            Some(file) => {
                if file.set_len(0).is_err() {
                    return;
                }
                if file.seek(SeekFrom::Start(0)).is_err() {
                    return;
                }
            }
            // No handle available; truncate via the path instead.
            None => {
                if fs::write(path, &[]).is_err() {
                    return;
                }
            }
        }
        self.bytes = 0;
    }
}

/// L3 persistent file-backed storage.
///
/// Durability model: the in-memory `HashMap` is authoritative while the
/// process is alive; every mutation is first appended to an append-only
/// write-ahead log (`.log`) and only applied in memory afterwards. On
/// construction the compacted snapshot (`.bin`) is loaded and the log is
/// replayed on top of it, which makes reloads equivalent to the last
/// acknowledged mutation.
///
/// The snapshot is refreshed by compaction, which fires every
/// [`StorageConfig::compact_interval`] mutations or when the log grows past
/// [`StorageConfig::compact_log_bytes`]. Compaction snapshots the map,
/// writes a temp file, renames it over the snapshot and truncates the log,
/// all while holding the log lock — replaying the pre-truncation log over
/// the new snapshot is idempotent, so every crash window recovers to the
/// same final state.
pub struct L3Storage {
    path: PathBuf,
    log_path: PathBuf,
    memories: RwLock<HashMap<u64, Memory>>,
    subject_index: RwLock<HashMap<String, Vec<u64>>>,
    log: Mutex<LogWriter>,
    config: StorageConfig,
    mutations: AtomicU64,
}

impl L3Storage {
    pub fn new(path: impl Into<PathBuf>) -> Self {
        Self::with_config(path, StorageConfig::default())
    }

    pub fn with_config(path: impl Into<PathBuf>, config: StorageConfig) -> Self {
        let path = path.into();
        let log_path = path.with_extension(LOG_EXT);
        let mut memories = HashMap::new();
        if path.exists() {
            if let Some(loaded) = Self::load_snapshot(&path) {
                memories = loaded;
            }
        }
        Self::replay_log(&log_path, &mut memories);
        let subject_index = Self::build_subject_index(&memories);
        let log = Mutex::new(LogWriter::open(&log_path));
        Self {
            path,
            log_path,
            memories: RwLock::new(memories),
            subject_index: RwLock::new(subject_index),
            log,
            config,
            mutations: AtomicU64::new(0),
        }
    }

    pub fn len(&self) -> usize {
        self.memories.read().unwrap().len()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    pub fn store(&self, memory: Memory) -> MemoryResult<()> {
        let id = memory.id;
        // Write-ahead: log the mutation, then apply it in memory. Both steps
        // happen under the log lock so compaction can never snapshot a state
        // that is ahead of the log it is about to truncate.
        let mut log_guard = self.log.lock().unwrap();
        log_guard.append(&LogEntry::Put(memory.clone()))?;
        {
            if let MemoryContent::Fact(ref fact) = memory.content {
                self.subject_index
                    .write()
                    .unwrap()
                    .entry(fact.subject.clone())
                    .or_insert_with(Vec::new)
                    .push(id);
            }
            self.memories.write().unwrap().insert(id, memory);
        }
        drop(log_guard);
        self.maybe_compact();
        Ok(())
    }

    pub fn get(&self, id: u64) -> Option<Memory> {
        self.memories.read().ok()?.get(&id).cloned()
    }

    pub fn get_by_subject(&self, subject: &str) -> Vec<Memory> {
        let index = self.subject_index.read().unwrap();
        let memories = self.memories.read().unwrap();
        match index.get(subject) {
            Some(ids) => ids
                .iter()
                .filter_map(|id| memories.get(id).cloned())
                .collect(),
            None => Vec::new(),
        }
    }

    /// In-place metadata update (e.g. `superseded_by`) without touching
    /// content. The updated memory is written to the WAL under the same log
    /// lock as `store()`, so crash replay reproduces the flag.
    pub fn update_metadata<F>(&self, id: u64, f: F) -> bool
    where
        F: FnOnce(&mut crate::core::MemoryMeta),
    {
        let mut log_guard = self.log.lock().unwrap();
        let updated = {
            let memories = self.memories.read().unwrap();
            let Some(mem) = memories.get(&id) else {
                return false;
            };
            let mut updated = mem.clone();
            f(&mut updated.metadata);
            updated
        };
        // WAL first, then in-memory — same write-ahead ordering as store(),
        // so a failed append never leaves memory ahead of the log.
        if log_guard.append(&LogEntry::Put(updated.clone())).is_err() {
            return false;
        }
        self.memories.write().unwrap().insert(id, updated);
        drop(log_guard);
        self.maybe_compact();
        true
    }

    pub fn remove(&self, id: u64) -> bool {
        let mut log_guard = self.log.lock().unwrap();
        if !self.memories.read().unwrap().contains_key(&id) {
            return false;
        }
        // WAL first, then in-memory — a failed append leaves memory untouched
        // so a crash cannot resurrect a removed entry.
        if log_guard.append(&LogEntry::Remove(id)).is_err() {
            return false;
        }
        let mem = self.memories.write().unwrap().remove(&id).unwrap();
        if let MemoryContent::Fact(ref fact) = mem.content {
            let mut index = self.subject_index.write().unwrap();
            if let Some(ids) = index.get_mut(&fact.subject) {
                ids.retain(|i| *i != id);
                if ids.is_empty() {
                    index.remove(&fact.subject);
                }
            }
        }
        drop(log_guard);
        self.maybe_compact();
        true
    }

    pub fn all_ids(&self) -> Vec<u64> {
        self.memories.read().unwrap().keys().copied().collect()
    }

    pub fn all(&self) -> Vec<Memory> {
        self.memories.read().unwrap().values().cloned().collect()
    }

    pub fn clear(&self) {
        let mut log_guard = self.log.lock().unwrap();
        // WAL first, then in-memory — same write-ahead ordering as store().
        if log_guard.append(&LogEntry::Clear).is_err() {
            return;
        }
        self.memories.write().unwrap().clear();
        self.subject_index.write().unwrap().clear();
        drop(log_guard);
        self.maybe_compact();
    }

    pub fn search_by_text(&self, query: &str) -> Vec<Memory> {
        let query_keywords: Vec<String> = query
            .split_whitespace()
            .filter(|w| w.len() >= 4)
            .map(|w| w.to_lowercase())
            .collect();
        if query_keywords.is_empty() {
            return Vec::new();
        }
        self.memories
            .read()
            .unwrap()
            .values()
            .filter(|m| {
                let text = match &m.content {
                    MemoryContent::Fact(f) => {
                        format!("{} {} {}", f.subject, f.predicate, f.object)
                    }
                    MemoryContent::Summary(s) => s.clone(),
                    _ => String::new(),
                };
                let text_lower = text.to_lowercase();
                query_keywords.iter().any(|kw| text_lower.contains(kw))
            })
            .cloned()
            .collect()
    }

    pub fn storage_path(&self) -> &Path {
        &self.path
    }

    /// True when a compaction is due per the configured policy.
    fn compaction_due(&self) -> bool {
        let mutations = self.mutations.fetch_add(1, Ordering::Relaxed) + 1;
        mutations % self.config.compact_interval == 0
    }

    fn maybe_compact(&self) {
        // Cheap check first (atomic, no locking). The byte check needs the
        // log lock; it only fires rarely, so locking here is acceptable.
        if !self.compaction_due() {
            return;
        }
        let log_bytes = self.log.lock().unwrap().bytes;
        if log_bytes < self.config.compact_log_bytes {
            return;
        }
        self.compact();
    }

    /// Full rewrite of the snapshot followed by log truncation.
    ///
    /// Runs entirely under the log lock: no append can interleave, so the
    /// snapshot always includes every mutation present in the log being
    /// truncated. Order is write-temp → rename → truncate, which keeps every
    /// crash window recoverable (replaying the old log over the new snapshot
    /// is idempotent).
    fn compact(&self) {
        let mut log_guard = self.log.lock().unwrap();
        let memories = self.memories.read().unwrap().clone();
        let data = match bincode::serialize(&memories) {
            Ok(d) => d,
            Err(_) => return,
        };
        let tmp_path = self.path.with_extension("bin.tmp");
        if let Some(parent) = self.path.parent() {
            let _ = fs::create_dir_all(parent);
        }
        if fs::write(&tmp_path, &data).is_err() {
            return;
        }
        if fs::rename(&tmp_path, &self.path).is_err() {
            let _ = fs::remove_file(&tmp_path);
            return;
        }
        log_guard.truncate(&self.log_path);
        self.mutations.store(0, Ordering::Relaxed);
    }

    fn load_snapshot(path: &Path) -> Option<HashMap<u64, Memory>> {
        let data = fs::read(path).ok()?;
        bincode::deserialize(&data).ok()
    }

    /// Applies the write-ahead log on top of a loaded snapshot.
    ///
    /// Entries are framed as `[u64 length][bincode bytes]`. A torn tail
    /// (crash mid-append) is silently ignored — everything before it is
    /// still replayed.
    fn replay_log(path: &Path, memories: &mut HashMap<u64, Memory>) {
        let data = match fs::read(path) {
            Ok(d) => d,
            Err(_) => return,
        };
        let mut offset = 0usize;
        while offset + 8 <= data.len() {
            let len = u64::from_le_bytes(data[offset..offset + 8].try_into().expect("8-byte frame"))
                as usize;
            offset += 8;
            // Checked arithmetic: a corrupt length prefix must never overflow
            // past the buffer or wrap around; both cases are torn/corrupt tails.
            let end = match offset.checked_add(len) {
                Some(end) if end <= data.len() => end,
                _ => break,
            };
            if let Ok(entry) = bincode::deserialize::<LogEntry>(&data[offset..end]) {
                match entry {
                    LogEntry::Put(m) => {
                        memories.insert(m.id, m);
                    }
                    LogEntry::Remove(id) => {
                        memories.remove(&id);
                    }
                    LogEntry::Clear => memories.clear(),
                }
            }
            offset = end;
        }
    }

    fn build_subject_index(memories: &HashMap<u64, Memory>) -> HashMap<String, Vec<u64>> {
        let mut subject_index: HashMap<String, Vec<u64>> = HashMap::new();
        for (id, mem) in memories {
            if let MemoryContent::Fact(ref fact) = mem.content {
                subject_index
                    .entry(fact.subject.clone())
                    .or_insert_with(Vec::new)
                    .push(*id);
            }
        }
        subject_index
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::{Fact, MemoryContent};
    use tempfile::tempdir;

    fn make_memory(id: u64, subject: &str) -> Memory {
        let mut mem = Memory::new(MemoryContent::Fact(Fact::new(subject, "pred", "obj")));
        mem.id = id;
        mem
    }

    fn tiny_config() -> StorageConfig {
        StorageConfig {
            compact_interval: 4,
            compact_log_bytes: 0, // always compact when due
        }
    }

    #[test]
    fn test_new_storage_empty() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("memories.bin");
        let s = L3Storage::new(&path);
        assert!(s.is_empty());
    }

    #[test]
    fn test_store_and_get() {
        let dir = tempdir().unwrap();
        let s = L3Storage::new(dir.path().join("mem.bin"));
        let m = make_memory(1, "alice");
        s.store(m).unwrap();
        assert_eq!(s.len(), 1);
        assert!(s.get(1).is_some());
    }

    #[test]
    fn test_persistence_across_reload() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("persist.bin");
        {
            let s = L3Storage::new(&path);
            s.store(make_memory(1, "alice")).unwrap();
            s.store(make_memory(2, "bob")).unwrap();
        }
        {
            let s = L3Storage::new(&path);
            assert_eq!(s.len(), 2);
            assert!(s.get(1).is_some());
            assert!(s.get(2).is_some());
        }
    }

    #[test]
    fn test_remove_and_persist() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("remove.bin");
        {
            let s = L3Storage::new(&path);
            s.store(make_memory(1, "x")).unwrap();
            s.remove(1);
        }
        {
            let s = L3Storage::new(&path);
            assert!(s.is_empty());
        }
    }

    #[test]
    fn test_clear_persists() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("clear.bin");
        {
            let s = L3Storage::new(&path);
            s.store(make_memory(1, "x")).unwrap();
            s.store(make_memory(2, "y")).unwrap();
            s.clear();
        }
        {
            let s = L3Storage::new(&path);
            assert!(s.is_empty());
        }
    }

    #[test]
    fn test_search_by_text() {
        let dir = tempdir().unwrap();
        let s = L3Storage::new(dir.path().join("search.bin"));
        s.store(make_memory(1, "rust")).unwrap();
        s.store(make_memory(2, "python")).unwrap();
        let results = s.search_by_text("rust");
        assert_eq!(results.len(), 1);
    }

    #[test]
    fn test_get_by_subject() {
        let dir = tempdir().unwrap();
        let s = L3Storage::new(dir.path().join("subj.bin"));
        s.store(make_memory(1, "alice")).unwrap();
        s.store(make_memory(2, "alice")).unwrap();
        assert_eq!(s.get_by_subject("alice").len(), 2);
    }

    #[test]
    fn test_clear() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("clear2.bin");
        {
            let s = L3Storage::new(&path);
            s.store(make_memory(1, "x")).unwrap();
            s.clear();
        }
        {
            let s = L3Storage::new(&path);
            assert!(s.is_empty());
        }
    }

    #[test]
    fn test_all_ids() {
        let dir = tempdir().unwrap();
        let s = L3Storage::new(dir.path().join("ids.bin"));
        s.store(make_memory(10, "a")).unwrap();
        s.store(make_memory(20, "b")).unwrap();
        let mut ids = s.all_ids();
        ids.sort();
        assert_eq!(ids, vec![10, 20]);
    }

    #[test]
    fn test_log_replay_recovers_uncompacted_writes() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("replay.bin");
        // No compaction ever fires with the default config at this volume,
        // so everything lives only in the log.
        {
            let s = L3Storage::new(&path);
            for i in 1..=5u64 {
                s.store(make_memory(i, "subject")).unwrap();
            }
            s.remove(3);
        }
        {
            let s = L3Storage::new(&path);
            assert_eq!(s.len(), 4);
            assert!(s.get(1).is_some());
            assert!(s.get(3).is_none());
            assert_eq!(s.get_by_subject("subject").len(), 4);
        }
    }

    #[test]
    fn test_compaction_preserves_data_and_truncates_log() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("compact.bin");
        let log_path = path.with_extension(LOG_EXT);
        {
            let s = L3Storage::with_config(&path, tiny_config());
            for i in 1..=12u64 {
                s.store(make_memory(i, "s")).unwrap();
            }
            // 12 stores with compact_interval = 4 → compaction ran at 4, 8, 12.
            let log_len = fs::metadata(&log_path).unwrap().len();
            assert!(
                log_len < 512,
                "log should be truncated after compaction, got {} bytes",
                log_len
            );
        }
        {
            let s = L3Storage::new(&path);
            assert_eq!(s.len(), 12);
            assert!(s.get(7).is_some());
            assert!(s.get(12).is_some());
        }
    }

    #[test]
    fn test_remove_after_compaction_persists() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("rmcompact.bin");
        {
            let s = L3Storage::with_config(&path, tiny_config());
            for i in 1..=6u64 {
                s.store(make_memory(i, "s")).unwrap();
            }
            s.remove(2); // tombstone lands in the (truncated) log
        }
        {
            let s = L3Storage::new(&path);
            assert_eq!(s.len(), 5);
            assert!(s.get(2).is_none());
        }
    }

    #[test]
    fn test_update_metadata_survives_reload() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("meta.bin");
        {
            let s = L3Storage::new(&path);
            s.store(make_memory(1, "s")).unwrap();
            assert!(
                s.update_metadata(1, |meta| meta.superseded_by = Some(99)),
                "existing id must be updatable"
            );
            assert!(
                !s.update_metadata(404, |meta| meta.superseded_by = Some(99)),
                "missing id must not update"
            );
        }
        {
            let s = L3Storage::new(&path);
            let mem = s.get(1).expect("memory must survive reload");
            assert_eq!(
                mem.metadata.superseded_by,
                Some(99),
                "superseded_by must replay from the WAL"
            );
        }
    }

    #[test]
    fn test_torn_tail_write_is_ignored_on_reload() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("torn.bin");
        let log_path = path.with_extension(LOG_EXT);
        {
            let s = L3Storage::new(&path);
            s.store(make_memory(1, "a")).unwrap();
            s.store(make_memory(2, "b")).unwrap();
        }
        // Simulate a crash mid-append: a valid-looking length prefix that
        // claims more bytes than remain in the file.
        let mut corrupt = fs::read(&log_path).unwrap();
        corrupt.extend_from_slice(&(1_000_000u64).to_le_bytes());
        corrupt.extend_from_slice(b"partial");
        fs::write(&log_path, &corrupt).unwrap();
        {
            let s = L3Storage::new(&path);
            assert_eq!(s.len(), 2, "replay must stop at the torn tail");
            assert!(s.get(1).is_some());
            assert!(s.get(2).is_some());
        }
    }

    #[test]
    fn test_clear_entry_replayed_over_snapshot() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("clearreplay.bin");
        {
            let s = L3Storage::with_config(&path, tiny_config());
            for i in 1..=8u64 {
                s.store(make_memory(i, "s")).unwrap();
            }
            // 8 stores compact at 4 and 8 → snapshot on disk has all 8.
            s.clear(); // Clear entry appended to the (now short) log
        }
        {
            let s = L3Storage::new(&path);
            assert!(s.is_empty(), "Clear in log must win over snapshot");
        }
    }
}
