use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, RwLock, Weak};

use crate::consolidation::{ConsolidationConfig, ConsolidationEngine, ConsolidationStats};
use crate::core::traits::MemoryLayer;
use crate::core::types::{
    Fact, KnowledgeSource, LayerId, Memory, MemoryContent, MemoryInput, Query,
};
use crate::core::MemoryResult;
use crate::l0::{ReflectionEngine, ReflectionEvent};
use crate::l1::L1Cache;
use crate::l2::L2Engine;
use crate::l3::L3Engine;
use crate::orchestrator::MemoryOrchestrator;
use crate::router::MemoryRouter;

/// Thread-safe operation counters for MemoryStore.
#[derive(Debug, Default)]
pub struct MemoryStoreCounters {
    pub stores: AtomicU64,
    pub recalls: AtomicU64,
    pub recall_hits: AtomicU64,
    pub recall_misses: AtomicU64,
    pub forgets: AtomicU64,
    pub consolidations: AtomicU64,
}

impl MemoryStoreCounters {
    pub fn snapshot(&self) -> MemoryStoreCounterSnapshot {
        MemoryStoreCounterSnapshot {
            stores: self.stores.load(Ordering::Relaxed),
            recalls: self.recalls.load(Ordering::Relaxed),
            recall_hits: self.recall_hits.load(Ordering::Relaxed),
            recall_misses: self.recall_misses.load(Ordering::Relaxed),
            forgets: self.forgets.load(Ordering::Relaxed),
            consolidations: self.consolidations.load(Ordering::Relaxed),
        }
    }

    pub fn recall_hit_rate(&self) -> f64 {
        let total = self.recalls.load(Ordering::Relaxed);
        if total == 0 {
            return 0.0;
        }
        self.recall_hits.load(Ordering::Relaxed) as f64 / total as f64
    }
}

#[derive(Debug, Clone, Default)]
pub struct MemoryStoreCounterSnapshot {
    pub stores: u64,
    pub recalls: u64,
    pub recall_hits: u64,
    pub recall_misses: u64,
    pub forgets: u64,
    pub consolidations: u64,
}

/// Aggregated statistics across all memory layers.
#[derive(Debug, Clone, Default)]
pub struct MemoryStoreStats {
    pub l1_entries: usize,
    pub l2_entries: usize,
    pub l3_entries: usize,
    pub total_entries: usize,
    pub l3_tokens_used_today: usize,
    pub l3_storage_bytes: usize,
    pub last_consolidation: Option<i64>,
    /// Operation counters snapshot.
    pub counters: MemoryStoreCounterSnapshot,
    /// L2 HNSW entry count (for index efficiency tracking).
    pub hnsw_entries: usize,
}

/// High-level facade over the three-tier memory system.
///
/// `MemoryStore` provides a simple API for storing and recalling memories,
/// with automatic consolidation, deduplication, and cross-layer orchestration.
pub struct MemoryStore {
    router: Arc<MemoryRouter>,
    consolidation: ConsolidationEngine,
    last_consolidation: std::sync::atomic::AtomicI64,
    counters: MemoryStoreCounters,
    /// Optional orchestrator for write-path conflict resolution (知行合一).
    ///
    /// Weak reference breaks the ownership cycle: the orchestrator holds an
    /// `Arc<MemoryStore>` while the store only holds a weak back-pointer.
    orchestrator: RwLock<Option<Weak<MemoryOrchestrator>>>,
    /// Optional L0 reflection engine (省察回路). Weak reference mirrors the
    /// orchestrator pattern: the engine holds an `Arc<MemoryStore>`, so the
    /// store keeps only a weak back-pointer to avoid reference cycles.
    soul: RwLock<Option<Weak<ReflectionEngine>>>,
}

impl MemoryStore {
    pub fn new(l1: L1Cache, l2: Arc<L2Engine>, l3: L3Engine, config: ConsolidationConfig) -> Self {
        let router = Arc::new(MemoryRouter::new(l1, l2, l3));
        Self {
            router,
            consolidation: ConsolidationEngine::new(config),
            last_consolidation: std::sync::atomic::AtomicI64::new(0),
            counters: MemoryStoreCounters::default(),
            orchestrator: RwLock::new(None),
            soul: RwLock::new(None),
        }
    }

