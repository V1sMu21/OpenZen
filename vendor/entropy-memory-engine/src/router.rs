use crate::core::error::MemoryError;
use crate::core::traits::MemoryLayer;
use crate::core::types::{generate_memory_id, LayerId, Memory, MemoryInput, Query};
use crate::core::MemoryResult;
use crate::l1::L1Cache;
use crate::l2::L2Engine;
use crate::l3::L3Engine;
use std::sync::Arc;

/// Tuning parameters for fusing L2 semantic distances with L3 keyword
/// match scores in `search()`.
#[derive(Debug, Clone)]
pub struct SearchConfig {
    /// Distance assigned to an L3 result that matches every query keyword.
    pub full_match_dist: f32,
    /// Distance added per missing-keyword fraction (0.0 = all keywords hit).
    pub miss_weight: f32,
}

impl Default for SearchConfig {
    fn default() -> Self {
        Self {
            full_match_dist: 0.05,
            miss_weight: 0.5,
        }
    }
}

impl SearchConfig {
    /// L3 keyword-match distance: full match ≈ `full_match_dist`,
    /// no match ≈ `full_match_dist + miss_weight`. Same scale as L2
    /// cosine distance so the two sources can be ranked together.
    pub fn keyword_dist(&self, matches: usize, total: usize) -> f32 {
        if total == 0 {
            return self.full_match_dist + self.miss_weight;
        }
        let match_ratio = matches as f32 / total as f32;
        self.full_match_dist + self.miss_weight * (1.0 - match_ratio)
    }
}

pub struct MemoryRouter {
    l1: L1Cache,
    /// L2 以 `Arc` 持有: Phase2 RamblingEngine 通过同一 `Arc<L2Engine>`
    /// 读取 store 中的记忆做联想, 否则其独立 L2 是空的, 内循环空转。
    l2: Arc<L2Engine>,
    l3: L3Engine,
    search_config: SearchConfig,
}

impl MemoryRouter {
    pub fn new(l1: L1Cache, l2: Arc<L2Engine>, l3: L3Engine) -> Self {
        Self {
            l1,
            l2,
            l3,
            search_config: SearchConfig::default(),
        }
    }

    pub fn with_search_config(mut self, config: SearchConfig) -> Self {
        self.search_config = config;
        self
    }

    pub fn l1(&self) -> &L1Cache {
        &self.l1
    }

    pub fn l2(&self) -> &L2Engine {
        self.l2.as_ref()
    }

    /// Shared handle to the L2 engine, for components that need the same
    /// `Arc` the router holds (e.g. Phase2 RamblingEngine).
    pub fn l2_arc(&self) -> &Arc<L2Engine> {
        &self.l2
    }

    pub fn l3(&self) -> &L3Engine {
        &self.l3
    }

    /// Cascade read: L1 (hash) → L2 (semantic) → L3 (persistent).
    /// When a memory is found in a deeper layer, it is promoted to all
    /// shallower layers so subsequent reads are faster.
    /// If the query has filters (importance, tags, time range), they are
    /// applied to the result before returning.
    pub fn read(&self, query: &Query) -> MemoryResult<Option<Memory>> {
        if let Some(mem) = self.l1.read(query)? {
            if query.matches(&mem) {
                return Ok(Some(mem));
            }
        }

        if let Some(mem) = self.l2.read(query)? {
            if query.matches(&mem) {
                self.promote_to_l1(mem.clone());
                return Ok(Some(mem));
            }
        }

        if let Some(mem) = self.l3.read(query)? {
            if query.matches(&mem) {
                self.promote_to_l2(mem.clone());
                self.promote_to_l1(mem.clone());
                return Ok(Some(mem));
            }
        }

        Ok(None)
    }

    /// Write to all layers with a single, consistent ID.
    ///
    /// **Critical**: The ID is generated ONCE and shared across all layers,
    /// so an ID-based recall always hits L1 on subsequent reads.
    pub fn write(&self, input: MemoryInput) -> MemoryResult<u64> {
        let id = generate_memory_id();

        // L3: compress + persist with shared ID
        self.l3.insert_with_id(input.clone(), id)?;

        // L2: embed + index with shared ID
        let l2_input = MemoryInput {
            content: input.content.clone(),
            importance: input.importance,
            alias: input.alias.clone(),
            tags: input.tags.clone(),
            layer: LayerId::L2,
        };
        self.l2.insert_with_id(l2_input, id).ok();

        // L1: cache with shared ID
        self.l1
            .insert_memory(Memory::from_input_with_id(input, id))?;

        Ok(id)
    }

