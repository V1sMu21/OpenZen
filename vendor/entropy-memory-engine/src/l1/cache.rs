use std::sync::Arc;

use dashmap::DashMap;
use foldhash::fast::RandomState as FoldBuildHasher;
use moka::sync::Cache as MokaCache;
use parking_lot::RwLock;

use crate::core::{Memory, MemoryInput, MemoryResult, Query};

use super::attention_lru::{AttentionLRU, AttentionLRUConfig};
use super::sdr::{SDRCache, SDRConfig};
use super::wal::{Wal, WalConfig, WalEntryType};

#[derive(Debug, Clone)]
pub struct L1CacheConfig {
    pub capacity: usize,
    pub attention_lru: AttentionLRUConfig,
    pub sdr: SDRConfig,
    pub wal: WalConfig,
}

impl Default for L1CacheConfig {
    fn default() -> Self {
        Self {
            capacity: 10_000,
            attention_lru: AttentionLRUConfig::default(),
            sdr: SDRConfig::default(),
            wal: WalConfig::default(),
        }
    }
}

type FoldState = FoldBuildHasher;

pub struct L1Cache {
    config: L1CacheConfig,

    /// Channel A: 主哈希表 (无锁并发读, foldhash)
    primary: Arc<DashMap<u64, Arc<RwLock<Memory>>, FoldState>>,

    /// 别名索引: String → Vec<u64>
    alias: Arc<DashMap<String, smallvec::SmallVec<[u64; 4]>, FoldState>>,

    /// moka W-TinyLFU 基线缓存 (淘汰兜底)
    moka: MokaCache<u64, Arc<RwLock<Memory>>>,

    /// Attention-LRU 自定义淘汰策略
    attention_lru: Arc<AttentionLRU>,

    /// 写前日志
    wal: Arc<Wal>,

    /// Channel B: SDR 超维记忆 (实验性)
    sdr: Arc<SDRCache>,

    /// 插入计数 (用于触发周期性维护)
    insert_count: std::sync::atomic::AtomicU64,
}

impl L1Cache {
    pub fn new(config: L1CacheConfig) -> Self {
        let capacity = config.capacity;
        let sdr_cfg = config.sdr.clone();
        let wal_cfg = config.wal.clone();
        let alru_cfg = config.attention_lru.clone();

        let moka: MokaCache<u64, Arc<RwLock<Memory>>> = MokaCache::builder()
            .max_capacity(capacity as u64)
            .time_to_live(std::time::Duration::from_secs(7 * 86400))
            .time_to_idle(std::time::Duration::from_secs(86400))
            .eviction_listener(|_k, _v, cause| {
                tracing::trace!("L1 moka eviction: cause={:?}", cause);
            })
            .build();

        let hasher = FoldBuildHasher::default();

        let wal = Arc::new(Wal::new(wal_cfg.clone()));
        let sdr = Arc::new(SDRCache::new(sdr_cfg.clone()));

        let cache = Self {
            config: L1CacheConfig {
                capacity,
                attention_lru: alru_cfg.clone(),
                sdr: sdr_cfg,
                wal: wal_cfg.clone(),
            },
            primary: Arc::new(DashMap::with_capacity_and_hasher(capacity, hasher)),
            alias: Arc::new(DashMap::with_hasher(hasher)),
            attention_lru: Arc::new(AttentionLRU::new(alru_cfg)),
            moka,
            wal,
            sdr,
            insert_count: std::sync::atomic::AtomicU64::new(0),
        };

        // Replay WAL on startup to restore from previous session
        cache.replay_from_wal();

        cache
    }

