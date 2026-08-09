use std::collections::BinaryHeap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Mutex;

use parking_lot::RwLock;

use crate::core::Memory;

#[derive(Debug, Clone)]
struct ScoredEntry {
    pub memory_id: u64,
    pub score: f64,
    pub access_count: u64,
}

impl Eq for ScoredEntry {}

impl PartialEq for ScoredEntry {
    fn eq(&self, other: &Self) -> bool {
        self.score == other.score
    }
}

impl PartialOrd for ScoredEntry {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for ScoredEntry {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        self.score
            .partial_cmp(&other.score)
            .unwrap_or(std::cmp::Ordering::Equal)
    }
}

#[derive(Debug, Clone)]
pub struct AttentionLRUConfig {
    pub max_capacity: usize,
    pub alpha: f64,
    pub beta: f64,
    pub decay_lambda: f64,
    pub scan_interval: u64,
}

impl Default for AttentionLRUConfig {
    fn default() -> Self {
        Self {
            max_capacity: 10_000,
            alpha: 0.4,
            beta: 0.6,
            decay_lambda: 0.1,
            scan_interval: 1_000,
        }
    }
}

pub struct AttentionLRU {
    config: AttentionLRUConfig,
    candidates: Mutex<BinaryHeap<ScoredEntry>>,
    scores: RwLock<Vec<(u64, ScoreState)>>,
    insertion_count: AtomicU64,
}

#[derive(Debug, Clone)]
struct ScoreState {
    importance: f64,
    access_count: u64,
    created_at_ns: i64,
    last_access_ns: i64,
}

impl AttentionLRU {
    pub fn new(config: AttentionLRUConfig) -> Self {
        let max = config.max_capacity;
        Self {
            config,
            candidates: Mutex::new(BinaryHeap::with_capacity(max)),
            scores: RwLock::new(Vec::with_capacity(max)),
            insertion_count: AtomicU64::new(0),
        }
    }

    pub fn record_insertion(&self, memory: &Memory) {
        let score = self.compute_score(
            memory.metadata.importance as f64,
            memory.metadata.access_count,
            memory.metadata.created_at,
            memory.metadata.last_access,
        );
        let entry = ScoredEntry {
            memory_id: memory.id,
            score,
            access_count: memory.metadata.access_count,
        };
        self.candidates
            .lock()
            .expect("AttentionLRU lock poisoned")
            .push(entry);
        self.scores.write().push((
            memory.id,
            ScoreState {
                importance: memory.metadata.importance as f64,
                access_count: memory.metadata.access_count,
                created_at_ns: memory.metadata.created_at,
                last_access_ns: memory.metadata.last_access,
            },
        ));
        self.insertion_count.fetch_add(1, Ordering::Relaxed);
    }

    pub fn record_hit(&self, memory_id: u64) {
        if let Some(state) = self
            .scores
            .write()
            .iter_mut()
            .find(|(id, _)| *id == memory_id)
        {
            state.1.access_count += 1;
            state.1.last_access_ns = Self::coarse_now_ns();
        }
    }

    /// Returns a cached nanosecond timestamp.
    /// The syscall (`now_nanos()`) is called once every 1024 invocations,
    /// amortizing the ~50-100ns overhead across thousands of hits.
    /// For eviction scoring, microsecond-level precision is irrelevant,
    /// so this trade-off is safe.
    fn coarse_now_ns() -> i64 {
        use std::sync::atomic::AtomicI64;
        static CALL_COUNT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        static COARSE_NOW: AtomicI64 = AtomicI64::new(0);
        // Fast path: return cached value for 1023 out of 1024 calls.
        if CALL_COUNT.fetch_add(1, Ordering::Relaxed) & 0x3FF != 0 {
            let cached = COARSE_NOW.load(Ordering::Relaxed);
            if cached != 0 {
                return cached;
            }
        }
        // Slow path: actual syscall, every 1024th call.
        let now = crate::core::now_nanos();
        COARSE_NOW.store(now, Ordering::Relaxed);
        now
    }

    pub fn record_access(&self, memory_id: u64) {
        self.record_hit(memory_id);
    }

    fn compute_score(
        &self,
        importance: f64,
        access_count: u64,
        _created_at: i64,
        last_access: i64,
    ) -> f64 {
        let now = crate::core::now_nanos();
        let elapsed_since_access = ((now - last_access).max(1) as f64) / 1_000_000_000.0_f64;
        let recency = (-self.config.decay_lambda * elapsed_since_access).exp();
        let attention = (1.0 + access_count as f64).ln();
        self.config.alpha * recency + self.config.beta * attention + importance * 0.01
    }

    pub fn eviction_candidates(&self, count: usize) -> Vec<u64> {
        let mut candidates = match self.candidates.lock() {
            Ok(c) => c,
            Err(_) => return Vec::new(),
        };
        let mut result = Vec::with_capacity(count.min(candidates.len()));
        for _ in 0..count {
            if let Some(entry) = candidates.pop() {
                let current_score = self.recompute_score(entry.memory_id);
                match current_score {
                    Some(new_score) => {
                        if new_score < 0.1 {
                            result.push(entry.memory_id);
                        } else {
                            candidates.push(ScoredEntry {
                                memory_id: entry.memory_id,
                                score: new_score,
                                access_count: entry.access_count,
                            });
                        }
                    }
                    None => {
                        result.push(entry.memory_id);
                    }
                }
            }
        }
        result
    }

