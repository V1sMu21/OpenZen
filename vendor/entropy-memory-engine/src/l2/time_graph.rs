use std::collections::HashMap;

use parking_lot::RwLock;

use crate::core::now_nanos;

/// 时间感知图节点
#[derive(Debug, Clone)]
pub struct GraphNode {
    pub id: u64,
    pub labels: Vec<String>,
    pub created_at: i64,
}

/// 时间感知图边
#[derive(Debug, Clone)]
pub struct GraphEdge {
    pub id: u64,
    pub source: u64,
    pub target: u64,
    pub relation: String,
    pub weight: f32,
    pub created_at: i64,
    pub co_activation_count: u32,
    pub last_co_activation: i64,
    pub structural_similarity: f32,
}

/// 时间感知图索引
///
/// 维护实体之间的带时间戳关系, 支持:
/// - 时间范围过滤的邻居查询
/// - 路径遍历
/// - 按关系类型过滤
pub struct TimeGraph {
    nodes: RwLock<HashMap<u64, GraphNode>>,
    edges: RwLock<Vec<GraphEdge>>,
    /// source → edge 索引列表
    outgoing: RwLock<HashMap<u64, Vec<usize>>>,
    /// target → edge 索引列表
    incoming: RwLock<HashMap<u64, Vec<usize>>>,
    next_edge_id: RwLock<u64>,
}

impl Default for TimeGraph {
    fn default() -> Self {
        Self::new()
    }
}

impl TimeGraph {
    pub fn new() -> Self {
        Self {
            nodes: RwLock::new(HashMap::new()),
            edges: RwLock::new(Vec::new()),
            outgoing: RwLock::new(HashMap::new()),
            incoming: RwLock::new(HashMap::new()),
            next_edge_id: RwLock::new(1),
        }
    }

    pub fn node_count(&self) -> usize {
        self.nodes.read().len()
    }

    pub fn edge_count(&self) -> usize {
        self.edges.read().len()
    }

    /// 添加或更新节点
    pub fn add_node(&self, id: u64, labels: Vec<String>) {
        let mut nodes = self.nodes.write();
        nodes.insert(
            id,
            GraphNode {
                id,
                labels,
                created_at: now_nanos(),
            },
        );
    }

    /// 添加带时间戳的边
    pub fn add_edge(&self, source: u64, target: u64, relation: String, weight: f32) {
        if !self.nodes.read().contains_key(&source) {
            self.add_node(source, Vec::new());
        }
        if !self.nodes.read().contains_key(&target) {
            self.add_node(target, Vec::new());
        }

        let mut edge_id = self.next_edge_id.write();
        let id = *edge_id;
        *edge_id += 1;

        let edge = GraphEdge {
            id,
            source,
            target,
            relation,
            weight,
            created_at: now_nanos(),
            co_activation_count: 0,
            last_co_activation: 0,
            structural_similarity: 0.0,
        };

        let idx = {
            let mut edges = self.edges.write();
            edges.push(edge);
            edges.len() - 1
        };

        self.outgoing.write().entry(source).or_default().push(idx);
        self.incoming.write().entry(target).or_default().push(idx);
    }

    /// 查询邻居节点
    ///
    /// * `node_id` — 起始节点
    /// * `relation_filter` — 可选, 只返回指定关系的边
    /// * `time_after` — 可选, 只返回在指定时间之后的边
    /// * `time_before` — 可选, 只返回在指定时间之前的边
    pub fn query_neighbors(
        &self,
        node_id: u64,
        relation_filter: Option<&str>,
        time_after: Option<i64>,
        time_before: Option<i64>,
    ) -> Vec<(GraphEdge, Option<GraphNode>)> {
        let edges = self.edges.read();
        let nodes = self.nodes.read();
        let outgoing = self.outgoing.read();
        let incoming = self.incoming.read();

        let mut results = Vec::new();

        // 出边
        if let Some(indices) = outgoing.get(&node_id) {
            for &idx in indices {
                if idx >= edges.len() {
                    continue;
                }
                let edge = &edges[idx];
                if let Some(rel) = relation_filter {
                    if edge.relation != rel {
                        continue;
                    }
                }
                if let Some(ta) = time_after {
                    if edge.created_at < ta {
                        continue;
                    }
                }
                if let Some(tb) = time_before {
                    if edge.created_at > tb {
                        continue;
                    }
                }
                let target_node = nodes.get(&edge.target).cloned();
                results.push((edge.clone(), target_node));
            }
        }

        // 入边
        if let Some(indices) = incoming.get(&node_id) {
            for &idx in indices {
                if idx >= edges.len() {
                    continue;
                }
                let edge = &edges[idx];
                if let Some(rel) = relation_filter {
                    if edge.relation != rel {
                        continue;
                    }
                }
                if let Some(ta) = time_after {
                    if edge.created_at < ta {
                        continue;
                    }
                }
                if let Some(tb) = time_before {
                    if edge.created_at > tb {
                        continue;
                    }
                }
                let source_node = nodes.get(&edge.source).cloned();
                results.push((edge.clone(), source_node));
            }
        }

        results
    }

