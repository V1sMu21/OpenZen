use dashmap::DashMap;
use parking_lot::RwLock;
use rand::Rng;

use crate::core::{Memory, MemoryContent, MemoryResult, Query};

const DEFAULT_SDR_DIMENSION: usize = 10_240;
const DEFAULT_SDR_CAPACITY: usize = 1_000;
const DEFAULT_SPARSITY: f64 = 0.1;

#[derive(Debug, Clone)]
pub struct SDRConfig {
    pub dimension: usize,
    pub capacity: usize,
    pub sparsity: f64,
    pub enabled: bool,
}

impl Default for SDRConfig {
    fn default() -> Self {
        Self {
            dimension: DEFAULT_SDR_DIMENSION,
            capacity: DEFAULT_SDR_CAPACITY,
            sparsity: DEFAULT_SPARSITY,
            enabled: false,
        }
    }
}

struct SDRVector {
    bits: Vec<u64>,
    memory_id: u64,
}

pub struct SDRCache {
    config: SDRConfig,
    vectors: RwLock<Vec<SDRVector>>,
    id_map: DashMap<u64, usize>,
    seed: u64,
}

impl SDRCache {
    pub fn new(config: SDRConfig) -> Self {
        let seed = rand::thread_rng().gen::<u64>();
        let capacity = config.capacity;
        Self {
            config,
            vectors: RwLock::new(Vec::with_capacity(capacity)),
            id_map: DashMap::new(),
            seed,
        }
    }

    pub fn insert(&self, memory: &Memory) -> MemoryResult<()> {
        if !self.config.enabled {
            return Ok(());
        }

        let text = self.extract_text(&memory.content);
        let hv = self.text_to_hypervector(&text);

        let mut vectors = self.vectors.write();
        if vectors.len() >= self.config.capacity {
            vectors.remove(0);
        }

        let idx = vectors.len();
        vectors.push(SDRVector {
            bits: hv,
            memory_id: memory.id,
        });
        self.id_map.insert(memory.id, idx);

        Ok(())
    }

    pub fn search(&self, query: &Query) -> MemoryResult<Vec<(u64, f64)>> {
        if !self.config.enabled {
            return Ok(Vec::new());
        }

        let text = query
            .text()
            .or(query.alias.as_deref())
            .unwrap_or("")
            .to_string();

        if text.is_empty() && query.hash_key().is_none() {
            return Ok(Vec::new());
        }

        let query_hv = self.text_to_hypervector(&text);
        let vectors = self.vectors.read();

        if vectors.is_empty() {
            return Ok(Vec::new());
        }

        let mut results: Vec<(u64, f64)> = vectors
            .iter()
            .map(|v| {
                let similarity = self.hamming_similarity(&query_hv, &v.bits);
                (v.memory_id, similarity)
            })
            .collect();

        results.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
        results.truncate(query.top_k.max(1));

        Ok(results)
    }

    fn text_to_hypervector(&self, text: &str) -> Vec<u64> {
        let n_words = self.config.dimension.div_ceil(64);
        let mut hv = vec![0u64; n_words];

        for chunk in text.as_bytes().chunks(8) {
            let mut val = self.seed;
            for &b in chunk {
                val = val.wrapping_mul(6364136223846793005).wrapping_add(b as u64);
            }
            let word_idx = (val as usize) % n_words;
            let bit_idx = (val.wrapping_shr(32) as usize) % 64;
            hv[word_idx] |= 1u64 << bit_idx;
        }

        hv
    }

    fn hamming_similarity(&self, a: &[u64], b: &[u64]) -> f64 {
        let total_bits = a.len().saturating_mul(64) as f64;
        if total_bits == 0.0 {
            return 0.0;
        }
        let common: u64 = a
            .iter()
            .zip(b.iter())
            .map(|(x, y)| (x & y).count_ones() as u64)
            .sum();
        let either: u64 = a
            .iter()
            .zip(b.iter())
            .map(|(x, y)| (x | y).count_ones() as u64)
            .sum();
        if either == 0 {
            return 0.0;
        }
        common as f64 / either as f64
    }

    fn extract_text(&self, content: &MemoryContent) -> String {
        match content {
            MemoryContent::Fact(f) => {
                format!("{} {} {}", f.subject, f.predicate, f.object)
            }
            MemoryContent::Summary(s) => s.clone(),
            MemoryContent::Fingerprint(_) => String::new(),
            MemoryContent::Embedding(_) => String::new(),
        }
    }

