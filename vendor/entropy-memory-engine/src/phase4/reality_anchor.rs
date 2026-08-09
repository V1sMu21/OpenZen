use std::collections::HashSet;
use std::sync::{Arc, RwLock};

use crate::memory_store::MemoryStore;
use crate::phase2::types::Conjecture;

#[derive(Debug, Clone)]
pub enum AnchorResult {
    Anchored(String),
    Unanchored,
}

pub struct RealityAnchor {
    anchors: Arc<RwLock<HashSet<u64>>>,
}

impl RealityAnchor {
    pub fn new() -> Self {
        Self {
            anchors: Arc::new(RwLock::new(HashSet::new())),
        }
    }

    pub fn set_anchor(&self, memory_id: u64) {
        self.anchors.write().unwrap().insert(memory_id);
    }

    pub fn remove_anchor(&self, memory_id: u64) {
        self.anchors.write().unwrap().remove(&memory_id);
    }

    pub fn is_anchor(&self, memory_id: u64) -> bool {
        self.anchors.read().unwrap().contains(&memory_id)
    }

    pub fn verify_against_anchors(
        &self,
        _conjecture: &Conjecture,
        _store: &MemoryStore,
    ) -> AnchorResult {
        let text = &_conjecture.statement;
        let query = crate::core::types::Query::by_text(text);
        let results = _store.router().l2().search_semantic(&query, 10);

        let anchors = self.anchors.read().unwrap();
        for (id, dist) in &results {
            if anchors.contains(id) && *dist < 0.3 {
                return AnchorResult::Anchored(format!(
                    "conjecture conflicts with anchor fact {}",
                    id
                ));
            }
        }

        AnchorResult::Unanchored
    }

    pub fn anchor_count(&self) -> usize {
        self.anchors.read().unwrap().len()
    }

    pub fn all_anchors(&self) -> Vec<u64> {
        self.anchors.read().unwrap().iter().copied().collect()
    }
}

impl Default for RealityAnchor {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_anchor() -> RealityAnchor {
        RealityAnchor::new()
    }

    #[test]
    fn test_set_and_check_anchor() {
        let anchor = make_anchor();

        anchor.set_anchor(42);
        assert!(anchor.is_anchor(42));
        assert!(!anchor.is_anchor(99));
    }

    #[test]
    fn test_remove_anchor() {
        let anchor = make_anchor();

        anchor.set_anchor(42);
        anchor.remove_anchor(42);
        assert!(!anchor.is_anchor(42));
    }

    #[test]
    fn test_anchor_count() {
        let anchor = make_anchor();

        assert_eq!(anchor.anchor_count(), 0);
        anchor.set_anchor(1);
        anchor.set_anchor(2);
        assert_eq!(anchor.anchor_count(), 2);
    }
}