    pub fn remove(&self, query: &Query) -> bool {
        if let Some(id) = query.hash_key() {
            self.l1.remove(id);
            self.l2.remove(id);
            return self.l3.remove(id);
        }
        false
    }

    /// Semantic search across L2 (fast) and L3 (comprehensive).
    /// Returns results marked with their source layer.
    pub fn search(&self, query: &Query, k: usize) -> Vec<(u64, f32, LayerId)> {
        let l2_candidates = k * 2;
        let mut results: Vec<(u64, f32, LayerId)> = self
            .l2
            .search_semantic(query, l2_candidates)
            .into_iter()
            .map(|(id, dist)| (id, dist, LayerId::L2))
            .collect();

        if let Some(text) = query.text() {
            let query_keywords: Vec<String> = text
                .split_whitespace()
                .filter(|w| w.len() >= 4)
                .map(|w| w.to_lowercase())
                .collect();
            if !query_keywords.is_empty() {
                let l3_results = self.l3.search(text);
                for mem in l3_results.iter().take(l2_candidates) {
                    if !results.iter().any(|(id, _, _)| *id == mem.id) {
                        let mem_text = match &mem.content {
                            crate::core::MemoryContent::Fact(f) => {
                                format!("{} {} {}", f.subject, f.predicate, f.object)
                            }
                            crate::core::MemoryContent::Summary(s) => s.clone(),
                            _ => String::new(),
                        };
                        let mem_text_lower = mem_text.to_lowercase();
                        let matches = query_keywords
                            .iter()
                            .filter(|kw| mem_text_lower.contains(kw.as_str()))
                            .count();
                        let dist = self
                            .search_config
                            .keyword_dist(matches, query_keywords.len());
                        results.push((mem.id, dist, LayerId::L3));
                    }
                }
            }
        }

        results.sort_by(|a, b| a.1.total_cmp(&b.1));
        results.truncate(k);
        results
    }

    pub fn len(&self) -> usize {
        self.l1.len() + self.l2.len() + self.l3.len()
    }

    pub fn l1_cache(&self) -> &L1Cache {
        &self.l1
    }

    pub fn l2_engine(&self) -> &L2Engine {
        &self.l2
    }

    pub fn l3_engine(&self) -> &L3Engine {
        &self.l3
    }

    fn promote_to_l1(&self, memory: Memory) {
        // Preserve the shared ID so an ID-based recall hits L1 on the next read.
        self.l1.insert_memory(memory).ok();
    }

    fn promote_to_l2(&self, memory: Memory) {
        let input = MemoryInput {
            content: memory.content.clone(),
            importance: memory.metadata.importance,
            alias: memory.alias.clone(),
            tags: memory.tags.clone(),
            layer: LayerId::L2,
        };
        // Preserve the shared ID (write() shares one ID across all layers).
        self.l2.insert_with_id(input, memory.id).ok();
    }
}

impl MemoryRouter {
    /// Cascade read with parallel L2/L3 lookup.
    ///
    /// Same semantics as [`MemoryRouter::read`] — L1 hash first, then the
    /// shallower matching layer wins and is promoted up the cascade — but the
    /// L2 and L3 lookups run concurrently via `rayon::join` instead of
    /// serially, halving the latency of an L1-miss recall.
    pub fn read_parallel(&self, query: &Query) -> MemoryResult<Option<Memory>> {
        if let Some(mem) = self.l1.read(query)? {
            if query.matches(&mem) {
                return Ok(Some(mem));
            }
        }

        let first_error: std::sync::Arc<std::sync::Mutex<Option<MemoryError>>> =
            std::sync::Arc::new(std::sync::Mutex::new(None));
        let (l2_hit, l3_hit) = rayon::join(
            || match self.l2.read(query) {
                Ok(m) => m,
                Err(e) => {
                    *first_error.lock().unwrap() = Some(e);
                    None
                }
            },
            || match self.l3.read(query) {
                Ok(m) => m,
                Err(e) => {
                    *first_error.lock().unwrap() = Some(e);
                    None
                }
            },
        );

        if let Some(e) = first_error.lock().unwrap().take() {
            return Err(e);
        }

        // Prefer the shallower layer (L2) when both hold the memory; the
        // `query.matches` filter rejects superseded entries.
        if let Some(mem) = l2_hit {
            if query.matches(&mem) {
                self.promote_to_l1(mem.clone());
                return Ok(Some(mem));
            }
        }
        if let Some(mem) = l3_hit {
            if query.matches(&mem) {
                self.promote_to_l2(mem.clone());
                self.promote_to_l1(mem.clone());
                return Ok(Some(mem));
            }
        }
        Ok(None)
    }
}