    /// Attach an orchestrator so that `store()` runs write-path conflict
    /// resolution (Overturn/Sublimate/Supplement) before persisting.
    ///
    /// This is the "知行合一" wiring: knowledge (知) is aligned with
    /// existing knowledge through action (行) on every write.
    pub fn attach_orchestrator(&self, orchestrator: Arc<MemoryOrchestrator>) {
        *self.orchestrator.write().unwrap() = Some(Arc::downgrade(&orchestrator));
    }

    /// Whether a live orchestrator is attached.
    pub fn has_orchestrator(&self) -> bool {
        self.orchestrator
            .read()
            .unwrap()
            .as_ref()
            .and_then(|w| w.upgrade())
            .is_some()
    }

    fn orchestrator(&self) -> Option<Arc<MemoryOrchestrator>> {
        self.orchestrator
            .read()
            .unwrap()
            .as_ref()
            .and_then(|w| w.upgrade())
    }

    /// Attach the L0 reflection engine so that `store()`/`recall()`/
    /// `consolidate_recursive()` emit events into the 省察 loop.
    pub fn attach_soul(&self, soul: Arc<ReflectionEngine>) {
        *self.soul.write().unwrap() = Some(Arc::downgrade(&soul));
    }

    fn soul(&self) -> Option<Arc<ReflectionEngine>> {
        self.soul.read().unwrap().as_ref().and_then(|w| w.upgrade())
    }

    fn notify_soul(&self, event: ReflectionEvent) {
        if let Some(soul) = self.soul() {
            soul.notify(event);
        }
    }

    /// Access the underlying router (for advanced operations).
    pub fn router(&self) -> &MemoryRouter {
        &self.router
    }

    /// Store a memory. If `auto_dedup_on_store` is enabled, checks L2 for
    /// similar memories and merges them into an L3 summary.
    ///
    /// If `align_on_write` is enabled AND an orchestrator is attached,
    /// write-path conflict resolution (Overturn/Sublimate/Supplement) runs
    /// before persisting — the 知行合一 loop on every write.
    ///
    /// Returns the memory ID of the stored entry.
    pub fn store(&self, input: MemoryInput) -> MemoryResult<u64> {
        let new_importance = input.importance;
        let text = match &input.content {
            MemoryContent::Fact(f) => format!("{} {} {}", f.subject, f.predicate, f.object),
            MemoryContent::Summary(s) => s.clone(),
            _ => String::new(),
        };

        // P0: write-path conflict resolution (格物致知).
        let resolutions = if self.consolidation.config().align_on_write {
            self.orchestrator()
                .map(|orch| orch.pre_resolve(&input, &self.router))
                .unwrap_or_default()
        } else {
            Vec::new()
        };

        if self.consolidation.config().auto_dedup_on_store && !text.is_empty() {
            let similar = self
                .consolidation
                .find_similar_in_l2(self.router.l2(), &text);
            if similar.len() >= self.consolidation.config().min_merge_group {
                let similar_ids: Vec<u64> = similar.into_iter().map(|(id, _)| id).collect();
                let merged = self.consolidation.merge_into_l3(
                    self.router.l2(),
                    self.router.l3(),
                    &similar_ids,
                )?;
                if let Some(merged_id) = merged {
                    if let Some(mem) = self.router.l3().get_by_id(merged_id) {
                        self.promote_to_top(mem);
                    }
                    if !resolutions.is_empty() {
                        if let Some(orch) = self.orchestrator() {
                            orch.apply_resolutions(
                                &text,
                                new_importance,
                                &self.router,
                                &resolutions,
                                merged_id,
                            );
                        }
                    }
                    self.notify_soul(ReflectionEvent::MemoryStored {
                        memory_id: merged_id,
                        source: KnowledgeSource::Consolidation,
                    });
                    return Ok(merged_id);
                }
            }
        }

        self.counters.stores.fetch_add(1, Ordering::Relaxed);
        let new_id = self.router.write(input)?;

        if !resolutions.is_empty() {
            if let Some(orch) = self.orchestrator() {
                orch.apply_resolutions(&text, new_importance, &self.router, &resolutions, new_id);
            }
        }

        self.notify_soul(ReflectionEvent::MemoryStored {
            memory_id: new_id,
            source: KnowledgeSource::ExternalInput,
        });

        Ok(new_id)
    }

