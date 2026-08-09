use crate::l2::TimeGraph;
use crate::phase2::types::SSSScore;

pub struct AestheticScorer {
    time_graph: std::sync::Arc<TimeGraph>,
}

impl AestheticScorer {
    pub fn new(time_graph: std::sync::Arc<TimeGraph>) -> Self {
        Self { time_graph }
    }

    pub fn score(&self, node_a: u64, node_b: u64) -> SSSScore {
        let compressibility = self.evaluate_compressibility(node_a, node_b);
        let path_len = self
            .time_graph
            .shortest_path_length(node_a, node_b)
            .unwrap_or(10);
        let unexpectedness = 1.0 / (1.0 + path_len as f32);
        let composite = compressibility / unexpectedness.max(0.001);

        SSSScore {
            simplicity: compressibility,
            surprise: unexpectedness,
            composite,
        }
    }

    fn evaluate_compressibility(&self, _node_a: u64, _node_b: u64) -> f32 {
        // Default: two nodes are compressible if they share neighbors.
        // In production, this would call MLX LLM to evaluate rule compressibility.
        let neighbors_a = self.time_graph.weighted_neighbors(_node_a);
        let neighbors_b = self.time_graph.weighted_neighbors(_node_b);

        let mut common = 0;
        for (id_a, _) in &neighbors_a {
            if neighbors_b.iter().any(|(id_b, _)| id_b == id_a) {
                common += 1;
            }
        }

        let total = (neighbors_a.len() + neighbors_b.len()).max(1) as f32;
        (common as f32 / total).min(1.0).max(0.1)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::l2::TimeGraph;

    #[test]
    fn test_score_connected_nodes() {
        let g = std::sync::Arc::new(TimeGraph::new());
        g.add_edge(1, 2, "knows".into(), 1.0);
        g.add_edge(1, 3, "knows".into(), 1.0);
        g.add_edge(2, 3, "knows".into(), 1.0);

        let scorer = AestheticScorer::new(g);
        let score = scorer.score(1, 3);
        assert!(score.composite > 0.0);
    }

    #[test]
    fn test_score_unconnected_nodes() {
        let g = std::sync::Arc::new(TimeGraph::new());
        g.add_node(1, vec![]);
        g.add_node(2, vec![]);

        let scorer = AestheticScorer::new(g);
        let score = scorer.score(1, 2);
        assert!(score.surprise > 0.0, "unconnected should have surprise");
    }
}