impl MemoryLayer for MemoryRouter {
    fn read(&self, query: &Query) -> Result<Option<Memory>, MemoryError> {
        self.read(query)
    }

    fn write(&self, input: MemoryInput) -> Result<(), MemoryError> {
        self.write(input)?;
        Ok(())
    }

    fn could_contain(&self, query: &Query) -> bool {
        if query.hash_key().is_some() {
            return self.l1.len() > 0 || self.l2.len() > 0 || self.l3.len() > 0;
        }
        if query.text().is_some() {
            return self.l2.len() > 0 || self.l3.len() > 0;
        }
        if query.embedding().is_some() {
            return self.l2.len() > 0;
        }
        false
    }

    fn len(&self) -> usize {
        self.l1.len() + self.l2.len() + self.l3.len()
    }

    fn is_empty(&self) -> bool {
        self.l1.is_empty() && self.l2.is_empty() && self.l3.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::{Fact, MemoryContent};
    use crate::l1::L1Cache;
    use crate::l2::{HnswConfig, L2Config};
    use crate::l3::{BudgetConfig, L3Config};
    use tempfile::tempdir;

    fn make_router() -> MemoryRouter {
        let l1 = L1Cache::builder().capacity(100).build();
        let l2 = L2Engine::new(L2Config {
            hnsw: HnswConfig {
                m: 8,
                m_max: 16,
                ef_construction: 50,
                ef_search: 50,
                dimension: 16,
                ..Default::default()
            },
            dimension: 16,
            ..Default::default()
        });
        let dir = tempdir().unwrap();
        let l3 = L3Engine::new(L3Config {
            storage_path: dir.path().join("router_test.bin"),
            budget: BudgetConfig {
                daily_token_limit: 1_000_000,
                annual_storage_limit: 10_000_000,
            },
            ..Default::default()
        });
        MemoryRouter::new(l1, Arc::new(l2), l3)
    }

    #[test]
    fn test_new_router_empty() {
        let r = make_router();
        assert!(r.is_empty());
    }

    #[test]
    fn test_write_and_read_by_id() {
        let r = make_router();
        let input = MemoryInput::new(MemoryContent::Fact(Fact::new("alice", "likes", "rust")));
        let id = r.write(input).unwrap();
        let query = Query::by_id(id);
        let result = r.read(&query).unwrap();
        assert!(result.is_some());
        assert_eq!(result.unwrap().id, id);
    }

    #[test]
    fn test_write_and_read_by_text() {
        let r = make_router();
        let input = MemoryInput::new(MemoryContent::Fact(Fact::new("bob", "knows", "rust")));
        r.write(input).unwrap();
        let query = Query::by_text("bob knows rust");
        let result = r.read(&query).unwrap();
        assert!(result.is_some());
        assert!(result.unwrap().content_text().contains("rust"));
    }

    #[test]
    fn test_read_not_found() {
        let r = make_router();
        let query = Query::by_id(99999);
        assert!(r.read(&query).unwrap().is_none());
    }

    #[test]
    fn test_remove() {
        let r = make_router();
        let input = MemoryInput::new(MemoryContent::Fact(Fact::new("x", "y", "z")));
        let id = r.write(input).unwrap();
        assert_eq!(r.len(), 3); // one in each layer
        let query = Query::by_id(id);
        assert!(r.remove(&query));
        assert!(r.read(&query).unwrap().is_none());
    }

    #[test]
    fn test_search_returns_l2_results() {
        let r = make_router();
        r.write(MemoryInput::new(MemoryContent::Fact(Fact::new(
            "user",
            "likes",
            "rust programming",
        ))))
        .unwrap();
        r.write(MemoryInput::new(MemoryContent::Fact(Fact::new(
            "user", "likes", "python",
        ))))
        .unwrap();
        let query = Query::by_text("rust code");
        let results = r.search(&query, 5);
        assert!(!results.is_empty());
    }

    #[test]
    fn test_keyword_dist_formula() {
        let cfg = SearchConfig::default();
        assert!(
            (cfg.keyword_dist(2, 2) - 0.05).abs() < 1e-6,
            "full match ≈ 0.05"
        );
        assert!(
            (cfg.keyword_dist(0, 2) - 0.55).abs() < 1e-6,
            "no match ≈ 0.55"
        );
        assert!(
            (cfg.keyword_dist(1, 2) - 0.3).abs() < 1e-6,
            "half match ≈ 0.3"
        );
        assert!(
            (cfg.keyword_dist(1, 0) - 0.55).abs() < 1e-6,
            "empty query guard"
        );
    }

    #[test]
    fn test_search_l3_keyword_hit_no_longer_dominates_l2() {
        let r = make_router();
        // L2: exact semantic match with the query (distance ≈ 0).
        r.write(MemoryInput::new(MemoryContent::Fact(Fact::new(
            "rust",
            "programming",
            "language",
        ))))
        .unwrap();
        // L3: full keyword match — the strongest possible L3 score (0.05).
        let l3_input = MemoryInput::new(MemoryContent::Summary(
            "rust programming language notes".into(),
        ));
        r.l3().insert(l3_input).unwrap();

        let query = Query::by_text("rust programming language");
        let results = r.search(&query, 5);

        let l2_rank = results.iter().position(|(_, _, l)| *l == LayerId::L2);
        let l3_rank = results.iter().position(|(_, _, l)| *l == LayerId::L3);
        assert!(l2_rank.is_some(), "L2 semantic match must be returned");
        assert!(
            l3_rank.is_some(),
            "L3 keyword match must be returned (mixed)"
        );
        assert!(
            l2_rank.unwrap() < l3_rank.unwrap(),
            "exact L2 semantic match (0.0) must outrank full L3 keyword match (0.05)"
        );
    }

    #[test]
    fn test_promotion_l3_to_l2_on_read() {
        let r = make_router();
        // Write directly to L3 (simulating cold start)
        let input = MemoryInput::new(MemoryContent::Fact(Fact::new("cold", "data", "test")));
        let _id = r.l3.insert(input).unwrap();
        // Read via router should promote to L1 and L2.
        // Use a text query since promoted memories get new IDs.
        let query = Query::by_text("cold data");
        let result = r.read(&query).unwrap();
        assert!(result.is_some(), "L3 data should be readable");
        // Now should be findable in L1 via text
        let l1_result = r.l1.read(&query).unwrap();
        assert!(l1_result.is_some(), "L3 data should be promoted to L1");
    }

    #[test]
    fn test_memory_layer_trait() {
        let r = make_router();
        let input = MemoryInput::new(MemoryContent::Fact(Fact::new("trait", "test", "impl")));
        r.write(input.clone()).unwrap();
        assert!(r.could_contain(&Query::by_text("trait test impl")));
        assert!(!r.is_empty());
    }

    #[test]
    fn test_write_persists_across_restart() {
        let dir = tempdir().unwrap();
        let storage_path = dir.path().join("persist.bin");

        let id = {
            let l1 = L1Cache::builder().capacity(100).build();
            let l2 = L2Engine::new(L2Config {
                hnsw: HnswConfig {
                    dimension: 16,
                    ..Default::default()
                },
                dimension: 16,
                ..Default::default()
            });
            let l3 = L3Engine::new(L3Config {
                storage_path: storage_path.clone(),
                budget: BudgetConfig {
                    daily_token_limit: 1_000_000,
                    annual_storage_limit: 10_000_000,
                },
                ..Default::default()
            });
            let r = MemoryRouter::new(l1, Arc::new(l2), l3);
            let input = MemoryInput::new(MemoryContent::Fact(Fact::new("persist", "test", "data")));
            r.write(input).unwrap()
        };

        // New router instance on same storage file
        {
            let l1 = L1Cache::builder().capacity(100).build();
            let l2 = L2Engine::new(L2Config {
                hnsw: HnswConfig {
                    dimension: 16,
                    ..Default::default()
                },
                dimension: 16,
                ..Default::default()
            });
            let l3 = L3Engine::new(L3Config {
                storage_path,
                budget: BudgetConfig {
                    daily_token_limit: 1_000_000,
                    annual_storage_limit: 10_000_000,
                },
                ..Default::default()
            });
            let r = MemoryRouter::new(l1, Arc::new(l2), l3);
            let query = Query::by_id(id);
            let result = r.l3.read(&query).unwrap();
            assert!(result.is_some(), "L3 data should survive restart");
            assert_eq!(result.unwrap().id, id);
        }
    }
}
