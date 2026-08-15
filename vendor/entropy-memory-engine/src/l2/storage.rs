use std::collections::HashMap;

use parking_lot::RwLock;

use crate::core::{Memory, MemoryResult};

pub struct L2Storage {
    memories: RwLock<HashMap<u64, Memory>>,
    subject_index: RwLock<HashMap<String, Vec<u64>>>,
}

impl Default for L2Storage {
    fn default() -> Self {
        Self::new()
    }
}

impl L2Storage {
    pub fn new() -> Self {
        Self {
            memories: RwLock::new(HashMap::new()),
            subject_index: RwLock::new(HashMap::new()),
        }
    }

    pub fn len(&self) -> usize {
        self.memories.read().len()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    pub fn store(&self, memory: Memory) -> MemoryResult<()> {
        let id = memory.id;
        if let crate::core::MemoryContent::Fact(ref fact) = memory.content {
            self.subject_index
                .write()
                .entry(fact.subject.clone())
                .or_default()
                .push(id);
        }
        self.memories.write().insert(id, memory);
        Ok(())
    }

    pub fn get(&self, id: u64) -> Option<Memory> {
        self.memories.read().get(&id).cloned()
    }

    /// In-place metadata update for an existing memory.
    ///
    /// Unlike store-then-reinsert, this mutates only `MemoryMeta` inside the
    /// lock — no vector re-embedding, no HNSW re-insertion, and no loss of
    /// fields that `Memory::from_input_with_id` would reset (e.g.
    /// `superseded_by`, `created_at`, `factuality`).
    pub fn update_metadata<F>(&self, id: u64, f: F) -> bool
    where
        F: FnOnce(&mut crate::core::MemoryMeta),
    {
        let mut memories = self.memories.write();
        if let Some(mem) = memories.get_mut(&id) {
            f(&mut mem.metadata);
            true
        } else {
            false
        }
    }

    pub fn get_by_subject(&self, subject: &str) -> Vec<Memory> {
        let index = self.subject_index.read();
        let memories = self.memories.read();
        match index.get(subject) {
            Some(ids) => ids
                .iter()
                .filter_map(|id| memories.get(id).cloned())
                .collect(),
            None => Vec::new(),
        }
    }

    pub fn remove(&self, id: u64) -> bool {
        let mut memories = self.memories.write();
        if let Some(mem) = memories.remove(&id) {
            // 清理主题索引
            if let crate::core::MemoryContent::Fact(ref fact) = mem.content {
                let mut index = self.subject_index.write();
                if let Some(ids) = index.get_mut(&fact.subject) {
                    ids.retain(|i| *i != id);
                    if ids.is_empty() {
                        index.remove(&fact.subject);
                    }
                }
            }
            true
        } else {
            false
        }
    }

    /// 获取所有记忆 ID
    pub fn all_ids(&self) -> Vec<u64> {
        self.memories.read().keys().copied().collect()
    }

    /// 清空所有数据
    pub fn clear(&self) {
        self.memories.write().clear();
        self.subject_index.write().clear();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::{Fact, Memory, MemoryContent};

    fn make_memory(id: u64, subject: &str) -> Memory {
        let mut mem = Memory::new(MemoryContent::Fact(Fact::new(subject, "test", "value")));
        mem.id = id;
        mem
    }

    #[test]
    fn test_empty_storage() {
        let s = L2Storage::new();
        assert!(s.is_empty());
        assert_eq!(s.len(), 0);
    }

    #[test]
    fn test_store_and_get() {
        let s = L2Storage::new();
        let m = make_memory(1, "alice");
        s.store(m.clone()).unwrap();
        assert_eq!(s.len(), 1);
        let retrieved = s.get(1);
        assert!(retrieved.is_some());
        assert_eq!(retrieved.unwrap().id, 1);
    }

    #[test]
    fn test_get_not_found() {
        let s = L2Storage::new();
        assert!(s.get(999).is_none());
    }

    #[test]
    fn test_get_by_subject() {
        let s = L2Storage::new();
        s.store(make_memory(1, "alice")).unwrap();
        s.store(make_memory(2, "alice")).unwrap();
        s.store(make_memory(3, "bob")).unwrap();
        let alice_mems = s.get_by_subject("alice");
        assert_eq!(alice_mems.len(), 2);
        let bob_mems = s.get_by_subject("bob");
        assert_eq!(bob_mems.len(), 1);
    }

    #[test]
    fn test_remove() {
        let s = L2Storage::new();
        s.store(make_memory(1, "alice")).unwrap();
        assert!(s.remove(1));
        assert!(s.is_empty());
        assert!(!s.remove(999));
    }

    #[test]
    fn test_clear() {
        let s = L2Storage::new();
        s.store(make_memory(1, "alice")).unwrap();
        s.store(make_memory(2, "bob")).unwrap();
        assert_eq!(s.len(), 2);
        s.clear();
        assert!(s.is_empty());
    }

    #[test]
    fn test_all_ids() {
        let s = L2Storage::new();
        s.store(make_memory(10, "x")).unwrap();
        s.store(make_memory(20, "y")).unwrap();
        let mut ids = s.all_ids();
        ids.sort();
        assert_eq!(ids, vec![10, 20]);
    }

    #[test]
    fn test_subject_index_cleanup_on_remove() {
        let s = L2Storage::new();
        s.store(make_memory(1, "alice")).unwrap();
        s.remove(1);
        assert!(s.get_by_subject("alice").is_empty());
    }
}
