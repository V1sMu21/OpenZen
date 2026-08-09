use std::collections::HashMap;
use std::sync::{Arc, RwLock};

pub struct DiversityRegularizer {
    path_frequency: Arc<RwLock<HashMap<(u64, u64), u64>>>,
    temperature: f32,
}

impl DiversityRegularizer {
    pub fn new(temperature: f32) -> Self {
        Self {
            path_frequency: Arc::new(RwLock::new(HashMap::new())),
            temperature,
        }
    }

    pub fn adjust_weights(&self, candidates: &[(u64, f32)]) -> Vec<(u64, f32)> {
        let freqs = self.path_frequency.read().unwrap();
        candidates
            .iter()
            .map(|(id, w)| {
                let freq = freqs
                    .iter()
                    .filter(|((_, to), _)| *to == *id)
                    .map(|(_, c)| *c)
                    .sum::<u64>() as f32;
                let adjusted = *w / (1.0 + freq * self.temperature);
                (*id, adjusted)
            })
            .collect()
    }

    pub fn record_path(&self, from: u64, to: u64) {
        let mut freqs = self.path_frequency.write().unwrap();
        *freqs.entry((from, to)).or_insert(0) += 1;
    }

    pub fn explore_unvisited(&self, node: u64, candidates: &[(u64, f32)]) -> Vec<(u64, f32)> {
        let freqs = self.path_frequency.read().unwrap();
        let mut unvisited: Vec<(u64, f32)> = candidates
            .iter()
            .filter(|(id, _)| !freqs.contains_key(&(node, *id)))
            .map(|(id, w)| (*id, *w))
            .collect();
        if !unvisited.is_empty() {
            unvisited.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
        }
        unvisited
    }

    pub fn temperature(&self) -> f32 {
        self.temperature
    }

    pub fn reset(&self) {
        self.path_frequency.write().unwrap().clear();
    }
}

impl Default for DiversityRegularizer {
    fn default() -> Self {
        Self::new(1.5)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_adjust_weights_reduces_frequent() {
        let dr = DiversityRegularizer::new(1.5);
        dr.record_path(1, 2);
        dr.record_path(1, 2);

        let candidates = vec![(2, 1.0), (3, 1.0)];
        let adjusted = dr.adjust_weights(&candidates);

        assert!(
            adjusted[0].1 < adjusted[1].1,
            "frequent path should have lower weight, got {:?}",
            adjusted
        );
    }

    #[test]
    fn test_explore_unvisited() {
        let dr = DiversityRegularizer::new(1.0);
        dr.record_path(1, 2);

        let candidates = vec![(2, 0.5), (3, 0.8), (4, 0.3)];
        let unvisited = dr.explore_unvisited(1, &candidates);

        assert_eq!(unvisited.len(), 2);
        assert_eq!(unvisited[0].0, 3); // highest weight unvisited first
    }

    #[test]
    fn test_record_path() {
        let dr = DiversityRegularizer::new(1.0);
        dr.record_path(1, 2);
        dr.record_path(1, 2);

        let freqs = dr.path_frequency.read().unwrap();
        assert_eq!(freqs.get(&(1, 2)), Some(&2));
    }

    #[test]
    fn test_reset() {
        let dr = DiversityRegularizer::new(1.0);
        dr.record_path(1, 2);
        dr.reset();

        let freqs = dr.path_frequency.read().unwrap();
        assert!(freqs.is_empty());
    }
}