    /// Distill a raw interaction transcript into facts and store them.
    /// Returns the number of facts persisted; 0 when extraction yields nothing.
    /// Errors are returned as MemoryError (callers should log and continue).
    pub fn distill_and_store(&self, raw_text: &str) -> MemoryResult<usize> {
        let facts = self.consolidation.extract_from_interaction(raw_text);
        let mut stored = 0usize;
        for (subject, predicate, object, confidence) in facts {
            let input =
                MemoryInput::new(MemoryContent::Fact(Fact::new(subject, predicate, object)))
                    .with_importance(confidence.clamp(0.0, 1.0));
            self.store(input)?;
            stored += 1;
        }
        Ok(stored)
    }

    pub(crate) fn mark_superseded(&self, old_id: u64, new_id: u64) {
        let now = crate::core::now_nanos();
        let marked_l2 = self.router.l2().update_metadata(old_id, |meta| {
            meta.superseded_by = Some(new_id);
            meta.superseded_at = now;
        });
        let marked_l3 = self.router.l3().update_metadata(old_id, |meta| {
            meta.superseded_by = Some(new_id);
            meta.superseded_at = now;
        });
        if marked_l2 || marked_l3 {
            self.router.l1().remove(old_id);
        }
    }

    /// Access the live operation counters.
    pub fn counters(&self) -> &MemoryStoreCounters {
        &self.counters
    }

    /// Recall memories matching the query.
    /// Returns ranked results from all layers.
    pub fn recall(&self, query: &Query, k: usize) -> MemoryResult<Vec<(Memory, f32, LayerId)>> {
        let results = self.router.search(query, k);
        let mut memories: Vec<(Memory, f32, LayerId)> = Vec::with_capacity(results.len());
        let mut seen_content: std::collections::HashSet<String> = std::collections::HashSet::new();

        for (id, dist, layer) in &results {
            let mem = match layer {
                LayerId::L2 => self.router.l2().get_by_id(*id),
                LayerId::L3 => self.router.l3().get_by_id(*id),
                _ => None,
            };
            if let Some(m) = mem {
                if !query.matches(&m) {
                    continue;
                }
                let content_key = m.content_text();
                if seen_content.contains(&content_key) {
                    continue;
                }
                seen_content.insert(content_key);
                memories.push((m, *dist, *layer));
            }
        }

        // Sort by distance (ascending — closer = more relevant)
        // Use total_cmp to handle NaN distances from removed HNSW nodes
        memories.sort_by(|a, b| a.1.total_cmp(&b.1));
        memories.truncate(k);

        self.counters.recalls.fetch_add(1, Ordering::Relaxed);
        if memories.is_empty() {
            self.counters.recall_misses.fetch_add(1, Ordering::Relaxed);
        } else {
            self.counters.recall_hits.fetch_add(1, Ordering::Relaxed);
        }

        if let Some((top, _, _)) = memories.first() {
            self.notify_soul(ReflectionEvent::MemoryRecalled {
                memory_id: top.id,
                context: query.text.clone().unwrap_or_default(),
            });
        }

        Ok(memories)
    }

    /// Recall by ID.
    ///
    /// Fast path: directly checks L1 without building a Query or going through
    /// the full cascade. Falls through to the cascade only on L1 miss.
    pub fn recall_by_id(&self, id: u64) -> MemoryResult<Option<Memory>> {
        // Fast path: DashMap lookup in L1 — no Query construction, no cascade.
        if let Some(cached) = self.router.l1().get_by_id_fast(id) {
            let mem = cached.read();
            return Ok(Some(mem.clone()));
        }
        // L1 miss — fall through to the parallel cascade (L2∥L3 with promotion).
        let query = Query::by_id(id);
        self.router.read_parallel(&query)
    }

