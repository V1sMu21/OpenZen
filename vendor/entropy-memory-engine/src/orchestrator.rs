use std::collections::HashSet;
use std::sync::{Arc, RwLock};

use crate::core::types::{MemoryContent, MemoryInput};
use crate::core::MemoryResult;
use crate::memory_store::MemoryStore;
use crate::phase1::ConflictResolution;
use crate::phase1::ConflictResolver;
use crate::phase2::RamblingEngine;
use crate::phase4::{
    AnchorResult, PriorityTaskScheduler, QuarantineManager, QuarantineStatus, RealityAnchor,
    TaskPriority,
};
use crate::phase5::observer::BehaviorEvent;
use crate::phase5::BehaviorObserver;
use crate::router::MemoryRouter;

pub struct MemoryOrchestrator {
    store: Arc<MemoryStore>,
    conflict_resolver: Arc<ConflictResolver>,
    quarantine: Arc<QuarantineManager>,
    /// Phase2-5 idle-cycle wiring (P3.3); attached via [`Self::with_idle_cycle`].
    /// `Arc` so the same engine is shared with the L0 ReflectionEngine.
    rambling: Option<Arc<RamblingEngine>>,
    reality_anchor: Option<RealityAnchor>,
    observer: BehaviorObserver,
    scheduler: Option<PriorityTaskScheduler>,
    /// Conjecture IDs already promoted into memory (prevents duplicate admits).
    promoted_conjectures: RwLock<HashSet<u64>>,
}

/// Result of one idle cycle: Phase2 rambling → Phase4 quarantine/reality
/// anchor → promote verified conjectures → Phase5 observer/scheduler.
#[derive(Debug, Clone, Default)]
pub struct IdleCycleReport {
    /// Conjectures produced by rambling this cycle.
    pub rambled: usize,
    /// Conjectures admitted into the quarantine zone.
    pub admitted_to_quarantine: usize,
    /// Conjectures rejected because they conflict with reality anchors.
    pub anchored_rejected: usize,
    /// Conjectures that passed the verification threshold.
    pub verified: usize,
    /// Verified conjectures promoted into long-term memory.
    pub promoted: usize,
    /// Total behavior events observed so far.
    pub observer_events: usize,
    /// Low-priority tasks queued on the scheduler.
    pub scheduled: usize,
}

impl MemoryOrchestrator {
    pub fn new(
        store: Arc<MemoryStore>,
        conflict_resolver: Arc<ConflictResolver>,
        quarantine: Arc<QuarantineManager>,
    ) -> Self {
        Self {
            store,
            conflict_resolver,
            quarantine,
            rambling: None,
            reality_anchor: None,
            observer: BehaviorObserver::new(),
            scheduler: None,
            promoted_conjectures: RwLock::new(HashSet::new()),
        }
    }

    /// Attach the Phase2-5 idle-cycle pipeline (P3.3 wiring).
    ///
    /// `rambling` is built by the caller with its own `Arc<L2Engine>` and
    /// `Arc<TimeGraph>` seeds; the reality anchor, observer and scheduler are
    /// constructed internally from the orchestrator's existing components.
    pub fn with_idle_cycle(mut self, rambling: Arc<RamblingEngine>) -> Self {
        self.rambling = Some(rambling);
        self.reality_anchor = Some(RealityAnchor::new());
        self.scheduler = Some(PriorityTaskScheduler::new());
        self
    }

    pub fn store(&self) -> &Arc<MemoryStore> {
        &self.store
    }

    pub fn conflict_resolver(&self) -> &Arc<ConflictResolver> {
        &self.conflict_resolver
    }

    pub fn quarantine(&self) -> &Arc<QuarantineManager> {
        &self.quarantine
    }

    pub fn observer(&self) -> &BehaviorObserver {
        &self.observer
    }

    pub fn reality_anchor(&self) -> Option<&RealityAnchor> {
        self.reality_anchor.as_ref()
    }

    pub fn scheduler(&self) -> Option<&PriorityTaskScheduler> {
        self.scheduler.as_ref()
    }

