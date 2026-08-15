use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

/// Tracks hit/miss rates and latency for a single cache layer.
#[derive(Debug, Default)]
pub struct CacheMetrics {
    hits: AtomicU64,
    misses: AtomicU64,
    total_latency_ns: AtomicU64,
    total_ops: AtomicU64,
}

impl CacheMetrics {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn record_hit(&self, latency_ns: u64) {
        self.hits.fetch_add(1, Ordering::Relaxed);
        self.total_latency_ns
            .fetch_add(latency_ns, Ordering::Relaxed);
        self.total_ops.fetch_add(1, Ordering::Relaxed);
    }

    pub fn record_miss(&self, latency_ns: u64) {
        self.misses.fetch_add(1, Ordering::Relaxed);
        self.total_latency_ns
            .fetch_add(latency_ns, Ordering::Relaxed);
        self.total_ops.fetch_add(1, Ordering::Relaxed);
    }

    pub fn hit_count(&self) -> u64 {
        self.hits.load(Ordering::Relaxed)
    }

    pub fn miss_count(&self) -> u64 {
        self.misses.load(Ordering::Relaxed)
    }

    pub fn total_ops(&self) -> u64 {
        self.total_ops.load(Ordering::Relaxed)
    }

    pub fn hit_rate(&self) -> f64 {
        let hits = self.hits.load(Ordering::Relaxed) as f64;
        let total = self.total_ops.load(Ordering::Relaxed) as f64;
        if total == 0.0 {
            0.0
        } else {
            hits / total
        }
    }

    pub fn avg_latency_ns(&self) -> f64 {
        let total = self.total_ops.load(Ordering::Relaxed) as f64;
        if total == 0.0 {
            0.0
        } else {
            self.total_latency_ns.load(Ordering::Relaxed) as f64 / total
        }
    }

    pub fn reset(&self) {
        self.hits.store(0, Ordering::Relaxed);
        self.misses.store(0, Ordering::Relaxed);
        self.total_latency_ns.store(0, Ordering::Relaxed);
        self.total_ops.store(0, Ordering::Relaxed);
    }
}

/// Aggregate metrics across all memory layers.
#[derive(Debug, Default)]
pub struct MemoryMetrics {
    pub l1_reads: CacheMetrics,
    pub l2_reads: CacheMetrics,
    pub l3_reads: CacheMetrics,
    pub writes: CacheMetrics,
    pub consolidations: AtomicU64,
    pub deduplications: AtomicU64,
    pub l0_generations: CacheMetrics,
    pub rambling_cycles: CacheMetrics,
    pub conflict_resolutions: CacheMetrics,
}

impl MemoryMetrics {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn record_l1_hit(&self, latency_ns: u64) {
        self.l1_reads.record_hit(latency_ns);
    }

    pub fn record_l1_miss(&self, latency_ns: u64) {
        self.l1_reads.record_miss(latency_ns);
    }

    pub fn record_l2_hit(&self, latency_ns: u64) {
        self.l2_reads.record_hit(latency_ns);
    }

    pub fn record_l2_miss(&self, latency_ns: u64) {
        self.l2_reads.record_miss(latency_ns);
    }

    pub fn record_l3_hit(&self, latency_ns: u64) {
        self.l3_reads.record_hit(latency_ns);
    }

    pub fn record_l3_miss(&self, latency_ns: u64) {
        self.l3_reads.record_miss(latency_ns);
    }

    pub fn record_write(&self, latency_ns: u64) {
        self.writes.record_hit(latency_ns);
    }

    pub fn record_consolidation(&self) {
        self.consolidations.fetch_add(1, Ordering::Relaxed);
    }

    pub fn record_deduplication(&self) {
        self.deduplications.fetch_add(1, Ordering::Relaxed);
    }

    pub fn record_l0_generation(&self, latency_ns: u64) {
        self.l0_generations.record_hit(latency_ns);
    }

    pub fn record_rambling_cycle(&self, latency_ns: u64) {
        self.rambling_cycles.record_hit(latency_ns);
    }

    pub fn record_conflict_resolution(&self, latency_ns: u64) {
        self.conflict_resolutions.record_hit(latency_ns);
    }