    /// Replay WAL entries into the cache on construction.
    /// Deserializes each Insert entry and populates primary, alias, moka,
    /// attention_lru, and sdr structures without re-writing to the WAL.
    fn replay_from_wal(&self) {
        let entries = self.wal.read_entries();
        if entries.is_empty() {
            return;
        }
        for entry in &entries {
            if entry.entry_type != WalEntryType::Insert {
                continue;
            }
            let input: MemoryInput = match bincode::deserialize(&entry.data) {
                Ok(v) => v,
                Err(_) => continue,
            };

            let memory = Memory::from_input(input);
            let id = memory.id;
            let memory_arc = Arc::new(RwLock::new(memory));

            self.primary.insert(id, memory_arc.clone());
            self.moka.insert(id, memory_arc.clone());
            self.attention_lru.record_insertion(&memory_arc.read());

            if let Some(ref alias) = memory_arc.read().alias.clone() {
                let mut ids = self
                    .alias
                    .entry(alias.clone())
                    .or_insert_with(|| smallvec::smallvec![]);
                ids.push(id);
            }

            let sdr_mem = memory_arc.read().clone();
            let _ = self.sdr.insert(&sdr_mem);
        }
    }

    pub fn builder() -> L1Config {
        L1Config::default()
    }

    /// Fast ID lookup — DashMap only, no attention tracking.
    /// Returns `None` if the ID is absent from the primary hash table.
    ///
    /// This is the hot path for ID-based reads. Callers that need
    /// attention/access tracking should use `get_by_id_and_record()` instead.
    pub fn get_by_id_fast(&self, id: u64) -> Option<Arc<RwLock<Memory>>> {
        self.primary.get(&id).map(|entry| entry.value().clone())
    }

    /// ID lookup with attention-LRU hit recording.
    /// Slightly slower than `get_by_id_fast` but keeps eviction metadata correct.
    pub fn get_by_id(&self, id: u64) -> Option<Arc<RwLock<Memory>>> {
        let result = self.get_by_id_fast(id);
        if result.is_some() {
            self.attention_lru.record_hit(id);
        }
        result
    }

    pub fn get_by_alias(&self, alias: &str) -> Vec<Arc<RwLock<Memory>>> {
        let mut results = Vec::new();
        if let Some(ids) = self.alias.get(alias) {
            for &id in ids.iter() {
                if let Some(entry) = self.primary.get(&id) {
                    self.attention_lru.record_hit(id);
                    results.push(entry.value().clone());
                }
            }
        }
        results
    }

    /// Insert a pre-built Memory directly (preserving its ID).
    /// Used by MemoryRouter::write to keep IDs consistent across layers.
    pub fn insert_memory(&self, memory: Memory) -> MemoryResult<u64> {
        let memory_id = memory.id;
        let memory_arc = Arc::new(RwLock::new(memory));

        if self.primary.len() >= self.config.capacity {
            if let Some(evict_id) = self.select_eviction_candidate() {
                self.remove(evict_id);
            }
        }

        self.primary.insert(memory_id, memory_arc.clone());
        self.moka.insert(memory_id, memory_arc.clone());
        self.attention_lru.record_insertion(&memory_arc.read());

        if let Some(ref a) = memory_arc.read().alias.clone() {
            let mut ids = self
                .alias
                .entry(a.clone())
                .or_insert_with(|| smallvec::smallvec![]);
            ids.push(memory_id);
        }

        let _ = self.wal.append(&MemoryInput {
            content: memory_arc.read().content.clone(),
            importance: memory_arc.read().metadata.importance,
            alias: memory_arc.read().alias.clone(),
            tags: memory_arc.read().tags.clone(),
            layer: memory_arc.read().metadata.layer,
        });

        let sdr_mem = memory_arc.read().clone();
        self.sdr.insert(&sdr_mem).ok();

        let cnt = self
            .insert_count
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        if cnt.is_multiple_of(100) {
            self.maintenance();
        }

        Ok(memory_id)
    }

    pub fn insert(&self, input: MemoryInput) -> MemoryResult<u64> {
        self.insert_memory(Memory::from_input(input))
    }

