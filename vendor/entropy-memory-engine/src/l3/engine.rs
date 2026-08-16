use std::path::PathBuf;

use crate::core::error::MemoryError;
use crate::core::traits::MemoryLayer;
use crate::core::types::{Memory, MemoryContent, MemoryInput, Query};
use crate::core::MemoryResult;

use super::budget::{BudgetConfig, BudgetController, BudgetStats};
use super::compress::{build_compressor, Compressor, DistillationConfig};
use super::storage::L3Storage;

pub struct L3Config {
    pub storage_path: PathBuf,
    pub budget: BudgetConfig,
    pub compression_max_chars: usize,
    pub distillation: DistillationConfig,
}

impl Default for L3Config {
    fn default() -> Self {
        Self {
            storage_path: PathBuf::from("l3_memories.bin"),
            budget: BudgetConfig::default(),
            compression_max_chars: 1024,
            distillation: DistillationConfig::default(),
        }
    }
}

pub struct L3Engine {
    storage: L3Storage,
    budget: BudgetController,
    compressor: Box<dyn Compressor>,
}

impl L3Engine {
    pub fn new(config: L3Config) -> Self {
        let compressor = build_compressor(&config.distillation, config.compression_max_chars);
        let storage = L3Storage::new(config.storage_path);
        let budget = BudgetController::new(config.budget);
        // The budget controller is memory-only; without backfilling the
        // persisted entry sizes, the annual storage limit restarted from
        // zero on every launch and never actually bounded growth.
        let existing_bytes: usize = storage
            .all()
            .iter()
            .map(|m| BudgetController::estimate_bytes(&m.content_text()))
            .sum();
        if existing_bytes > 0 {
            budget.record_usage(0, existing_bytes);
        }
        Self {
            storage,
            budget,
            compressor,
        }
    }

    pub fn storage(&self) -> &L3Storage {
        &self.storage
    }

    pub fn budget(&self) -> &BudgetController {
        &self.budget
    }

    pub fn insert(&self, input: MemoryInput) -> MemoryResult<u64> {
        let compressed = self.prepare_compressed(input)?;
        let memory = Memory::from_input(compressed);
        let id = memory.id;
        self.storage.store(memory)?;
        Ok(id)
    }

    /// Insert with a pre-determined ID.
    /// Used by MemoryRouter to keep IDs consistent across layers.
    pub fn insert_with_id(&self, input: MemoryInput, id: u64) -> MemoryResult<u64> {
        let compressed = self.prepare_compressed(input)?;
        let memory = Memory::from_input_with_id(compressed, id);
        self.storage.store(memory)?;
        Ok(id)
    }

    /// Insert a pre-built Memory directly (preserving its ID and compressed content).
    /// Budget checks are still applied. Used by MemoryRouter to keep IDs consistent.
    pub fn insert_memory(&self, memory: Memory) -> MemoryResult<u64> {
        let id = memory.id;
        let text_preview = memory.content_text();
        let estimated_tokens = BudgetController::estimate_tokens(&memory.content);
        let estimated_bytes = BudgetController::estimate_bytes(&text_preview);
        self.budget
            .check_budget(estimated_tokens, estimated_bytes)
            .map_err(|e| MemoryError::Internal(e.to_string()))?;
        self.storage.store(memory)?;
        self.budget.record_usage(estimated_tokens, estimated_bytes);
        Ok(id)
    }

    /// Compress content and prepare a MemoryInput with the compressed result.
    /// Budget is checked against the compressed size for accurate storage tracking.
    fn prepare_compressed(&self, input: MemoryInput) -> MemoryResult<MemoryInput> {
        let estimated_tokens = BudgetController::estimate_tokens(&input.content);

        // Compress first so we can check/record budget with actual stored size
        let compressed = self.compressor.compress(&input.content);
        let compressed_text = &compressed.text;

        let estimated_bytes = BudgetController::estimate_bytes(compressed_text);
        self.budget
            .check_budget(estimated_tokens, estimated_bytes)
            .map_err(|e| MemoryError::Internal(e.to_string()))?;
        self.budget.record_usage(estimated_tokens, estimated_bytes);

        Ok(MemoryInput {
            content: MemoryContent::Summary(compressed_text.clone()),
            importance: input.importance,
            alias: input.alias,
            tags: input.tags,
            layer: input.layer,
        })
    }

    pub fn get_by_id(&self, id: u64) -> Option<Memory> {
        self.storage.get(id)
    }

    /// In-place metadata update (e.g. `superseded_by`) without recompressing.
    pub fn update_metadata<F>(&self, id: u64, f: F) -> bool
    where
        F: FnOnce(&mut crate::core::MemoryMeta),
    {
        self.storage.update_metadata(id, f)
    }

    pub fn search(&self, query_text: &str) -> Vec<Memory> {
        self.storage.search_by_text(query_text)
    }

    pub fn get_by_subject(&self, subject: &str) -> Vec<Memory> {
        self.storage.get_by_subject(subject)
    }