    pub fn reset(&self) {
        self.l1_reads.reset();
        self.l2_reads.reset();
        self.l3_reads.reset();
        self.writes.reset();
        self.l0_generations.reset();
        self.rambling_cycles.reset();
        self.conflict_resolutions.reset();
        self.consolidations.store(0, Ordering::Relaxed);
        self.deduplications.store(0, Ordering::Relaxed);
    }

    pub fn report(&self) -> MetricsReport {
        MetricsReport {
            l1_hit_rate: self.l1_reads.hit_rate(),
            l1_avg_latency_ns: self.l1_reads.avg_latency_ns(),
            l2_hit_rate: self.l2_reads.hit_rate(),
            l2_avg_latency_ns: self.l2_reads.avg_latency_ns(),
            l3_hit_rate: self.l3_reads.hit_rate(),
            l3_avg_latency_ns: self.l3_reads.avg_latency_ns(),
            write_avg_latency_ns: self.writes.avg_latency_ns(),
            consolidations: self.consolidations.load(Ordering::Relaxed),
            deduplications: self.deduplications.load(Ordering::Relaxed),
            l0_avg_latency_ns: self.l0_generations.avg_latency_ns(),
            l0_ops: self.l0_generations.total_ops(),
            rambling_avg_latency_ns: self.rambling_cycles.avg_latency_ns(),
            rambling_ops: self.rambling_cycles.total_ops(),
            conflict_avg_latency_ns: self.conflict_resolutions.avg_latency_ns(),
            conflict_ops: self.conflict_resolutions.total_ops(),
        }
    }
}

#[derive(Debug, Clone)]
pub struct MetricsReport {
    pub l1_hit_rate: f64,
    pub l1_avg_latency_ns: f64,
    pub l2_hit_rate: f64,
    pub l2_avg_latency_ns: f64,
    pub l3_hit_rate: f64,
    pub l3_avg_latency_ns: f64,
    pub write_avg_latency_ns: f64,
    pub consolidations: u64,
    pub deduplications: u64,
    pub l0_avg_latency_ns: f64,
    pub l0_ops: u64,
    pub rambling_avg_latency_ns: f64,
    pub rambling_ops: u64,
    pub conflict_avg_latency_ns: f64,
    pub conflict_ops: u64,
}

/// A metrics-enabled wrapper around a `MemoryStore` that tracks performance.
pub struct MonitoredMemoryStore {
    store: crate::memory_store::MemoryStore,
    metrics: Arc<MemoryMetrics>,
}

impl MonitoredMemoryStore {
    pub fn new(store: crate::memory_store::MemoryStore) -> Self {
        Self {
            store,
            metrics: Arc::new(MemoryMetrics::new()),
        }
    }

    pub fn store(&self, input: crate::core::types::MemoryInput) -> crate::core::MemoryResult<u64> {
        let start = std::time::Instant::now();
        let result = self.store.store(input);
        let elapsed = start.elapsed().as_nanos() as u64;
        self.metrics.record_write(elapsed);
        result
    }

    pub fn recall_by_id(
        &self,
        id: u64,
    ) -> crate::core::MemoryResult<Option<crate::core::types::Memory>> {
        let start = std::time::Instant::now();
        let result = self.store.recall_by_id(id);
        let elapsed = start.elapsed().as_nanos() as u64;
        // Track as L1 hit/miss — in the cascade read, L1 is checked first.
        // For per-layer accuracy the router would need to report the source.
        if result.as_ref().ok().and_then(|r| r.as_ref()).is_some() {
            self.metrics.record_l1_hit(elapsed);
        } else {
            self.metrics.record_l1_miss(elapsed);
        }
        result
    }

    pub fn recall_by_text(
        &self,
        text: &str,
        k: usize,
    ) -> crate::core::MemoryResult<
        Vec<(crate::core::types::Memory, f32, crate::core::types::LayerId)>,
    > {
        let start = std::time::Instant::now();
        let result = self.store.recall_by_text(text, k);
        let elapsed = start.elapsed().as_nanos() as u64;
        // Text search primarily hits L2 (semantic) — record as L2
        if result.as_ref().is_ok_and(|r| !r.is_empty()) {
            self.metrics.record_l2_hit(elapsed);
        } else {
            self.metrics.record_l2_miss(elapsed);
        }
        result
    }