    pub fn read(&self, query: &Query) -> MemoryResult<Option<Memory>> {
        if let Some(id) = query.hash_key() {
            // Hot path: use get_by_id_fast() to avoid attention_lru tracking overhead
            if let Some(cached) = self.get_by_id_fast(id) {
                // Read lock only — no write lock, no record_access syscall.
                // Eviction-quality impact is negligible (AttentionLRU handles its own
                // tracking via separate paths); the speed gain is substantial.
                let mem = cached.read();
                return Ok(Some(mem.clone()));
            }
        }

        if let Some(alias) = query.alias() {
            let results = self.get_by_alias(alias);
            if !results.is_empty() {
                let mem = results[0].read();
                return Ok(Some(mem.clone()));
            }
        }

        if query.embedding().is_some() && self.sdr.is_enabled() {
            let sdr_results = self.sdr.search(query).ok().unwrap_or_default();
            if let Some(&(id, _)) = sdr_results.first() {
                if let Some(cached) = self.get_by_id_fast(id) {
                    let mem = cached.read();
                    return Ok(Some(mem.clone()));
                }
            }
        }

        if let Some(text) = query.text() {
            if self.sdr.is_enabled() {
                let sdr_results = self.sdr.search(query).ok().unwrap_or_default();
                if let Some(&(id, _)) = sdr_results.first() {
                    if let Some(cached) = self.get_by_id_fast(id) {
                        let mem = cached.read();
                        return Ok(Some(mem.clone()));
                    }
                }
            }

            if let Some(results) = self.search_by_text(text) {
                if let Some(mem) = results.first() {
                    let mem = mem.read();
                    return Ok(Some(mem.clone()));
                }
            }
        }

        Ok(None)
    }

    fn search_by_text(&self, text: &str) -> Option<Vec<Arc<RwLock<Memory>>>> {
        let normalized = text.to_lowercase();
        let mut results: Vec<(Arc<RwLock<Memory>>, u64)> = Vec::new();

        for entry in self.primary.iter() {
            let mem = entry.value().read();
            let mem_text = mem.content_text().to_lowercase();
            if mem_text.contains(&normalized) {
                let score = mem.metadata.access_count;
                drop(mem);
                results.push((entry.value().clone(), score));
            }
        }

        if results.is_empty() {
            None
        } else {
            results.sort_by(|a, b| b.1.cmp(&a.1));
            results.truncate(5);
            Some(results.into_iter().map(|(mem, _)| mem).collect())
        }
    }

    pub fn remove(&self, id: u64) {
        self.primary.remove(&id);
        self.moka.invalidate(&id);
        self.attention_lru.remove(id);
        self.sdr.remove(id);

        self.alias.retain(|_key, ids| {
            ids.retain(|i| *i != id);
            !ids.is_empty()
        });
    }

    pub fn get_moka(&self) -> &MokaCache<u64, Arc<RwLock<Memory>>> {
        &self.moka
    }

    pub fn get_wal(&self) -> &Wal {
        &self.wal
    }

    pub fn get_sdr(&self) -> &SDRCache {
        &self.sdr
    }

    pub fn get_attention_lru(&self) -> &AttentionLRU {
        &self.attention_lru
    }

    pub fn flush_wal(&self) {
        self.wal.flush();
    }

    fn select_eviction_candidate(&self) -> Option<u64> {
        // Try Attention-LRU scoring first
        let candidates = self.attention_lru.eviction_candidates(3);
        if let Some(id) = candidates.into_iter().next() {
            return Some(id);
        }

        // Fallback: FIFO eviction — remove the entry with the lowest ID
        // Since IDs are monotonically increasing, this is a simple LRU approximation
        let min_id = self.primary.iter().map(|e| *e.key()).min();
        min_id
    }

    fn maintenance(&self) {
        let current = self.primary.len();
        if current > self.config.capacity {
            let to_evict = current - self.config.capacity;
            for _ in 0..to_evict {
                if let Some(id) = self.select_eviction_candidate() {
                    self.remove(id);
                }
            }
        }
    }

    pub fn len(&self) -> usize {
        self.primary.len()
    }

    pub fn is_empty(&self) -> bool {
        self.primary.is_empty()
    }

    pub fn capacity(&self) -> usize {
        self.config.capacity
    }