    /// Pure conflict resolution: detect Overturn/Sublimate/Supplement
    /// candidates without persisting anything.
    ///
    /// Returns `(old_id, resolution, ccs)` sorted by CCS descending.
    pub fn pre_resolve(
        &self,
        input: &MemoryInput,
        router: &MemoryRouter,
    ) -> Vec<(u64, ConflictResolution, f32)> {
        let text = memory_input_text(input);
        if text.is_empty() {
            return Vec::new();
        }
        self.conflict_resolver
            .resolve(input, router.l2(), router.l3().storage())
    }

    /// Apply resolutions to existing memories after the new memory is stored.
    ///
    /// `new_id` is the ID of the just-stored memory (so `superseded_by`
    /// points at a real successor, never 0).
    pub fn apply_resolutions(
        &self,
        text: &str,
        new_importance: f32,
        router: &MemoryRouter,
        resolutions: &[(u64, ConflictResolution, f32)],
        new_id: u64,
    ) {
        for (old_id, resolution, _score) in resolutions {
            match resolution {
                ConflictResolution::Overturn => {
                    self.store.mark_superseded(*old_id, new_id);
                    // Overturn 后物理清理 L3 旧记忆, 避免被推翻的知识继续占用长期存储
                    router.l3().remove(*old_id);
                }
                ConflictResolution::Sublimate => {
                    if let Some(old_mem) = router.l2().get_by_id(*old_id) {
                        let old_text = old_mem.content_text();
                        if !old_text.is_empty() {
                            let combined = format!("{} | updated: {}", old_text, text);
                            let merged_input = MemoryInput::new(MemoryContent::Summary(combined))
                                .with_importance(
                                    (old_mem.metadata.importance + new_importance) / 2.0,
                                );
                            router.l3().insert(merged_input).ok();
                            self.store.mark_superseded(*old_id, new_id);
                        }
                    }
                }
                ConflictResolution::Supplement => {}
            }
        }
    }

    pub fn process_store(&self, input: MemoryInput) -> MemoryResult<u64> {
        let router = self.store.router();
        let text = memory_input_text(&input);
        let importance = input.importance;
        let resolutions = self.pre_resolve(&input, router);
        let new_id = self.store.store(input)?;
        self.apply_resolutions(&text, importance, router, &resolutions, new_id);
        Ok(new_id)
    }

    /// 空闲周期 (P3.3): 内循环 + 安全阀 + 观察上报。
    ///
    /// Pipeline:
    /// 1. **rambling** — seed from high-abstraction L2 memories, produce
    ///    conjectures for the quarantine zone.
    /// 2. **reality anchor** — reject conjectures that conflict with anchored
    ///    facts (hallucination filter) before they enter quarantine.
    /// 3. **quarantine** — admit new conjectures; run a verification cycle so
    ///    candidates reaching `verification_threshold` become `Verified`.
    /// 4. **promote** — verified conjectures are written into long-term
    ///    memory exactly once (dedup via `promoted_conjectures`).
    /// 5. **observer / scheduler** — record the round's events and queue a
    ///    low-priority maintenance task (narrative rebuild / consolidation).
    pub fn run_idle_cycle(&self) -> IdleCycleReport {
        let mut report = IdleCycleReport::default();

        // 1. rambling: 从 L2 高抽象层 seed，产猜想过隔离区
        if let Some(rambler) = &self.rambling {
            let conjectures = rambler.ramble();
            report.rambled = conjectures.len();

            for conjecture in conjectures {
                // 2. reality_anchor: verify_against_anchors 过滤幻觉
                if let Some(anchor) = &self.reality_anchor {
                    if let AnchorResult::Anchored(_) =
                        anchor.verify_against_anchors(&conjecture, &self.store)
                    {
                        report.anchored_rejected += 1;
                        self.observer.record(BehaviorEvent::SelfCorrection {
                            old_id: conjecture.node_a,
                            new_id: conjecture.node_b,
                            trigger: "reality_anchor".into(),
                        });
                        continue;
                    }
                }
                // 3. quarantine: 新猜想先入隔离区，达到 verification_threshold 才 admit
                if self.quarantine.admit(conjecture) {
                    report.admitted_to_quarantine += 1;
                }
            }
        }

        // 3b. verification pass (advances Pending → Verified/Expired)
        self.quarantine.verify_cycle();
        self.quarantine.cleanup_expired();

        // 4. promote verified conjectures into long-term memory
        self.promote_verified(&mut report);

        // 5. observer: 记录本轮事件
        report.observer_events = self.observer.event_count();

        // 5b. scheduler: 低优先任务（叙事重建等）排队执行
        if let Some(scheduler) = &self.scheduler {
            let store = Arc::clone(&self.store);
            scheduler.submit(TaskPriority::Low, move || {
                let _ = store.consolidate();
            });
            report.scheduled = 1;
        }

        // 6. decay the time-graph weights so stale associations fade
        if let Some(rambler) = &self.rambling {
            rambler.decay_cycle();
        }

        report
    }

