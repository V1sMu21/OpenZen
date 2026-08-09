use std::sync::Arc;

use crate::core::types::{KnowledgeSource, MemoryContent, MemoryInput, MemoryMeta, Query};
use crate::l2::L2Engine;
use crate::phase1::annotator::MetadataAnnotator;
use crate::phase1::types::{ConflictResolution, ConflictScore};

pub struct ConflictResolver {
    annotator: MetadataAnnotator,
}

impl ConflictResolver {
    pub fn new(_l2_engine: Arc<L2Engine>) -> Self {
        Self {
            annotator: MetadataAnnotator::new(),
        }
    }

    pub fn resolve(
        &self,
        new_input: &MemoryInput,
        l2_engine: &L2Engine,
        l3_storage: &crate::l3::L3Storage,
    ) -> Vec<(u64, ConflictResolution, f32)> {
        let new_text = memory_input_text(new_input);
        if new_text.is_empty() {
            return Vec::new();
        }

        let query = Query::by_text(&new_text);
        let candidates = l2_engine.search_semantic(&query, 20);

        let new_meta = self.build_meta(new_input);

        let mut results = Vec::new();
        for (candidate_id, distance) in &candidates {
            if let Some(old_mem) = l2_engine.get_by_id(*candidate_id) {
                if old_mem.metadata.superseded_by.is_some() {
                    continue;
                }
                let compatibility = 1.0 - distance.clamp(0.0, 1.0);
                let score = self.compute_ccs(&new_meta, &old_mem.metadata, compatibility);
                let resolution = score.resolution();
                results.push((*candidate_id, resolution, score.ccs));
            }
        }

        for mem in l3_storage.search_by_text(&new_text).into_iter().take(10) {
            if mem.metadata.superseded_by.is_some() {
                continue;
            }
            if results.iter().any(|(id, _, _)| *id == mem.id) {
                continue;
            }
            let compatibility = 0.5;
            let score = self.compute_ccs(&new_meta, &mem.metadata, compatibility);
            let resolution = score.resolution();
            results.push((mem.id, resolution, score.ccs));
        }

        results.sort_by(|a, b| b.2.partial_cmp(&a.2).unwrap_or(std::cmp::Ordering::Equal));
        results
    }

    fn build_meta(&self, input: &MemoryInput) -> MemoryMeta {
        let (factuality, abstraction_level) = self.annotator.annotate(input);
        MemoryMeta {
            factuality,
            abstraction_level,
            source: KnowledgeSource::ExternalInput,
            ..Default::default()
        }
    }

    pub fn compute_ccs(
        &self,
        new_meta: &MemoryMeta,
        old_meta: &MemoryMeta,
        compatibility: f32,
    ) -> ConflictScore {
        let f_old = old_meta.factuality;
        let f_new = new_meta.factuality;
        let c_new = new_meta.importance.clamp(0.0, 1.0);
        let l_new = new_meta.abstraction_level.clamp(0.0, 1.0);
        let i = compatibility.clamp(0.0, 1.0);

        let ccs = (f_old - (f_old - f_new).abs()) * c_new * l_new * i;

        ConflictScore {
            ccs: ccs.clamp(0.0, 1.0),
            factuality_gap: (f_old - f_new).abs(),
            compatibility,
        }
    }