    /// BFS 查找路径
    pub fn find_paths(&self, from: u64, to: u64, max_hops: usize) -> Vec<Vec<u64>> {
        let outgoing = self.outgoing.read();
        let edges = self.edges.read();

        let mut paths = Vec::new();
        let mut visited = vec![from];
        let mut current = vec![(from, vec![from])];

        for _ in 0..max_hops {
            let mut next = Vec::new();
            for (node_id, path) in &current {
                if let Some(indices) = outgoing.get(node_id) {
                    for &idx in indices {
                        if idx >= edges.len() {
                            continue;
                        }
                        let edge = &edges[idx];
                        if !visited.contains(&edge.target) {
                            let mut new_path = path.clone();
                            new_path.push(edge.target);
                            if edge.target == to {
                                paths.push(new_path.clone());
                            } else {
                                next.push((edge.target, new_path));
                            }
                            visited.push(edge.target);
                        }
                    }
                }
            }
            current = next;
            if !paths.is_empty() {
                break;
            }
        }

        paths
    }

    /// 时间范围查询: 在指定时间范围内的所有边
    pub fn temporal_query(
        &self,
        time_after: i64,
        time_before: i64,
    ) -> Vec<(u64, u64, String, f32, i64)> {
        let edges = self.edges.read();
        edges
            .iter()
            .filter(|e| e.created_at >= time_after && e.created_at <= time_before)
            .map(|e| {
                (
                    e.source,
                    e.target,
                    e.relation.clone(),
                    e.weight,
                    e.created_at,
                )
            })
            .collect()
    }

    /// 删除节点及其所有边
    pub fn remove_node(&self, id: u64) {
        self.nodes.write().remove(&id);

        let mut edges = self.edges.write();
        let mut outgoing = self.outgoing.write();
        let mut incoming = self.incoming.write();

        // 找出要删除的边索引
        let to_remove: Vec<usize> = edges
            .iter()
            .enumerate()
            .filter(|(_, e)| e.source == id || e.target == id)
            .map(|(i, _)| i)
            .collect();

        // 从 outgoing/incoming 中移除
        outgoing.remove(&id);
        incoming.remove(&id);
        for &idx in &to_remove {
            if idx < edges.len() {
                let edge = &edges[idx];
                if let Some(out) = outgoing.get_mut(&edge.source) {
                    out.retain(|i| *i != idx);
                }
                if let Some(inc) = incoming.get_mut(&edge.target) {
                    inc.retain(|i| *i != idx);
                }
            }
        }

        // 从后往前删除 (保持索引有效)
        let mut sorted: Vec<usize> = to_remove.clone();
        sorted.sort_by(|a, b| b.cmp(a));
        for idx in sorted {
            if idx < edges.len() {
                edges.remove(idx);
            }
        }
    }

    /// 按关系类型删除边
    pub fn remove_edges_by_relation(&self, relation: &str) -> usize {
        let mut edges = self.edges.write();
        let outgoing = self.outgoing.read();
        let incoming = self.incoming.read();

        let before = edges.len();
        edges.retain(|e| e.relation != relation);
        let removed = before - edges.len();

        // 清理索引中失效的引用
        drop(outgoing);
        drop(incoming);
        self.rebuild_index();
        removed
    }

    fn rebuild_index(&self) {
        let edges = self.edges.read();
        let mut outgoing = self.outgoing.write();
        let mut incoming = self.incoming.write();

        outgoing.clear();
        incoming.clear();
        for (i, edge) in edges.iter().enumerate() {
            outgoing.entry(edge.source).or_default().push(i);
            incoming.entry(edge.target).or_default().push(i);
        }
    }

    pub fn update_association(&self, from: u64, to: u64, semantic_sim: f32, structural_sim: f32) {
        let now = crate::core::now_nanos();
        let edges = self.edges.read();

        let existing_idx = edges
            .iter()
            .position(|e| e.source == from && e.target == to && e.relation == "association");

        drop(edges);

        if let Some(idx) = existing_idx {
            let mut edges = self.edges.write();
            if let Some(edge) = edges.get_mut(idx) {
                let old_weight = edge.weight;
                let co_act = (edge.co_activation_count + 1) as f32;
                edge.co_activation_count += 1;
                edge.last_co_activation = now;
                edge.structural_similarity = structural_sim;
                edge.weight = old_weight * 0.95
                    + 0.3 * semantic_sim
                    + 0.3 * co_act.min(10.0) / 10.0
                    + 0.4 * structural_sim;
            }
        } else {
            self.add_edge(from, to, "association".into(), semantic_sim);
        }
    }

    pub fn decay_all_weights(&self, gamma: f32) {
        let mut edges = self.edges.write();
        for edge in edges.iter_mut() {
            edge.weight *= gamma;
        }
    }