    fn recompute_score(&self, memory_id: u64) -> Option<f64> {
        let scores = self.scores.read();
        let (_, state) = scores.iter().find(|(id, _)| *id == memory_id)?;
        Some(self.compute_score(
            state.importance,
            state.access_count,
            state.created_at_ns,
            state.last_access_ns,
        ))
    }

    pub fn clear(&self) {
        self.scores.write().clear();
        self.candidates
            .lock()
            .expect("AttentionLRU lock poisoned")
            .clear();
        self.insertion_count.store(0, Ordering::Relaxed);
    }

    pub fn remove(&self, memory_id: u64) {
        self.scores.write().retain(|(id, _)| *id != memory_id);
        let mut candidates = self.candidates.lock().expect("AttentionLRU lock poisoned");
        candidates.retain(|e| e.memory_id != memory_id);
    }

    pub fn should_evict(&self, candidate: &Memory, incoming: &Memory) -> bool {
        let candidate_score = self.compute_score(
            candidate.metadata.importance as f64,
            candidate.metadata.access_count,
            candidate.metadata.created_at,
            candidate.metadata.last_access,
        );
        let incoming_score = self.compute_score(
            incoming.metadata.importance as f64,
            incoming.metadata.access_count,
            incoming.metadata.created_at,
            incoming.metadata.last_access,
        );
        incoming_score > candidate_score
    }

    pub fn len(&self) -> usize {
        self.scores.read().len()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    pub fn is_full(&self) -> bool {
        self.len() >= self.config.max_capacity
    }

    pub fn config(&self) -> &AttentionLRUConfig {
        &self.config
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::{Fact, MemoryContent};

    fn make_test_memory(id: u64, importance: f32, access_count: u64) -> Memory {
        Memory {
            id,
            content: MemoryContent::Fact(Fact::new("test", "is", "memory")),
            alias: None,
            tags: Vec::new(),
            metadata: crate::core::types::MemoryMeta {
                access_count,
                importance,
                ..Default::default()
            },
        }
    }

    #[test]
    fn test_new_attention_lru() {
        let config = AttentionLRUConfig::default();
        let lru = AttentionLRU::new(config);
        assert!(lru.is_empty());
        assert!(!lru.is_full());
    }

    #[test]
    fn test_record_and_score() {
        let lru = AttentionLRU::new(AttentionLRUConfig::default());
        let mem = make_test_memory(1, 0.8, 5);
        lru.record_insertion(&mem);
        assert_eq!(lru.len(), 1);
        assert!(!lru.is_empty());
    }

    #[test]
    fn test_eviction_candidates_empty() {
        let lru = AttentionLRU::new(AttentionLRUConfig::default());
        let candidates = lru.eviction_candidates(10);
        assert!(candidates.is_empty());
    }

    #[test]
    fn test_should_evict() {
        let lru = AttentionLRU::new(AttentionLRUConfig::default());
        let low_importance = make_test_memory(1, 0.1, 0);
        let high_importance = make_test_memory(2, 0.9, 10);
        assert!(lru.should_evict(&low_importance, &high_importance));
        assert!(!lru.should_evict(&high_importance, &low_importance));
    }

    #[test]
    fn test_config_defaults() {
        let config = AttentionLRUConfig::default();
        assert_eq!(config.max_capacity, 10_000);
        assert!((config.alpha - 0.4).abs() < 1e-6);
        assert!((config.beta - 0.6).abs() < 1e-6);
    }

    #[test]
    fn test_score_decreases_over_time() {
        let lru = AttentionLRU::new(AttentionLRUConfig::default());
        let recent = Memory {
            id: 1,
            content: MemoryContent::Fact(Fact::new("test", "is", "memory")),
            alias: None,
            tags: Vec::new(),
            metadata: crate::core::types::MemoryMeta {
                last_access: crate::core::now_nanos(),
                access_count: 1,
                importance: 0.5,
                ..Default::default()
            },
        };
        let old = Memory {
            id: 2,
            content: MemoryContent::Fact(Fact::new("test", "is", "memory")),
            alias: None,
            tags: Vec::new(),
            metadata: crate::core::types::MemoryMeta {
                last_access: crate::core::now_nanos() - 3_600_000_000_000,
                access_count: 1,
                importance: 0.5,
                ..Default::default()
            },
        };

        let recent_score = lru.compute_score(
            recent.metadata.importance as f64,
            recent.metadata.access_count,
            recent.metadata.created_at,
            recent.metadata.last_access,
        );
        let old_score = lru.compute_score(
            old.metadata.importance as f64,
            old.metadata.access_count,
            old.metadata.created_at,
            old.metadata.last_access,
        );
        assert!(
            recent_score > old_score,
            "recent memory should score higher than old memory"
        );
    }

    #[test]
    fn test_is_full() {
        let config = AttentionLRUConfig {
            max_capacity: 2,
            ..Default::default()
        };
        let lru = AttentionLRU::new(config);
        let mem1 = make_test_memory(1, 0.5, 0);
        let mem2 = make_test_memory(2, 0.5, 0);
        lru.record_insertion(&mem1);
        assert!(!lru.is_full());
        lru.record_insertion(&mem2);
        assert!(lru.is_full());
    }
}