    pub fn clear(&self) {
        self.primary.clear();
        self.moka.invalidate_all();
        self.alias.clear();
        self.attention_lru.clear();
        self.sdr.clear();
        self.insert_count
            .store(0, std::sync::atomic::Ordering::Relaxed);
    }

    pub fn stats(&self) -> L1Stats {
        L1Stats {
            entries: self.primary.len(),
            capacity: self.config.capacity,
            wal_entries: self.wal.total_entries(),
            sdr_entries: self.sdr.len(),
            alias_count: self.alias.len(),
        }
    }
}

impl Drop for L1Cache {
    fn drop(&mut self) {
        self.wal.shutdown();
    }
}

#[derive(Default)]
pub struct L1Config {
    inner: L1CacheConfig,
}

impl L1Config {
    pub fn capacity(mut self, cap: usize) -> Self {
        self.inner.capacity = cap;
        self
    }

    pub fn with_attention_lru(mut self, config: AttentionLRUConfig) -> Self {
        self.inner.attention_lru = config;
        self
    }

    pub fn with_sdr(mut self, config: SDRConfig) -> Self {
        self.inner.sdr = config;
        self
    }

    pub fn with_wal(mut self, config: WalConfig) -> Self {
        self.inner.wal = config;
        self
    }

    pub fn enable_sdr(mut self) -> Self {
        self.inner.sdr.enabled = true;
        self
    }

    pub fn build(self) -> L1Cache {
        L1Cache::new(self.inner)
    }
}

#[derive(Debug, Clone)]
pub struct L1Stats {
    pub entries: usize,
    pub capacity: usize,
    pub wal_entries: u64,
    pub sdr_entries: usize,
    pub alias_count: usize,
}

impl std::fmt::Display for L1Stats {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "L1Cache(entries={}/{}, wal={}, sdr={}, aliases={})",
            self.entries, self.capacity, self.wal_entries, self.sdr_entries, self.alias_count
        )
    }
}

impl Memory {
    pub fn from_input(input: MemoryInput) -> Self {
        let id = crate::core::generate_memory_id();
        Self::from_input_with_id(input, id)
    }

    /// Create a Memory from input with an explicitly provided ID.
    /// Used by MemoryRouter to keep IDs consistent across layers.
    pub fn from_input_with_id(input: MemoryInput, id: u64) -> Self {
        let now = crate::core::now_nanos();
        Self {
            id,
            content: input.content,
            alias: input.alias,
            tags: input.tags,
            metadata: crate::core::MemoryMeta {
                importance: input.importance,
                created_at: now,
                last_access: now,
                layer: input.layer,
                ..Default::default()
            },
        }
    }

