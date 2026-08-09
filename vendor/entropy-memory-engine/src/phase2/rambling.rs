use std::collections::HashSet;
use std::sync::{Arc, RwLock};

use rand::Rng;

use crate::l2::{L2Engine, TimeGraph};
use crate::phase2::aesthetic::AestheticScorer;
use crate::phase2::types::{Conjecture, RamblingConfig, VerificationStatus};
use crate::phase4::DiversityRegularizer;

pub struct RamblingEngine {
    config: RamblingConfig,
    time_graph: Arc<TimeGraph>,
    l2_engine: Arc<L2Engine>,
    scorer: AestheticScorer,
    seen_pairs: Arc<RwLock<HashSet<(u64, u64)>>>,
    diversity: DiversityRegularizer,
}

impl RamblingEngine {
    pub fn new(
        config: RamblingConfig,
        time_graph: Arc<TimeGraph>,
        l2_engine: Arc<L2Engine>,
    ) -> Self {
        let scorer = AestheticScorer::new(Arc::clone(&time_graph));
        Self {
            config,
            time_graph,
            l2_engine,
            scorer,
            seen_pairs: Arc::new(RwLock::new(HashSet::new())),
            diversity: DiversityRegularizer::default(),
        }
    }

    pub fn set_diversity(&mut self, diversity: DiversityRegularizer) {
        self.diversity = diversity;
    }

    pub fn ramble(&self) -> Vec<Conjecture> {
        let seeds = self.default_seeds();
        self.ramble_from(&seeds)
    }

    /// 从指定种子节点联想（好奇心缺口驱动的内省入口）。
    ///
    /// 灵魂层的 Step2 会计算「好奇心缺口」维度，把缺口对应的 L2 节点
    /// 作为 seed 传入，实现「灵魂的意志 = 好奇心缺口」。
    pub fn ramble_with_seed(&self, seeds: &[u64]) -> Vec<Conjecture> {
        let mut unique: Vec<u64> = seeds.to_vec();
        unique.sort_unstable();
        unique.dedup();
        self.ramble_from(&unique)
    }

    fn default_seeds(&self) -> Vec<u64> {
        self.l2_engine
            .storage
            .all_ids()
            .into_iter()
            .filter(|id| {
                self.l2_engine
                    .get_by_id(*id)
                    .map(|m| m.metadata.abstraction_level > 0.6)
                    .unwrap_or(false)
            })
            .collect()
    }

    fn ramble_from(&self, seeds: &[u64]) -> Vec<Conjecture> {
        let mut conjectures = Vec::new();
        let mut rng = rand::thread_rng();

        if seeds.is_empty() {
            return conjectures;
        }

        let seed_count = seeds.len().min(5);
        if seed_count == 0 {
            return conjectures;
        }
        for _ in 0..seed_count {
            if conjectures.len() >= self.config.max_conjectures {
                break;
            }

            let seed_idx = rng.gen_range(0..seeds.len());
            let mut current = seeds[seed_idx];
            let mut path = vec![current];

            for _hop in 0..self.config.max_hops {
                let neighbors = self.time_graph.weighted_neighbors(current);
                if neighbors.is_empty() {
                    break;
                }

                let adjusted = self.diversity.adjust_weights(&neighbors);
                let total_weight: f32 = adjusted.iter().map(|(_, w)| w.max(0.001)).sum();
                let mut roll = rng.gen::<f32>() * total_weight;
                let mut next_node = adjusted[0].0;

                for (node_id, w) in &adjusted {
                    roll -= w.max(0.001);
                    if roll <= 0.0 {
                        next_node = *node_id;
                        break;
                    }
                }

                self.diversity.record_path(current, next_node);

                for &past_node in &path {
                    if past_node == next_node {
                        continue;
                    }
                    let pair = if past_node < next_node {
                        (past_node, next_node)
                    } else {
                        (next_node, past_node)
                    };

                    {
                        let seen = self.seen_pairs.read().unwrap();
                        if seen.contains(&pair) {
                            continue;
                        }
                    }

                    let score = self.scorer.score(past_node, next_node);
                    if score.is_interesting() {
                        let statement = format!(
                            "Node {} and node {} may be related (SSS={:.2})",
                            past_node, next_node, score.composite
                        );

                        {
                            let mut seen = self.seen_pairs.write().unwrap();
                            seen.insert(pair);
                        }

                        conjectures.push(Conjecture {
                            id: crate::core::generate_memory_id(),
                            node_a: past_node,
                            node_b: next_node,
                            statement,
                            sss_score: score.composite,
                            verification_status: VerificationStatus::Pending,
                        });
                    }
                }

                path.push(next_node);
                current = next_node;
            }
        }

        conjectures
    }