    pub fn weighted_neighbors(&self, node_id: u64) -> Vec<(u64, f32)> {
        let edges = self.edges.read();
        let outgoing = self.outgoing.read();
        let mut results = Vec::new();

        if let Some(indices) = outgoing.get(&node_id) {
            for &idx in indices {
                if let Some(edge) = edges.get(idx) {
                    results.push((edge.target, edge.weight));
                }
            }
        }

        let incoming = self.incoming.read();
        if let Some(indices) = incoming.get(&node_id) {
            for &idx in indices {
                if let Some(edge) = edges.get(idx) {
                    if !results.iter().any(|(id, _)| *id == edge.source) {
                        results.push((edge.source, edge.weight));
                    }
                }
            }
        }

        results.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
        results
    }

    pub fn shortest_path_length(&self, from: u64, to: u64) -> Option<usize> {
        let paths = self.find_paths(from, to, 5);
        paths.iter().map(|p| p.len()).min()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn setup_graph() -> TimeGraph {
        let g = TimeGraph::new();
        g.add_node(1, vec!["person".into()]);
        g.add_node(2, vec!["person".into()]);
        g.add_node(3, vec!["project".into()]);
        g.add_edge(1, 2, "knows".into(), 1.0);
        g.add_edge(1, 3, "works_on".into(), 0.8);
        g.add_edge(2, 3, "contributes_to".into(), 0.6);
        g
    }

    #[test]
    fn test_empty_graph() {
        let g = TimeGraph::new();
        assert_eq!(g.node_count(), 0);
        assert_eq!(g.edge_count(), 0);
    }

    #[test]
    fn test_add_node() {
        let g = TimeGraph::new();
        g.add_node(42, vec!["test".into()]);
        assert_eq!(g.node_count(), 1);
    }

    #[test]
    fn test_add_edge() {
        let g = TimeGraph::new();
        g.add_edge(1, 2, "related".into(), 0.5);
        assert_eq!(g.node_count(), 2); // 自动创建节点
        assert_eq!(g.edge_count(), 1);
    }

    #[test]
    fn test_query_neighbors() {
        let g = setup_graph();
        let neighbors = g.query_neighbors(1, None, None, None);
        // 节点 1 有 2 条出边 (→2, →3)
        assert_eq!(neighbors.len(), 2);
    }

    #[test]
    fn test_query_neighbors_with_relation_filter() {
        let g = setup_graph();
        let neighbors = g.query_neighbors(1, Some("knows"), None, None);
        assert_eq!(neighbors.len(), 1);
        assert_eq!(neighbors[0].0.target, 2);
    }

    #[test]
    fn test_query_neighbors_with_time_filter() {
        let g = setup_graph();
        let now = now_nanos();
        // 所有边都在 "未来" 之后 → 全部匹配
        let neighbors = g.query_neighbors(1, None, Some(0), Some(now + 1_000_000_000));
        assert_eq!(neighbors.len(), 2);
        // 所有边都在 "未来" 之前 → 无匹配
        let neighbors = g.query_neighbors(1, None, Some(now + 1_000_000_000), None);
        assert_eq!(neighbors.len(), 0);
    }

    #[test]
    fn test_find_paths() {
        let g = setup_graph();
        let paths = g.find_paths(1, 3, 3);
        // 1 → 3 (direct) + 1 → 2 → 3
        assert!(!paths.is_empty());
        // 至少有一条直接路径
        let has_direct = paths.iter().any(|p| p.len() == 2 && p[0] == 1 && p[1] == 3);
        assert!(has_direct);
    }

    #[test]
    fn test_temporal_query() {
        let g = TimeGraph::new();
        g.add_edge(1, 2, "meets".into(), 1.0);
        std::thread::sleep(std::time::Duration::from_millis(2));
        let before = now_nanos();
        std::thread::sleep(std::time::Duration::from_millis(2));
        g.add_edge(2, 3, "meets".into(), 1.0);
        let after = now_nanos();

        let results = g.temporal_query(before, after);
        assert_eq!(results.len(), 1); // 只有第二个边在时间范围内
        assert_eq!(results[0].0, 2);
        assert_eq!(results[0].1, 3);
    }

    #[test]
    fn test_remove_node() {
        let g = setup_graph();
        assert_eq!(g.node_count(), 3);
        g.remove_node(1);
        assert_eq!(g.node_count(), 2);
        // 边的索引清理后, 查询应返回 0
        let neighbors = g.query_neighbors(1, None, None, None);
        assert_eq!(neighbors.len(), 0);
    }

    #[test]
    fn test_incoming_neighbors() {
        let g = setup_graph();
        // 节点 3 有 2 条入边 (←1, ←2)
        let neighbors = g.query_neighbors(3, None, None, None);
        assert_eq!(neighbors.len(), 2);
    }

    #[test]
    fn test_node_labels() {
        let g = TimeGraph::new();
        g.add_node(1, vec!["person".into(), "engineer".into()]);
        g.add_edge(1, 2, "mentors".into(), 0.9);
        let neighbors = g.query_neighbors(1, None, None, None);
        assert!(!neighbors.is_empty());
        // 目标节点 2 没有标签
        assert_eq!(
            neighbors[0].1.as_ref().map(|n| n.labels.clone()),
            Some(vec![])
        );
    }
}