    pub fn remove(&self, id: u64) -> bool {
        let mem = { self.storage.get(id) };
        if !self.storage.remove(id) {
            return false;
        }
        if let Some(m) = mem {
            let text = match &m.content {
                MemoryContent::Summary(s) => s.len(),
                MemoryContent::Fact(f) => f.subject.len() + f.predicate.len() + f.object.len(),
                _ => 0,
            };
            self.budget
                .record_removal(BudgetController::estimate_bytes(&"x".repeat(text)));
        }
        true
    }

    pub fn budget_stats(&self) -> BudgetStats {
        self.budget.stats()
    }

    pub fn storage_path(&self) -> &std::path::Path {
        self.storage.storage_path()
    }
}

impl MemoryLayer for L3Engine {
    fn read(&self, query: &Query) -> Result<Option<Memory>, MemoryError> {
        if let Some(id) = query.hash_key() {
            return Ok(self.storage.get(id));
        }
        if let Some(text) = query.text() {
            let results = self.storage.search_by_text(text);
            if results.is_empty() {
                return Ok(None);
            }
            if query.top_k > 1 {
                return Ok(Some(results[0].clone()));
            }
            return Ok(results.into_iter().next());
        }
        Ok(None)
    }

    fn write(&self, input: MemoryInput) -> Result<(), MemoryError> {
        self.insert(input)?;
        Ok(())
    }

    fn could_contain(&self, query: &Query) -> bool {
        if query.hash_key().is_some() {
            return true;
        }
        if query.text().is_some() {
            return !self.storage.is_empty();
        }
        false
    }

    fn len(&self) -> usize {
        self.storage.len()
    }

    fn is_empty(&self) -> bool {
        self.storage.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::Fact;
    use tempfile::tempdir;

    fn make_engine() -> L3Engine {
        let dir = tempdir().unwrap();
        L3Engine::new(L3Config {
            storage_path: dir.path().join("test.bin"),
            budget: BudgetConfig {
                daily_token_limit: 1_000_000,
                annual_storage_limit: 10_000_000,
            },
            compression_max_chars: 200,
            // Deterministic truncation in tests: on MLX-enabled machines the
            // MLX fallback truncates to max_tokens*4 (512), not 200.
            distillation: DistillationConfig {
                enable_mlx: false,
                ..Default::default()
            },
        })
    }

    #[test]
    fn test_new_engine_empty() {
        let e = make_engine();
        assert!(e.is_empty());
    }

    #[test]
    fn test_insert_and_get() {
        let e = make_engine();
        let input = MemoryInput::new(MemoryContent::Fact(Fact::new("user", "says", "hello")));
        let id = e.insert(input).unwrap();
        let mem = e.get_by_id(id);
        assert!(mem.is_some());
        assert_eq!(mem.unwrap().id, id);
    }

    #[test]
    fn test_search_by_text() {
        let e = make_engine();
        e.insert(MemoryInput::new(MemoryContent::Fact(Fact::new(
            "rust", "is", "fast",
        ))))
        .unwrap();
        e.insert(MemoryInput::new(MemoryContent::Fact(Fact::new(
            "python", "is", "slow",
        ))))
        .unwrap();
        let results = e.search("rust");
        assert_eq!(results.len(), 1);
    }

    #[test]
    fn test_memory_layer_read() {
        let e = make_engine();
        let input = MemoryInput::new(MemoryContent::Fact(Fact::new("k", "v", "test")));
        let id = e.insert(input).unwrap();
        let query = Query::by_id(id);
        let result = e.read(&query).unwrap();
        assert!(result.is_some());
        assert_eq!(result.unwrap().id, id);
    }

    #[test]
    fn test_memory_layer_write() {
        let e = make_engine();
        let input = MemoryInput::new(MemoryContent::Fact(Fact::new("a", "b", "c")));
        e.write(input).unwrap();
        assert_eq!(e.len(), 1);
    }

    #[test]
    fn test_budget_tracking() {
        let e = make_engine();
        let input = MemoryInput::new(MemoryContent::Fact(Fact::new("budget", "test", "track")));
        e.insert(input).unwrap();
        let stats = e.budget_stats();
        assert!(stats.tokens_used_today > 0);
        assert!(stats.storage_bytes > 0);
    }

    #[test]
    fn test_remove() {
        let e = make_engine();
        let id = e
            .insert(MemoryInput::new(MemoryContent::Fact(Fact::new(
                "x", "y", "z",
            ))))
            .unwrap();
        assert!(e.remove(id));
        assert!(e.is_empty());
    }

    #[test]
    fn test_compression_stores_summary() {
        let e = make_engine();
        let long_text = "a".repeat(500);
        let input = MemoryInput::new(MemoryContent::Fact(Fact::new(
            "subject",
            "predicate",
            &long_text,
        )));
        let id = e.insert(input).unwrap();
        let mem = e.get_by_id(id).unwrap();
        // Content should be compressed (Summary variant)
        match mem.content {
            MemoryContent::Summary(ref s) => {
                assert!(s.len() <= 200);
            }
            _ => panic!("expected compressed Summary variant"),
        }
    }
}