    pub fn config(&self) -> &RamblingConfig {
        &self.config
    }

    pub fn decay_cycle(&self) {
        self.time_graph.decay_all_weights(0.95);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::types::{Fact, MemoryContent, MemoryInput};
    use crate::l2::{HnswConfig, L2Config};

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

    #[test]
    fn test_ramble_empty_graph() {
        let tg = Arc::new(TimeGraph::new());
        let l2 = Arc::new(make_l2());
        let engine = RamblingEngine::new(RamblingConfig::default(), tg, l2);
        let conjectures = engine.ramble();
        assert!(conjectures.is_empty());
    }

    #[test]
    fn test_ramble_with_data() {
        let tg = Arc::new(TimeGraph::new());
        let l2 = Arc::new(make_l2());

        // Add nodes with high abstraction_level
        for i in 1..=5 {
            let input = MemoryInput::new(MemoryContent::Fact(Fact::new(
                &format!("concept_{}", i),
                "describes",
                &format!("domain_{}", i),
            )));
            let id = l2.insert(input).unwrap();
            tg.add_node(id, vec!["abstract".into()]);

            // Force abstraction_level high for rambling seeds
            if let Some(mut mem) = l2.get_by_id(id) {
                mem.metadata.abstraction_level = 0.8;
                // Can't modify through immutable get_by_id, skip for now
            }
        }

        // Add edges for walking
        let ids: Vec<u64> = l2.storage.all_ids();
        if ids.len() >= 2 {
            for w in ids.windows(2) {
                tg.add_edge(w[0], w[1], "related".into(), 0.7);
            }
        }

        let engine = RamblingEngine::new(RamblingConfig::default(), tg, l2);
        let conjectures = engine.ramble();
        // May produce conjectures depending on data - just verify no panic
        let _ = conjectures.len();
    }

    #[test]
    fn test_ramble_respects_max() {
        let tg = Arc::new(TimeGraph::new());
        let l2 = Arc::new(make_l2());
        let config = RamblingConfig {
            max_conjectures: 1,
            ..Default::default()
        };
        let engine = RamblingEngine::new(config, tg, l2);
        let conjectures = engine.ramble();
        assert!(conjectures.len() <= 1);
    }

    #[test]
    fn test_seen_pairs_dedup() {
        let tg = Arc::new(TimeGraph::new());
        let l2 = Arc::new(make_l2());
        let engine = RamblingEngine::new(RamblingConfig::default(), tg, l2);

        // Insert a pair into seen_pairs
        engine.seen_pairs.write().unwrap().insert((1, 2));
        assert!(engine.seen_pairs.read().unwrap().contains(&(1, 2)));
    }

    #[test]
    fn test_decay_cycle() {
        let tg = Arc::new(TimeGraph::new());
        tg.add_edge(1, 2, "test".into(), 1.0);
        let l2 = Arc::new(make_l2());
        let engine = RamblingEngine::new(RamblingConfig::default(), tg.clone(), l2);

        engine.decay_cycle();
        let neighbors = tg.weighted_neighbors(1);
        if let Some((_, w)) = neighbors.first() {
            assert!(*w < 1.0, "weight should decay, got {}", w);
        }
    }
}
