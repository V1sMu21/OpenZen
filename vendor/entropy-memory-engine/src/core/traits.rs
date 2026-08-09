use crate::core::error::MemoryError;
use crate::core::types::{Memory, MemoryInput, Query};

pub trait MemoryLayer: Send + Sync {
    fn read(&self, query: &Query) -> Result<Option<Memory>, MemoryError>;
    fn write(&self, memory: MemoryInput) -> Result<(), MemoryError>;
    fn could_contain(&self, query: &Query) -> bool;
    fn len(&self) -> usize;
    fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

pub trait EvictionPolicy: Send + Sync {
    fn score(&self, memory: &Memory) -> f64;
    fn should_evict(&self, memory: &Memory, replacement: &Memory) -> bool;
    fn record_access(&mut self, memory_id: u64);
    fn record_hit(&mut self, memory_id: u64);
}