    pub fn remove(&self, memory_id: u64) {
        self.id_map.remove(&memory_id);
        let mut vectors = self.vectors.write();
        vectors.retain(|v| v.memory_id != memory_id);
    }

    pub fn len(&self) -> usize {
        self.vectors.read().len()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    pub fn is_enabled(&self) -> bool {
        self.config.enabled
    }

    pub fn clear(&self) {
        self.vectors.write().clear();
        self.id_map.clear();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::{Fact, Memory, MemoryContent};

    fn make_sdr_cache() -> SDRCache {
        SDRCache::new(SDRConfig {
            enabled: true,
            dimension: 1024,
            capacity: 100,
            sparsity: 0.1,
        })
    }

    #[test]
    fn test_sdr_disabled_by_default() {
        let sdr = SDRCache::new(SDRConfig::default());
        assert!(!sdr.is_enabled());
        assert!(sdr.is_empty());
    }

    #[test]
    fn test_sdr_insert_and_search() {
        let sdr = make_sdr_cache();
        let mem = Memory::new(MemoryContent::Fact(Fact::new("user", "likes", "rust")));

        sdr.insert(&mem).unwrap();
        assert_eq!(sdr.len(), 1);

        // The SimHash over small text is sparse; query with overlapping tokens to find a match
        let query = Query::by_text("user likes rust");
        let results = sdr.search(&query).unwrap();
        assert!(!results.is_empty());
        assert_eq!(results[0].0, mem.id);
        assert!(results[0].1 > 0.0);
    }

    #[test]
    fn test_sdr_similarity_same_text() {
        let sdr = make_sdr_cache();
        let mem = Memory::new(MemoryContent::Fact(Fact::new("user", "likes", "rust")));
        sdr.insert(&mem).unwrap();

        let query = Query::by_text("likes rust");
        let results = sdr.search(&query).unwrap();
        assert!(!results.is_empty());
        let exact = Query::by_text("user likes rust");
        let exact_results = sdr.search(&exact).unwrap();
        assert!(
            exact_results[0].1 >= results[0].1,
            "exact match should have higher or equal similarity"
        );
    }

    #[test]
    fn test_sdr_no_match() {
        let sdr = make_sdr_cache();
        let mem = Memory::new(MemoryContent::Fact(Fact::new("user", "likes", "rust")));
        sdr.insert(&mem).unwrap();

        let query = Query::by_text("python");
        let results = sdr.search(&query).unwrap();
        assert!(!results.is_empty());
        assert!(results[0].1 < 0.5);
    }

    #[test]
    fn test_sdr_remove() {
        let sdr = make_sdr_cache();
        let mem = Memory::new(MemoryContent::Fact(Fact::new("test", "is", "test")));
        sdr.insert(&mem).unwrap();
        assert_eq!(sdr.len(), 1);
        sdr.remove(mem.id);
        assert_eq!(sdr.len(), 0);
    }

    #[test]
    fn test_sdr_clear() {
        let sdr = make_sdr_cache();
        for i in 0..5 {
            let mem = Memory::new(MemoryContent::Fact(Fact::new("k", "v", i.to_string())));
            sdr.insert(&mem).unwrap();
        }
        assert_eq!(sdr.len(), 5);
        sdr.clear();
        assert_eq!(sdr.len(), 0);
    }

    #[test]
    fn test_sdr_capacity_limit() {
        let config = SDRConfig {
            enabled: true,
            dimension: 1024,
            capacity: 3,
            sparsity: 0.1,
        };
        let sdr = SDRCache::new(config);
        for i in 0..5 {
            let mem = Memory::new(MemoryContent::Fact(Fact::new("k", "v", i.to_string())));
            sdr.insert(&mem).unwrap();
        }
        assert_eq!(sdr.len(), 3);
    }

    #[test]
    fn test_sdr_search_empty() {
        let sdr = make_sdr_cache();
        let query = Query::by_text("anything");
        let results = sdr.search(&query).unwrap();
        assert!(results.is_empty());
    }

    #[test]
    fn test_hamming_similarity_identical() {
        let sdr = make_sdr_cache();
        let a = vec![0b1010u64, 0b0101u64];
        let b = vec![0b1010u64, 0b0101u64];
        let sim = sdr.hamming_similarity(&a, &b);
        assert!((sim - 1.0).abs() < 1e-6);
    }

    #[test]
    fn test_hamming_similarity_opposite() {
        let sdr = make_sdr_cache();
        let a = vec![0b1111u64];
        let b = vec![0b0000u64];
        let sim = sdr.hamming_similarity(&a, &b);
        assert!((sim - 0.0).abs() < 1e-6);
    }
}