    pub fn recall(
        &self,
        query: &crate::core::types::Query,
        k: usize,
    ) -> crate::core::MemoryResult<
        Vec<(crate::core::types::Memory, f32, crate::core::types::LayerId)>,
    > {
        let start = std::time::Instant::now();
        let result = self.store.recall(query, k);
        let elapsed = start.elapsed().as_nanos() as u64;
        if result.as_ref().is_ok_and(|r| !r.is_empty()) {
            self.metrics.record_l2_hit(elapsed);
        } else {
            self.metrics.record_l2_miss(elapsed);
        }
        result
    }

    pub fn forget(&self, id: u64) -> bool {
        self.store.forget(id)
    }

    pub fn consolidate(&self) -> (usize, usize) {
        let result = self.store.consolidate();
        self.metrics.record_consolidation();
        result
    }

    pub fn stats(&self) -> crate::memory_store::MemoryStoreStats {
        self.store.stats()
    }

    pub fn metrics(&self) -> Arc<MemoryMetrics> {
        self.metrics.clone()
    }

    pub fn report(&self) -> MetricsReport {
        self.metrics.report()
    }

    pub fn inner(&self) -> &crate::memory_store::MemoryStore {
        &self.store
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::consolidation::ConsolidationConfig;
    use crate::core::{Fact, MemoryContent, MemoryInput};
    use crate::l1::L1Cache;
    use crate::l2::{HnswConfig, L2Config, L2Engine};
    use crate::l3::{BudgetConfig, L3Config, L3Engine};
    use tempfile::tempdir;

    fn make_monitored_store() -> MonitoredMemoryStore {
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
            storage_path: dir.path().join("metrics.bin"),
            budget: BudgetConfig {
                daily_token_limit: 1_000_000,
                annual_storage_limit: 10_000_000,
            },
            ..Default::default()
        });
        let store =
            crate::memory_store::MemoryStore::new(l1, Arc::new(l2), l3, ConsolidationConfig::default());
        MonitoredMemoryStore::new(store)
    }

    #[test]
    fn test_cache_metrics_basics() {
        let m = CacheMetrics::new();
        assert_eq!(m.hit_count(), 0);
        assert_eq!(m.miss_count(), 0);
        assert!((m.hit_rate() - 0.0).abs() < 1e-9);
        assert!((m.avg_latency_ns() - 0.0).abs() < 1e-9);

        m.record_hit(100);
        m.record_hit(200);
        m.record_miss(50);
        assert_eq!(m.hit_count(), 2);
        assert_eq!(m.miss_count(), 1);
        assert!((m.hit_rate() - 2.0 / 3.0).abs() < 1e-6);
        assert!((m.avg_latency_ns() - 350.0 / 3.0).abs() < 1e-6);
    }

    #[test]
    fn test_memory_metrics_report() {
        let m = MemoryMetrics::new();
        m.record_l1_hit(100);
        m.record_l2_miss(200);
        m.record_write(50);
        let report = m.report();
        assert!((report.l1_hit_rate - 1.0).abs() < 1e-6);
        assert!((report.l2_hit_rate - 0.0).abs() < 1e-6);
        assert!((report.write_avg_latency_ns - 50.0).abs() < 1e-6);
    }

    #[test]
    fn test_monitored_store_tracks_metrics() {
        let ms = make_monitored_store();
        let input = MemoryInput::new(MemoryContent::Fact(Fact::new("perf", "test", "entry")));
        let id = ms.store(input).unwrap();
        let _ = ms.recall_by_id(id).unwrap();
        let _ = ms.recall_by_text("perf test", 5).unwrap();

        let report = ms.report();
        assert!(
            report.write_avg_latency_ns > 0.0,
            "write should have latency"
        );
        assert!(
            report.l1_avg_latency_ns > 0.0,
            "L1 read should have latency"
        );
        assert!(
            report.l2_avg_latency_ns > 0.0,
            "L2 read should have latency"
        );
    }

    #[test]
    fn test_metrics_reset() {
        let m = CacheMetrics::new();
        m.record_hit(100);
        m.record_miss(50);
        m.reset();
        assert_eq!(m.hit_count(), 0);
        assert_eq!(m.miss_count(), 0);
    }

    #[test]
    fn test_monitored_store_consolidation_metrics() {
        let ms = make_monitored_store();
        ms.consolidate();
        let report = ms.report();
        assert_eq!(report.consolidations, 1);
    }
}