    fn promote_verified(&self, report: &mut IdleCycleReport) -> usize {
        let mut promoted = 0usize;
        for qc in self.quarantine.all_conjectures() {
            if qc.status != QuarantineStatus::Verified {
                continue;
            }
            let id = qc.conjecture.id;
            {
                let seen = self.promoted_conjectures.read().unwrap();
                if seen.contains(&id) {
                    continue;
                }
            }
            report.verified += 1;
            if let Ok(Some(input)) = self.quarantine.promote(id) {
                if let Ok(_new_id) = self.store.store(input) {
                    self.promoted_conjectures.write().unwrap().insert(id);
                    promoted += 1;
                    self.observer.record(BehaviorEvent::InsightGenerated {
                        depth: qc.conjecture.sss_score,
                        statement: qc.conjecture.statement.clone(),
                    });
                }
            }
        }
        report.promoted += promoted;
        promoted
    }
}

fn memory_input_text(input: &MemoryInput) -> String {
    match &input.content {
        MemoryContent::Fact(f) => {
            format!("{} {} {}", f.subject, f.predicate, f.object)
        }
        MemoryContent::Summary(s) => s.clone(),
        _ => String::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::consolidation::ConsolidationConfig;
    use crate::core::traits::MemoryLayer;
    use crate::core::types::{Fact, MemoryContent, MemoryInput};
    use crate::l1::L1Cache;
    use crate::l2::{HnswConfig, L2Config, L2Engine, TimeGraph};
    use crate::l3::{BudgetConfig, L3Config, L3Engine};
    use crate::phase2::RamblingConfig;
    use crate::phase4::QuarantineConfig;
    use tempfile::tempdir;

    fn make_orchestrator() -> MemoryOrchestrator {
        let l1 = L1Cache::builder().capacity(100).build();
        let dir = tempdir().unwrap();
        let l3 = L3Engine::new(L3Config {
            storage_path: dir.path().join("orch_test.bin"),
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
            ConsolidationConfig::default(),
        ));

        let l2_for_resolver = Arc::new(L2Engine::new(L2Config {
            hnsw: HnswConfig {
                dimension: 16,
                ..Default::default()
            },
            dimension: 16,
            ..Default::default()
        }));
        let resolver = Arc::new(ConflictResolver::new(Arc::clone(&l2_for_resolver)));

        let l2_for_quarantine = Arc::new(L2Engine::new(L2Config {
            hnsw: HnswConfig {
                dimension: 16,
                ..Default::default()
            },
            dimension: 16,
            ..Default::default()
        }));
        let quarantine = Arc::new(QuarantineManager::new(
            Arc::clone(&l2_for_quarantine),
            QuarantineConfig::default(),
        ));

        MemoryOrchestrator::new(store, resolver, quarantine)
    }

    #[test]
    fn test_process_store_simple() {
        let orch = make_orchestrator();
        let input = MemoryInput::new(MemoryContent::Fact(Fact::new("orch", "test", "works")));
        let id = orch.process_store(input).unwrap();
        assert!(id > 0);
    }

    #[test]
    fn test_store_accessor() {
        let orch = make_orchestrator();
        assert!(orch.store().router().l1().is_empty());
    }

    #[test]
    fn test_run_idle_cycle_without_wiring_is_noop() {
        // Without with_idle_cycle, the pipeline must degrade gracefully.
        let orch = make_orchestrator();
        let report = orch.run_idle_cycle();
        assert_eq!(report.rambled, 0);
        assert_eq!(report.admitted_to_quarantine, 0);
        assert_eq!(report.promoted, 0);
        assert_eq!(report.scheduled, 0);
        assert_eq!(report.observer_events, 0);
    }

    /// Build a dense clique of high-abstraction memories so rambling reliably
    /// produces interesting conjectures (SSS composite ≈ 1.0 for every pair).
    fn make_rambling_engine() -> Arc<RamblingEngine> {
        let l2 = Arc::new(L2Engine::new(L2Config {
            hnsw: HnswConfig {
                dimension: 16,
                ..Default::default()
            },
            dimension: 16,
            ..Default::default()
        }));
        // Fully-connected 8-node clique: every pair shares 7 common neighbors,
        // so compressibility = 0.5 and composite = 0.5/0.5 = 1.0 > 0.5.
        let ids: Vec<u64> = (1..=8)
            .map(|i| {
                let id = l2
                    .insert(MemoryInput::new(MemoryContent::Summary(format!(
                        "abstract concept {i}"
                    ))))
                    .unwrap();
                l2.update_metadata(id, |m| m.abstraction_level = 0.9);
                id
            })
            .collect();
        let tg = Arc::new(TimeGraph::new());
        for (i, &a) in ids.iter().enumerate() {
            for &b in &ids[i + 1..] {
                tg.add_edge(a, b, "related".into(), 1.0);
            }
        }
        Arc::new(RamblingEngine::new(RamblingConfig::default(), tg, l2))
    }

    #[test]
    fn test_run_idle_cycle_full_pipeline() {
        // verification_threshold = 0: any conjecture becomes Verified after a
        // single verify cycle, so the ramble→quarantine→verify→promote chain
        // completes deterministically without relying on semantic evidence.
        let store_arc = Arc::new(build_store("pipeline.bin"));
        let resolver = Arc::new(ConflictResolver::new(Arc::new(L2Engine::new(L2Config {
            hnsw: HnswConfig {
                dimension: 16,
                ..Default::default()
            },
            dimension: 16,
            ..Default::default()
        }))));
        let l2_for_quarantine = Arc::new(L2Engine::new(L2Config {
            hnsw: HnswConfig {
                dimension: 16,
                ..Default::default()
            },
            dimension: 16,
            ..Default::default()
        }));
        let quarantine = Arc::new(QuarantineManager::new(
            l2_for_quarantine,
            QuarantineConfig {
                verification_threshold: 0,
                ..Default::default()
            },
        ));
        let orch = MemoryOrchestrator::new(store_arc.clone(), resolver, quarantine)
            .with_idle_cycle(make_rambling_engine());

        let report = orch.run_idle_cycle();

        assert!(
            report.rambled >= 1,
            "clique must produce at least one conjecture, got {}",
            report.rambled
        );
        assert_eq!(report.rambled, report.admitted_to_quarantine);
        assert_eq!(
            report.anchored_rejected, 0,
            "no anchors set → nothing rejected"
        );
        assert!(
            report.verified >= 1,
            "threshold 0 must verify all admitted conjectures"
        );
        assert!(
            report.promoted >= 1,
            "verified conjectures must be promoted into memory"
        );
        assert_eq!(
            report.scheduled, 1,
            "scheduler must queue the low-priority task"
        );
        assert!(
            report.observer_events >= 1,
            "promotion must record an insight event"
        );

        // Promoted summaries must actually be in the store.
        let l3_count = store_arc.router().l3().len();
        assert!(
            l3_count >= report.promoted,
            "store must contain the promoted summaries"
        );
    }

    #[test]
    fn test_run_idle_cycle_promote_dedup() {
        let store_arc = Arc::new(build_store("dedup.bin"));
        let resolver = Arc::new(ConflictResolver::new(Arc::new(L2Engine::new(L2Config {
            hnsw: HnswConfig {
                dimension: 16,
                ..Default::default()
            },
            dimension: 16,
            ..Default::default()
        }))));
        let l2_for_quarantine = Arc::new(L2Engine::new(L2Config {
            hnsw: HnswConfig {
                dimension: 16,
                ..Default::default()
            },
            dimension: 16,
            ..Default::default()
        }));
        let quarantine = Arc::new(QuarantineManager::new(
            l2_for_quarantine,
            QuarantineConfig {
                verification_threshold: 0,
                ..Default::default()
            },
        ));
        let orch = MemoryOrchestrator::new(store_arc, resolver, quarantine)
            .with_idle_cycle(make_rambling_engine());

        let first = orch.run_idle_cycle();
        let first_promoted: HashSet<u64> = orch.promoted_conjectures.read().unwrap().clone();
        // Re-running the cycle must not re-promote already-promoted conjecture
        // ids: the promoted set only grows, and its growth equals the number
        // of promotions in the second cycle (any overlap would shrink the diff).
        let second = orch.run_idle_cycle();
        let second_promoted: HashSet<u64> = orch.promoted_conjectures.read().unwrap().clone();
        assert!(
            first_promoted.is_subset(&second_promoted),
            "promoted set must only grow across cycles"
        );
        assert_eq!(
            second_promoted.len() - first_promoted.len(),
            second.promoted,
            "second cycle must only promote new conjecture ids"
        );
    }

    #[test]
    fn test_reality_anchor_rejects_conflicting_conjecture() {
        let store_arc = Arc::new(build_store("anchor.bin"));
        // Seed the store's L2 with a memory and mark it as an anchor.
        let anchor_id = store_arc
            .router()
            .l2()
            .insert(MemoryInput::new(MemoryContent::Summary(
                "the sky is always green".into(),
            )))
            .unwrap();
        let resolver = Arc::new(ConflictResolver::new(Arc::new(L2Engine::new(L2Config {
            hnsw: HnswConfig {
                dimension: 16,
                ..Default::default()
            },
            dimension: 16,
            ..Default::default()
        }))));
        let l2_for_quarantine = Arc::new(L2Engine::new(L2Config {
            hnsw: HnswConfig {
                dimension: 16,
                ..Default::default()
            },
            dimension: 16,
            ..Default::default()
        }));
        let quarantine = Arc::new(QuarantineManager::new(
            l2_for_quarantine,
            QuarantineConfig::default(),
        ));
        let orch = MemoryOrchestrator::new(store_arc, resolver, quarantine)
            .with_idle_cycle(make_rambling_engine());
        let anchor = orch
            .reality_anchor()
            .expect("wired orchestrator has an anchor");
        anchor.set_anchor(anchor_id);

        // A conjecture whose statement is exactly the anchored fact must be
        // rejected as a hallucination (distance 0 < 0.3 threshold).
        let conjecture = crate::phase2::Conjecture {
            id: 424242,
            node_a: 1,
            node_b: 2,
            statement: "the sky is always green".into(),
            sss_score: 1.0,
            verification_status: crate::phase2::VerificationStatus::Pending,
        };
        let verdict = anchor.verify_against_anchors(&conjecture, orch.store());
        assert!(
            matches!(verdict, AnchorResult::Anchored(_)),
            "identical anchor text must be rejected, got {verdict:?}"
        );
    }

    fn build_store(name: &str) -> MemoryStore {
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
            storage_path: dir.path().join(name),
            budget: BudgetConfig {
                daily_token_limit: 1_000_000,
                annual_storage_limit: 10_000_000,
            },
            ..Default::default()
        });
        MemoryStore::new(l1, Arc::new(l2), l3, ConsolidationConfig::default())
    }
}