    /// Recall by text.
    pub fn recall_by_text(
        &self,
        text: &str,
        k: usize,
    ) -> MemoryResult<Vec<(Memory, f32, LayerId)>> {
        let query = Query::by_text(text);
        self.recall(&query, k)
    }

    /// Remove a memory from all layers by ID.
    pub fn forget(&self, id: u64) -> bool {
        self.counters.forgets.fetch_add(1, Ordering::Relaxed);
        let query = Query::by_id(id);
        self.router.remove(&query)
    }

    /// Run a consolidation cycle: find similar L2 entries, merge into L3 summaries.
    /// Returns (merged_count, dedup_count).
    pub fn consolidate(&self) -> (usize, usize) {
        self.counters.consolidations.fetch_add(1, Ordering::Relaxed);
        let result = self
            .consolidation
            .consolidate(self.router.l2(), self.router.l3());
        self.last_consolidation.store(
            crate::core::now_nanos(),
            std::sync::atomic::Ordering::Relaxed,
        );
        result
    }

    /// Run recursive consolidation with multiple rounds:
    /// Round 1: L2 facts → L3 summaries.
    /// Rounds 2+: L3 summaries → higher-level summaries.
    /// After all rounds, applies the configured forgetting strategy.
    pub fn consolidate_recursive(&self) -> ConsolidationStats {
        self.counters.consolidations.fetch_add(1, Ordering::Relaxed);
        let result = self
            .consolidation
            .consolidate_recursive(self.router.l2(), self.router.l3());
        self.last_consolidation.store(
            crate::core::now_nanos(),
            std::sync::atomic::Ordering::Relaxed,
        );
        self.notify_soul(ReflectionEvent::ConsolidationDone {
            stats: result.clone(),
        });
        result
    }

    /// Remove L2 facts below the given importance threshold.
    /// Returns count of removed entries.
    pub fn forget_below_importance(&self, threshold: f32) -> (usize, usize) {
        let l2 = self
            .consolidation
            .forget_below_importance_l2(self.router.l2(), threshold);
        let l3 = self
            .consolidation
            .forget_below_importance_l3(self.router.l3(), threshold);
        (l2, l3)
    }

    /// Remove memories older than ttl_nanos from all layers.
    /// Returns count of removed entries.
    pub fn forget_older_than(&self, ttl_nanos: i64) -> (usize, usize) {
        let l2 = self
            .consolidation
            .forget_older_than_l2(self.router.l2(), ttl_nanos);
        let l3 = self
            .consolidation
            .forget_older_than_l3(self.router.l3(), ttl_nanos);
        (l2, l3)
    }

    /// Get comprehensive statistics.
    pub fn stats(&self) -> MemoryStoreStats {
        let l2_stats = self.router.l2().stats();
        let l3_stats = self.router.l3().budget_stats();
        MemoryStoreStats {
            l1_entries: self.router.l1().len(),
            l2_entries: l2_stats.stored,
            l3_entries: self.router.l3().len(),
            total_entries: self.router.len(),
            l3_tokens_used_today: l3_stats.tokens_used_today,
            l3_storage_bytes: l3_stats.storage_bytes,
            last_consolidation: {
                let val = self
                    .last_consolidation
                    .load(std::sync::atomic::Ordering::Relaxed);
                if val > 0 {
                    Some(val)
                } else {
                    None
                }
            },
            counters: self.counters.snapshot(),
            hnsw_entries: l2_stats.vectors,
        }
    }