    pub fn classify(
        &self,
        new_meta: &MemoryMeta,
        old_meta: &MemoryMeta,
        compatibility: f32,
    ) -> ConflictResolution {
        self.compute_ccs(new_meta, old_meta, compatibility)
            .resolution()
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
    use crate::core::types::Fact;
    use crate::l2::{HnswConfig, L2Config};
    use crate::l3::{BudgetConfig, L3Config};
    use tempfile::tempdir;

    fn make_l2() -> L2Engine {
        L2Engine::new(L2Config {
            hnsw: HnswConfig {
                dimension: 16,
                ..Default::default()
            },
            dimension: 16,
            ..Default::default()
        })
    }

    fn make_l3() -> crate::l3::L3Engine {
        let dir = tempdir().unwrap();
        crate::l3::L3Engine::new(L3Config {
            storage_path: dir.path().join("conflict_test.bin"),
            budget: BudgetConfig {
                daily_token_limit: 1_000_000,
                annual_storage_limit: 10_000_000,
            },
            ..Default::default()
        })
    }

    fn insert_fact(l2: &L2Engine, subject: &str, predicate: &str, object: &str) -> u64 {
        l2.insert(MemoryInput::new(MemoryContent::Fact(Fact::new(
            subject, predicate, object,
        ))))
        .unwrap()
    }

    #[test]
    fn test_ccs_supplement_when_high_compatibility() {
        let new_meta = MemoryMeta {
            factuality: 0.7,
            importance: 0.8,
            abstraction_level: 0.5,
            ..Default::default()
        };
        let old_meta = MemoryMeta {
            factuality: 0.8,
            ..Default::default()
        };
        let l2 = Arc::new(make_l2());
        let resolver = ConflictResolver::new(l2);
        let score = resolver.compute_ccs(&new_meta, &old_meta, 0.9);
        assert!(
            score.ccs > 0.15,
            "high compatibility + similar factuality should Supplement, got {}",
            score.ccs
        );
        assert_eq!(score.resolution(), ConflictResolution::Supplement);
    }

    #[test]
    fn test_ccs_overturn_when_low_compatibility() {
        let new_meta = MemoryMeta {
            factuality: 0.9,
            importance: 0.9,
            abstraction_level: 0.3,
            ..Default::default()
        };
        let old_meta = MemoryMeta {
            factuality: 0.4,
            ..Default::default()
        };
        let l2 = Arc::new(make_l2());
        let resolver = ConflictResolver::new(l2);
        let score = resolver.compute_ccs(&new_meta, &old_meta, 0.1);
        assert!(
            score.ccs < 0.05,
            "low compatibility + large factuality gap should Overturn, got {}",
            score.ccs
        );
        assert_eq!(score.resolution(), ConflictResolution::Overturn);
    }

    #[test]
    fn test_ccs_sublimate_mid_range() {
        let new_meta = MemoryMeta {
            factuality: 0.6,
            importance: 0.7,
            abstraction_level: 0.5,
            ..Default::default()
        };
        let old_meta = MemoryMeta {
            factuality: 0.7,
            ..Default::default()
        };
        let l2 = Arc::new(make_l2());
        let resolver = ConflictResolver::new(l2);
        let score = resolver.compute_ccs(&new_meta, &old_meta, 0.7);
        assert!(
            score.ccs > 0.05 && score.ccs <= 0.15,
            "mid-range values should be Sublimate, got {}",
            score.ccs
        );
        assert_eq!(score.resolution(), ConflictResolution::Sublimate);
    }

    #[test]
    fn test_resolve_finds_similar_in_l2() {
        let l2 = Arc::new(make_l2());
        insert_fact(&l2, "earth", "orbits", "sun");
        insert_fact(&l2, "mars", "orbits", "sun");

        let resolver = ConflictResolver::new(Arc::clone(&l2));
        let l3 = make_l3();
        let input = MemoryInput::new(MemoryContent::Fact(Fact::new("venus", "orbits", "sun")));
        let results = resolver.resolve(&input, &l2, l3.storage());
        assert!(
            !results.is_empty(),
            "semantically similar memories must be found as resolution candidates"
        );
    }

    #[test]
    fn test_resolve_empty_for_no_input() {
        let l2 = Arc::new(make_l2());
        let l3 = make_l3();
        let resolver = ConflictResolver::new(l2);
        let input = MemoryInput::new(MemoryContent::Summary(String::new()));
        let results = resolver.resolve(&input, &make_l2(), l3.storage());
        assert!(results.is_empty());
    }

    #[test]
    fn test_classify_consistency_with_compute_ccs() {
        let new_meta = MemoryMeta {
            factuality: 0.8,
            importance: 0.9,
            abstraction_level: 0.4,
            ..Default::default()
        };
        let old_meta = MemoryMeta {
            factuality: 0.5,
            ..Default::default()
        };
        let l2 = Arc::new(make_l2());
        let resolver = ConflictResolver::new(l2);

        let score = resolver.compute_ccs(&new_meta, &old_meta, 0.8);
        let classification = resolver.classify(&new_meta, &old_meta, 0.8);

        assert_eq!(
            score.resolution(),
            classification,
            "classify should match compute_ccs resolution"
        );
    }
}