    pub fn content_text(&self) -> String {
        match &self.content {
            crate::core::MemoryContent::Fact(f) => {
                format!("{} {} {}", f.subject, f.predicate, f.object)
            }
            crate::core::MemoryContent::Summary(s) => s.clone(),
            crate::core::MemoryContent::Fingerprint(_) => String::new(),
            crate::core::MemoryContent::Embedding(_) => String::new(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::Fact;
    use crate::core::MemoryContent;
    use tokio;

    fn make_cache() -> L1Cache {
        L1Cache::builder().capacity(100).build()
    }

    fn make_memory(text: &str) -> MemoryInput {
        MemoryInput::new(MemoryContent::Fact(Fact::new("user", "says", text)))
    }

    #[test]
    fn test_l1_new() {
        let cache = make_cache();
        assert!(cache.is_empty());
        assert_eq!(cache.capacity(), 100);
    }

    #[test]
    fn test_l1_insert_and_get_by_id() {
        let cache = make_cache();
        let input = make_memory("hello world");
        let id = cache.insert(input).unwrap();
        assert_eq!(cache.len(), 1);

        let mem = cache.get_by_id(id);
        assert!(mem.is_some());
    }

    #[test]
    fn test_l1_read_by_id() {
        let cache = make_cache();
        let input = make_memory("test data");
        let id = cache.insert(input).unwrap();

        let query = Query::by_id(id);
        let result = cache.read(&query).unwrap();
        assert!(result.is_some());
        assert_eq!(result.unwrap().id, id);
    }

    #[test]
    fn test_l1_read_not_found() {
        let cache = make_cache();
        let query = Query::by_id(999);
        let result = cache.read(&query).unwrap();
        assert!(result.is_none());
    }

    #[test]
    fn test_l1_insert_with_alias() {
        let cache = make_cache();
        let input = MemoryInput::new(MemoryContent::Fact(Fact::new("project", "name", "entropy")))
            .with_alias("main_project");

        cache.insert(input).unwrap();
        let results = cache.get_by_alias("main_project");
        assert_eq!(results.len(), 1);
    }

    #[test]
    fn test_l1_remove() {
        let cache = make_cache();
        let id = cache.insert(make_memory("to_remove")).unwrap();
        assert_eq!(cache.len(), 1);
        cache.remove(id);
        assert_eq!(cache.len(), 0);
    }

    #[test]
    fn test_l1_clear() {
        let cache = make_cache();
        for i in 0..5 {
            cache.insert(make_memory(&format!("item {}", i))).unwrap();
        }
        assert_eq!(cache.len(), 5);
        cache.clear();
        assert_eq!(cache.len(), 0);
    }

    #[test]
    fn test_l1_capacity_limit() {
        let cache = L1Cache::builder().capacity(3).build();

        for i in 0..10 {
            let input = MemoryInput::new(MemoryContent::Fact(Fact::new("k", "v", i.to_string())));
            cache.insert(input).unwrap();
        }
        assert!(cache.len() <= 3);
    }

    #[test]
    fn test_l1_stats() {
        let cache = make_cache();
        cache.insert(make_memory("stats")).unwrap();
        let stats = cache.stats();
        assert_eq!(stats.entries, 1);
        assert_eq!(stats.capacity, 100);
    }

    #[test]
    fn test_l1_moka_cache() {
        let cache = make_cache();
        let id = cache.insert(make_memory("moka_test")).unwrap();
        let moka = cache.get_moka();
        let entry = moka.get(&id);
        assert!(entry.is_some());
    }

    #[test]
    fn test_l1_wal_integration() {
        let cache = make_cache();
        cache.insert(make_memory("wal_test")).unwrap();
        cache.flush_wal();
        assert!(cache.get_wal().total_entries() > 0);
    }

    #[test]
    fn test_l1_read_by_alias() {
        let cache = make_cache();
        let input = MemoryInput::new(MemoryContent::Fact(Fact::new(
            "service", "status", "active",
        )))
        .with_alias("svc_active");
        cache.insert(input).unwrap();

        let query = Query::by_alias("svc_active");
        let result = cache.read(&query).unwrap();
        assert!(result.is_some());
    }

    #[test]
    fn test_l1_sdr_integration() {
        let cache = L1Cache::builder()
            .capacity(100)
            .enable_sdr()
            .with_sdr(SDRConfig {
                enabled: true,
                dimension: 1024,
                capacity: 100,
                sparsity: 0.1,
            })
            .build();

        let input = MemoryInput::new(MemoryContent::Fact(Fact::new(
            "user",
            "prefers",
            "dark mode",
        )));
        cache.insert(input).unwrap();
        assert!(cache.get_sdr().is_enabled());
    }

    #[tokio::test]
    async fn test_l1_concurrent_inserts() {
        let cache = Arc::new(make_cache());
        let mut handles = Vec::new();

        for i in 0..10 {
            let c = cache.clone();
            handles.push(tokio::spawn(async move {
                let input = make_memory(&format!("concurrent {}", i));
                c.insert(input).unwrap();
            }));
        }

        for h in handles {
            h.await.unwrap();
        }

        assert_eq!(cache.len(), 10);
    }

    #[test]
    fn test_l1_display_stats() {
        let cache = make_cache();
        cache.insert(make_memory("display")).unwrap();
        let stats = format!("{}", cache.stats());
        assert!(stats.contains("L1Cache"));
        assert!(stats.contains("entries=1"));
    }
}