    fn promote_to_top(&self, memory: Memory) {
        // Promote to L1
        let l1_input = MemoryInput {
            content: memory.content.clone(),
            importance: memory.metadata.importance,
            alias: memory.alias.clone(),
            tags: memory.tags.clone(),
            layer: LayerId::L1,
        };
        self.router.l1().insert(l1_input).ok();

        // Promote to L2
        let l2_input = MemoryInput {
            content: memory.content.clone(),
            importance: memory.metadata.importance,
            alias: memory.alias.clone(),
            tags: memory.tags.clone(),
            layer: LayerId::L2,
        };
        self.router.l2().insert(l2_input).ok();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::consolidation::ConsolidationConfig;
    use crate::core::Fact;
    use crate::l2::{HnswConfig, L2Config};
    use crate::l3::{BudgetConfig, L3Config};
    use tempfile::tempdir;

    fn make_store() -> MemoryStore {
        let l1 = L1Cache::builder().capacity(100).build();
        let l2 = L2Engine::new(L2Config {
            hnsw: HnswConfig {
                dimension: 16,
                ..Default::default()
            },
            dimension: 16,
            ..Default::default()
        });
        let dir = tempdir().unwrap();
        let l3 = L3Engine::new(L3Config {
            storage_path: dir.path().join("store.bin"),
            budget: BudgetConfig {
                daily_token_limit: 1_000_000,
                annual_storage_limit: 10_000_000,
            },
            ..Default::default()
        });
        MemoryStore::new(l1, Arc::new(l2), l3, ConsolidationConfig::default())
    }

    #[test]
    fn test_store_and_recall_by_id() {
        let store = make_store();
        let input = MemoryInput::new(MemoryContent::Fact(Fact::new("alice", "knows", "rust")));
        let id = store.store(input).unwrap();
        let result = store.recall_by_id(id).unwrap();
        assert!(result.is_some());
        assert_eq!(result.unwrap().id, id);
    }

    #[test]
    fn test_store_and_recall_by_text() {
        let store = make_store();
        store
            .store(MemoryInput::new(MemoryContent::Fact(Fact::new(
                "bob", "programs", "in rust",
            ))))
            .unwrap();
        let results = store.recall_by_text("rust programming", 5).unwrap();
        assert!(!results.is_empty(), "should find at least one result");
        assert_eq!(results[0].2, LayerId::L2, "should originate from L2");
    }

    #[test]
    fn test_forget_removes_from_all_layers() {
        let store = make_store();
        let input = MemoryInput::new(MemoryContent::Fact(Fact::new("temp", "data", "remove")));
        let id = store.store(input).unwrap();
        assert!(store.recall_by_id(id).unwrap().is_some());

        assert!(store.forget(id));
        assert!(store.recall_by_id(id).unwrap().is_none());
    }

    #[test]
    fn test_stats_reports_all_layers() {
        let store = make_store();
        store
            .store(MemoryInput::new(MemoryContent::Fact(Fact::new(
                "stats", "test", "entry",
            ))))
            .unwrap();

        let stats = store.stats();
        assert_eq!(stats.total_entries, 3); // one in each layer
        assert_eq!(stats.l1_entries, 1);
        assert_eq!(stats.l2_entries, 1);
        assert_eq!(stats.l3_entries, 1);
        assert!(stats.last_consolidation.is_none());
    }

    #[test]
    fn test_consolidation_updates_timestamp() {
        let store = make_store();
        assert!(store.stats().last_consolidation.is_none());
        store.consolidate();
        assert!(store.stats().last_consolidation.is_some());
    }

    #[test]
    fn test_store_with_auto_dedup_disabled() {
        // With auto_dedup_on_store = false, store() should work normally
        let l1 = L1Cache::builder().capacity(100).build();
        let l2 = L2Engine::new(L2Config {
            hnsw: HnswConfig {
                dimension: 16,
                ..Default::default()
            },
            dimension: 16,
            ..Default::default()
        });
        let dir = tempdir().unwrap();
        let l3 = L3Engine::new(L3Config {
            storage_path: dir.path().join("nodup.bin"),
            budget: BudgetConfig {
                daily_token_limit: 1_000_000,
                annual_storage_limit: 10_000_000,
            },
            ..Default::default()
        });
        let store = MemoryStore::new(
            l1,
            Arc::new(l2),
            l3,
            ConsolidationConfig {
                auto_dedup_on_store: false,
                ..Default::default()
            },
        );

        let id = store
            .store(MemoryInput::new(MemoryContent::Fact(Fact::new(
                "simple", "test", "no_dedup",
            ))))
            .unwrap();
        assert!(store.recall_by_id(id).unwrap().is_some());
    }

    fn make_conflict_engine() -> (
        Arc<MemoryStore>,
        Arc<crate::orchestrator::MemoryOrchestrator>,
    ) {
        let l1 = L1Cache::builder().capacity(100).build();
        let l2 = Arc::new(L2Engine::new(L2Config {
            hnsw: HnswConfig {
                dimension: 16,
                ..Default::default()
            },
            dimension: 16,
            ..Default::default()
        }));
        let dir = tempdir().unwrap();
        let l3 = L3Engine::new(L3Config {
            storage_path: dir.path().join("align.bin"),
            budget: BudgetConfig {
                daily_token_limit: 1_000_000,
                annual_storage_limit: 10_000_000,
            },
            ..Default::default()
        });
        let store = Arc::new(MemoryStore::new(
            l1,
            Arc::new(L2Engine::new(L2Config {
                hnsw: HnswConfig {
                    dimension: 16,
                    ..Default::default()
                },
                dimension: 16,
                ..Default::default()
            })),
            l3,
            ConsolidationConfig {
                align_on_write: true,
                ..Default::default()
            },
        ));
        let resolver = Arc::new(crate::phase1::ConflictResolver::new(Arc::clone(&l2)));
        let quarantine = Arc::new(crate::phase4::QuarantineManager::new(
            Arc::clone(&l2),
            crate::phase4::QuarantineConfig::default(),
        ));
        let orch = Arc::new(crate::orchestrator::MemoryOrchestrator::new(
            Arc::clone(&store),
            resolver,
            quarantine,
        ));
        store.attach_orchestrator(Arc::clone(&orch));
        (store, orch)
    }

    #[test]
    fn test_attach_orchestrator_enables_align_on_write() {
        let (store, _orch) = make_conflict_engine();
        assert!(store.has_orchestrator());

        // A clearly contradictory fact should be Overturned: old memory
        // gets superseded_by pointing at the new ID (never 0).
        let old_id = store
            .store(MemoryInput::new(MemoryContent::Fact(Fact::new(
                "earth", "shape", "flat",
            ))))
            .unwrap();
        let new_id = store
            .store(MemoryInput::new(MemoryContent::Fact(Fact::new(
                "earth", "shape", "round",
            ))))
            .unwrap();
        assert_ne!(old_id, new_id);

        let old_mem = store.router().l2().get_by_id(old_id).unwrap();
        assert_eq!(
            old_mem.metadata.superseded_by,
            Some(new_id),
            "conflicting old memory must be superseded by the new memory"
        );
    }

    #[test]
    fn test_align_sublimate_merges_into_l3() {
        let (store, _orch) = make_conflict_engine();

        // A Summary input carries higher abstraction, pushing CCS into the
        // Sublimate band (0.05, 0.15] — the write path must merge old+new
        // text into L3 and supersede the old memory.
        let old_id = store
            .store(MemoryInput::new(MemoryContent::Fact(Fact::new(
                "earth", "shape", "flat",
            ))))
            .unwrap();
        let new_id = store
            .store(MemoryInput::new(MemoryContent::Summary(
                "earth shape round".into(),
            )))
            .unwrap();
        assert_ne!(old_id, new_id);

        let old_mem = store.router().l2().get_by_id(old_id).unwrap();
        assert_eq!(
            old_mem.metadata.superseded_by,
            Some(new_id),
            "sublimated old memory must be superseded by the new memory"
        );

        let merged = store.router().l3().storage().all().into_iter().find(|m| {
            matches!(m.content, MemoryContent::Summary(_)) && m.content_text().contains("flat")
        });
        assert!(
            merged.is_some(),
            "Sublimate must write a merged summary containing the old text"
        );
    }

    #[test]
    fn test_mark_superseded_preserves_metadata() {
        let (store, _orch) = make_conflict_engine();
        let old_id = store
            .store(MemoryInput::new(MemoryContent::Fact(Fact::new(
                "x", "shape", "flat",
            ))))
            .unwrap();
        let new_id = store
            .store(MemoryInput::new(MemoryContent::Fact(Fact::new(
                "x", "shape", "round",
            ))))
            .unwrap();

        let old_mem = store.router().l2().get_by_id(old_id).unwrap();
        assert_eq!(old_mem.metadata.superseded_by, Some(new_id));
        assert_eq!(old_mem.content_text(), "x shape flat");
        assert!(old_mem.metadata.created_at > 0, "created_at must survive");
        let ids = store.router().l2().hnsw.all_ids();
        let count = ids.iter().filter(|id| **id == old_id).count();
        assert_eq!(count, 1, "mark_superseded must not duplicate HNSW nodes");
    }

    #[test]
    fn test_align_on_write_disabled_no_resolution() {
        let l1 = L1Cache::builder().capacity(100).build();
        let l2 = L2Engine::new(L2Config {
            hnsw: HnswConfig {
                dimension: 16,
                ..Default::default()
            },
            dimension: 16,
            ..Default::default()
        });
        let dir = tempdir().unwrap();
        let l3 = L3Engine::new(L3Config {
            storage_path: dir.path().join("noalign.bin"),
            budget: BudgetConfig {
                daily_token_limit: 1_000_000,
                annual_storage_limit: 10_000_000,
            },
            ..Default::default()
        });
        let store = MemoryStore::new(l1, Arc::new(l2), l3, ConsolidationConfig::default());
        assert!(!store.has_orchestrator());

        let old_id = store
            .store(MemoryInput::new(MemoryContent::Fact(Fact::new(
                "earth", "shape", "flat",
            ))))
            .unwrap();
        let new_id = store
            .store(MemoryInput::new(MemoryContent::Fact(Fact::new(
                "earth", "shape", "round",
            ))))
            .unwrap();
        assert_ne!(old_id, new_id);

        let old_mem = store.router().l2().get_by_id(old_id).unwrap();
        assert!(
            old_mem.metadata.superseded_by.is_none(),
            "align_on_write=false must not touch old memory"
        );
    }

    #[test]
    fn test_recall_by_id_parallel_finds_l3_only_memory() {
        let store = make_store();
        // Seed only into L3 — L1 and L2 miss, so recall must exercise the
        // parallel L2∥L3 cascade and promote the hit up with its ID intact.
        let input = MemoryInput::new(MemoryContent::Summary(
            "deeply archived fact from long ago".into(),
        ));
        let l3_id = store.router().l3().insert(input).unwrap();
        assert!(
            store.router().l1().get_by_id_fast(l3_id).is_none(),
            "precondition: L1 must miss"
        );
        assert!(
            store.router().l2().get_by_id(l3_id).is_none(),
            "precondition: L2 must miss"
        );

        let recalled = store.recall_by_id(l3_id).unwrap();
        let mem = recalled.expect("parallel cascade must find L3-only memory");
        assert_eq!(mem.id, l3_id, "ID must survive promotion");
        assert!(matches!(mem.content, MemoryContent::Summary(_)));

        // Promotion: L1 now caches the same ID, so the next recall is a
        // single DashMap lookup (no cascade).
        assert!(
            store.router().l1().get_by_id_fast(l3_id).is_some(),
            "parallel recall must promote the hit into L1"
        );
    }

    #[test]
    fn test_recall_by_id_skips_superseded() {
        let store = make_store();
        let old_id = store
            .store(MemoryInput::new(MemoryContent::Fact(Fact::new(
                "city",
                "population",
                "old number",
            ))))
            .unwrap();
        let new_id = store
            .store(MemoryInput::new(MemoryContent::Fact(Fact::new(
                "city",
                "population",
                "new number",
            ))))
            .unwrap();

        // Simulate the orchestrator marking the old memory superseded.
        store.mark_superseded(old_id, new_id);

        // The superseded memory is still in L2/L3 but must not surface
        // through recall (Query::matches rejects superseded entries).
        assert!(
            store.recall_by_id(old_id).unwrap().is_none(),
            "superseded memory must not be recalled by ID"
        );
        assert!(store.recall_by_id(new_id).unwrap().is_some());
    }
}
