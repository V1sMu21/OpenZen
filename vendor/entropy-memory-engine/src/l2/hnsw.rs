use std::collections::{BinaryHeap, HashSet};
use std::sync::atomic::{AtomicU64, Ordering};

use parking_lot::RwLock;
use rand::Rng;
use rand::SeedableRng;

/// 距离类型
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DistanceMetric {
    /// 欧几里得距离平方 (L2²)
    EuclideanSquared,
    /// 余弦距离 (1 - cosine_similarity)
    Cosine,
}

/// HNSW 索引配置
#[derive(Debug, Clone)]
pub struct HnswConfig {
    /// 每节点最大连接数 (上层)
    pub m: usize,
    /// 底层最大连接数 (通常 2*M)
    pub m_max: usize,
    /// 构建时动态候选列表大小
    pub ef_construction: usize,
    /// 搜索时动态候选列表大小
    pub ef_search: usize,
    /// 层级归一化因子
    pub m_l: f64,
    /// 向量维度
    pub dimension: usize,
    /// 距离度量
    pub distance: DistanceMetric,
}

impl Default for HnswConfig {
    fn default() -> Self {
        Self {
            m: 16,
            m_max: 32,
            ef_construction: 200,
            ef_search: 50,
            m_l: 1.0 / (16.0_f64).ln(),
            dimension: 384,
            distance: DistanceMetric::EuclideanSquared,
        }
    }
}

/// HNSW 图中的节点
#[derive(Debug, Clone)]
struct HnswNode {
    id: u64,
    vector: Vec<f32>,
    /// neighbors[level] = 该层邻居 ID 列表
    neighbors: Vec<Vec<u64>>,
    /// Tombstone: true 表示已逻辑删除, len/all_ids/search 跳过
    deleted: bool,
}

impl HnswNode {
    fn new(id: u64, vector: Vec<f32>, level: usize) -> Self {
        let neighbors = (0..=level).map(|_| Vec::new()).collect();
        Self {
            id,
            vector,
            neighbors,
            deleted: false,
        }
    }
}

/// 墓碑 (deleted) 节点占比超过该阈值时触发物理回收
const REBUILD_THRESHOLD: f32 = 0.3;

/// HNSW 层级近邻小世界图索引
///
/// 实现参考: Malkov & Yashunin (2018) "Efficient and robust approximate nearest
/// neighbor search using Hierarchical Navigable Small World graphs"
pub struct HnswIndex {
    config: HnswConfig,
    nodes: RwLock<Vec<HnswNode>>,
    /// id → nodes 中的索引
    id_to_idx: RwLock<Vec<(u64, usize)>>,
    entry_point: RwLock<Option<usize>>,
    max_level: RwLock<usize>,
    next_idx: AtomicU64,
    /// 随机层级生成器种子
    rand_seed: AtomicU64,
}

impl HnswIndex {
    pub fn new(config: HnswConfig) -> Self {
        Self {
            config,
            nodes: RwLock::new(Vec::new()),
            id_to_idx: RwLock::new(Vec::new()),
            entry_point: RwLock::new(None),
            max_level: RwLock::new(0),
            next_idx: AtomicU64::new(0),
            rand_seed: AtomicU64::new(42),
        }
    }

    pub fn config(&self) -> &HnswConfig {
        &self.config
    }

    pub fn len(&self) -> usize {
        self.nodes.read().iter().filter(|n| !n.deleted).count()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// 距离计算: L2 平方距离
    fn distance(a: &[f32], b: &[f32]) -> f32 {
        a.iter()
            .zip(b.iter())
            .map(|(x, y)| {
                let d = x - y;
                d * d
            })
            .sum()
    }

    /// 余弦距离
    fn cosine_distance(a: &[f32], b: &[f32]) -> f32 {
        let dot: f32 = a.iter().zip(b.iter()).map(|(x, y)| x * y).sum();
        let norm_a: f32 = a.iter().map(|x| x * x).sum::<f32>().sqrt();
        let norm_b: f32 = b.iter().map(|x| x * x).sum::<f32>().sqrt();
        if norm_a == 0.0 || norm_b == 0.0 {
            return 1.0;
        }
        1.0 - (dot / (norm_a * norm_b)).clamp(-1.0, 1.0)
    }

    fn compute_distance(&self, a: &[f32], b: &[f32]) -> f32 {
        match self.config.distance {
            DistanceMetric::EuclideanSquared => Self::distance(a, b),
            DistanceMetric::Cosine => Self::cosine_distance(a, b),
        }
    }

    /// 生成本层随机层级 (指数衰减)
    fn random_level(&self) -> usize {
        let seed = self.rand_seed.fetch_add(1, Ordering::Relaxed);
        let mut rng = rand::rngs::StdRng::seed_from_u64(seed);
        let r: f64 = rng.gen();
        (-r.ln() * self.config.m_l).floor() as usize
    }

    /// Single-layer search: from `entry_idx`, find `ef` nearest neighbors at `layer`.
    fn search_layer(
        &self,
        query: &[f32],
        entry_idx: usize,
        layer: usize,
        ef: usize,
        nodes: &[HnswNode],
    ) -> Vec<(usize, f32)> {
        // Guard: entry 被 tombstone 时返回空; 层级不足时返回入口距离
        if nodes[entry_idx].deleted {
            return Vec::new();
        }
        if layer >= nodes[entry_idx].neighbors.len() {
            let dist = self.compute_distance(query, &nodes[entry_idx].vector);
            return vec![(entry_idx, dist)];
        }

        let mut visited = HashSet::new();
        let mut candidates = BinaryHeap::new();

        let entry_dist = self.compute_distance(query, &nodes[entry_idx].vector);
        visited.insert(entry_idx);
        candidates.push(NeighborEntry {
            idx: entry_idx,
            dist: entry_dist,
            is_query: true,
        });

        let mut result = BinaryHeap::new();
        result.push(NeighborEntry {
            idx: entry_idx,
            dist: entry_dist,
            is_query: false,
        });

        while let Some(current) = candidates.pop() {
            let furthest_in_result = result.peek().map(|e| e.dist).unwrap_or(f32::MAX);
            if current.dist > furthest_in_result {
                break;
            }

            // Skip nodes whose level is below the current layer
            if layer >= nodes[current.idx].neighbors.len() {
                continue;
            }

            for &neighbor_idx in &nodes[current.idx].neighbors[layer] {
                let nidx = neighbor_idx as usize;
                // 跳过 tombstone 节点
                if visited.contains(&nidx) || nodes[nidx].deleted {
                    continue;
                }
                visited.insert(nidx);

                let dist = self.compute_distance(query, &nodes[nidx].vector);
                let furthest = result.peek().map(|e| e.dist).unwrap_or(f32::MAX);

                if dist < furthest || result.len() < ef {
                    candidates.push(NeighborEntry {
                        idx: nidx,
                        dist,
                        is_query: true,
                    });
                    result.push(NeighborEntry {
                        idx: nidx,
                        dist,
                        is_query: false,
                    });
                    if result.len() > ef {
                        result.pop();
                    }
                }
            }
        }

        result
            .into_sorted_vec()
            .into_iter()
            .map(|e| (e.idx, e.dist))
            .collect()
    }

    /// 选择最近邻 (简单: 取最近距离的 M 个)
    fn select_neighbors_simple(candidates: &[(usize, f32)], max_conn: usize) -> Vec<usize> {
        let mut sorted: Vec<(usize, f32)> = candidates.to_vec();
        sorted.sort_by(|a, b| a.1.total_cmp(&b.1));
        sorted.truncate(max_conn);
        sorted.into_iter().map(|(idx, _)| idx).collect()
    }

    /// 插入向量到索引
    pub fn insert(&self, id: u64, vector: Vec<f32>) {
        assert_eq!(vector.len(), self.config.dimension);

        // 同 id 已存在: 原地更新向量并复活 tombstone (不重建连接)
        {
            let mut nodes = self.nodes.write();
            let mut map = self.id_to_idx.write();
            if let Some((_, existing_idx)) = map.iter().find(|(stored_id, _)| *stored_id == id) {
                if *existing_idx < nodes.len() {
                    nodes[*existing_idx].deleted = false;
                    nodes[*existing_idx].vector = vector;
                    return;
                }
            }
            // 清除同 id 的旧映射 (避免重复条目指向陈旧索引)
            map.retain(|(stored_id, _)| *stored_id != id);
        }

        let level = self.random_level();
        let idx = self.next_idx.fetch_add(1, Ordering::Relaxed) as usize;

        let mut nodes = self.nodes.write();

        // 扩展 nodes 确保 idx 存在 (占位节点标记为 tombstone, 避免 len() 误计)
        while nodes.len() <= idx {
            let mut placeholder = HnswNode::new(0, vec![0.0; self.config.dimension], 0);
            placeholder.deleted = true;
            nodes.push(placeholder);
        }
        nodes[idx] = HnswNode::new(id, vector, level);

        // 更新 id→idx 映射 (再次清除同 id 旧映射, 保持唯一)
        {
            let mut map = self.id_to_idx.write();
            map.retain(|(stored_id, _)| *stored_id != id);
            map.push((id, idx));
        }

        let entry = *self.entry_point.read();

        if let Some(entry_idx) = entry {
            let max_level = *self.max_level.read();

            // 从顶层向下搜索到 level+1
            let mut curr_entry = entry_idx;
            for l in (level + 1..=max_level).rev() {
                let candidates =
                    self.search_layer(&nodes[curr_entry].vector, curr_entry, l, 1, &nodes);
                if let Some(best) = candidates.first() {
                    curr_entry = best.0;
                }
            }

            // 在 level 到 0 层连接邻居
            for l in (0..=level).rev() {
                let candidates = self.search_layer(
                    &nodes[idx].vector,
                    curr_entry,
                    l,
                    self.config.ef_construction,
                    &nodes,
                );
                let max_conn_l = if l == 0 {
                    self.config.m_max
                } else {
                    self.config.m
                };
                let neighbors = Self::select_neighbors_simple(&candidates, max_conn_l);

                for &neighbor_idx in &neighbors {
                    nodes[idx].neighbors[l].push(neighbor_idx as u64);
                    // Bidirectional connection — only if neighbor has this layer
                    if neighbor_idx < nodes.len() && l < nodes[neighbor_idx].neighbors.len() {
                        nodes[neighbor_idx].neighbors[l].push(idx as u64);
                        if nodes[neighbor_idx].neighbors[l].len() > max_conn_l {
                            Self::shrink_connections(&mut nodes[neighbor_idx], l, max_conn_l);
                        }
                    }
                }

                if !candidates.is_empty() {
                    curr_entry = candidates[0].0;
                }
            }

            if level > max_level {
                *self.max_level.write() = level;
                *self.entry_point.write() = Some(idx);
            }
        } else {
            // 第一个节点
            *self.entry_point.write() = Some(idx);
            *self.max_level.write() = level;
        }

        drop(nodes);
    }

    /// 搜索 k 个最近邻
    pub fn search(&self, query: &[f32], k: usize) -> Vec<(u64, f32)> {
        assert_eq!(query.len(), self.config.dimension);

        let nodes = self.nodes.read();
        let mut entry_idx = match *self.entry_point.read() {
            Some(idx) => idx,
            None => return Vec::new(),
        };
        // 防御: 入口点不应是 tombstone, 若是则回退到任一存活节点
        if nodes[entry_idx].deleted {
            match nodes.iter().position(|n| !n.deleted) {
                Some(i) => entry_idx = i,
                None => return Vec::new(),
            }
        }

        let max_level = *self.max_level.read();
        let ef = self.config.ef_search.max(k);

        // 从顶层搜索到第 0 层
        let mut curr_entry = entry_idx;
        for l in (1..=max_level).rev() {
            let candidates = self.search_layer(query, curr_entry, l, 1, &nodes);
            if let Some(best) = candidates.first() {
                curr_entry = best.0;
            }
        }

        // 在第 0 层搜索 ef 个候选
        let candidates = self.search_layer(query, curr_entry, 0, ef, &nodes);
        let mut results: Vec<(u64, f32)> = candidates
            .into_iter()
            .filter(|(idx, _)| !nodes[*idx].deleted)
            .map(|(idx, dist)| (nodes[idx].id, dist))
            .collect();

        results.sort_by(|a, b| a.1.total_cmp(&b.1));
        results.truncate(k);
        results
    }

    /// 按 ID 删除节点 (tombstone)
    ///
    /// 仅标记 `deleted = true`, 保留向量与邻居以加速重建; 当墓碑比例超过
    /// [`REBUILD_THRESHOLD`] 时自动触发 [`Self::rebuild`] 物理回收。
    /// 返回是否真正删除了一个此前存活的节点。
    pub fn remove(&self, id: u64) -> bool {
        let mut nodes = self.nodes.write();
        let map = self.id_to_idx.write();

        let idx = match map
            .iter()
            .find(|(stored_id, _)| *stored_id == id)
            .map(|(_, i)| *i)
        {
            Some(i) => i,
            None => return false,
        };

        // 已是墓碑: 幂等删除返回 false
        if idx >= nodes.len() || nodes[idx].deleted {
            return false;
        }

        // Tombstone: 仅标记, 不物理删除
        nodes[idx].deleted = true;

        // 如果删除的是入口点, 重新选一个未删除节点
        if *self.entry_point.read() == Some(idx) {
            let new_entry = nodes.iter().position(|n| !n.deleted);
            *self.entry_point.write() = new_entry;
        }

        drop(nodes);
        drop(map);

        // 墓碑比例过高 -> 物理回收
        if self.deleted_ratio() > REBUILD_THRESHOLD {
            self.rebuild();
        }
        true
    }

    /// 当前墓碑 (deleted) 节点占比
    pub fn deleted_ratio(&self) -> f32 {
        let nodes = self.nodes.read();
        if nodes.is_empty() {
            return 0.0;
        }
        let deleted = nodes.iter().filter(|n| n.deleted).count();
        deleted as f32 / nodes.len() as f32
    }

    /// 物理回收墓碑节点: 压缩 nodes、重建 id->idx 映射与入口点。
    ///
    /// 由 `remove` 在墓碑比例超过阈值时自动调用, 也可手动调用。
    pub fn rebuild(&self) {
        let mut nodes = self.nodes.write();
        let mut map = self.id_to_idx.write();

        // old_idx -> new_idx
        let mut remap: Vec<Option<usize>> = vec![None; nodes.len()];
        let mut compact: Vec<HnswNode> = Vec::with_capacity(nodes.len());
        for (old_idx, node) in nodes.iter().enumerate() {
            if !node.deleted {
                remap[old_idx] = Some(compact.len());
                compact.push(node.clone());
            }
        }

        // 重映射邻居索引 (剔除指向墓碑的边)
        for node in &mut compact {
            for l in 0..node.neighbors.len() {
                node.neighbors[l]
                    .retain(|&nid| (nid as usize) < remap.len() && remap[nid as usize].is_some());
                for nid in &mut node.neighbors[l] {
                    *nid = remap[*nid as usize].unwrap() as u64;
                }
            }
        }

        // 重建 id->idx 映射
        let new_map: Vec<(u64, usize)> = map
            .iter()
            .filter_map(|(id, old_idx)| remap.get(*old_idx).copied().flatten().map(|ni| (*id, ni)))
            .collect();

        // 修复入口点与层级
        let old_entry = *self.entry_point.read();
        let new_entry = old_entry
            .and_then(|e| remap.get(e).copied().flatten())
            .or_else(|| compact.first().map(|_| 0));
        let new_max = compact
            .iter()
            .map(|n| n.neighbors.len().saturating_sub(1))
            .max()
            .unwrap_or(0);

        *nodes = compact;
        *map = new_map;
        *self.entry_point.write() = new_entry;
        *self.max_level.write() = new_max;
    }

    /// 裁剪节点在某层的连接数
    fn shrink_connections(node: &mut HnswNode, layer: usize, max_conn: usize) {
        if node.neighbors[layer].len() <= max_conn {
            return;
        }
        // 简单策略: 保留最近的 max_conn 个
        // 实际上应该在向量空间中选择最 diverse 的连接
        node.neighbors[layer].truncate(max_conn);
    }

    /// 获取索引中的记忆 ID 列表 (用于遍历, 跳过 tombstone)
    pub fn all_ids(&self) -> Vec<u64> {
        let nodes = self.nodes.read();
        let map = self.id_to_idx.read();
        map.iter()
            .filter(|(_, idx)| *idx < nodes.len() && !nodes[*idx].deleted)
            .map(|(id, _)| *id)
            .collect()
    }
}

/// 堆中的候选项
#[derive(Debug, Clone)]
struct NeighborEntry {
    idx: usize,
    dist: f32,
    /// true = 在 candidates 堆中 (距离远优先弹出), false = 在 result 堆中
    is_query: bool,
}

impl Eq for NeighborEntry {}

impl PartialEq for NeighborEntry {
    fn eq(&self, other: &Self) -> bool {
        self.dist == other.dist
    }
}

impl PartialOrd for NeighborEntry {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

/// 最大堆: 距离远的优先 (用于 result), 或距离近的优先 (用于 candidates via is_query)
impl Ord for NeighborEntry {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        match (self.is_query, other.is_query) {
            // candidates 堆: 近距离优先弹出 (小顶堆)
            (true, true) => other.dist.total_cmp(&self.dist),
            // result 堆: 远距离优先弹出 (大顶堆)
            (false, false) => self.dist.total_cmp(&other.dist),
            (true, false) => std::cmp::Ordering::Less,
            (false, true) => std::cmp::Ordering::Greater,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_index(dim: usize) -> HnswIndex {
        HnswIndex::new(HnswConfig {
            m: 8,
            m_max: 16,
            ef_construction: 50,
            ef_search: 50,
            m_l: 1.0 / (8.0_f64).ln(),
            dimension: dim,
            distance: DistanceMetric::EuclideanSquared,
        })
    }

    fn random_vector(dim: usize) -> Vec<f32> {
        use rand::Rng;
        let mut rng = rand::thread_rng();
        (0..dim).map(|_| rng.gen::<f32>()).collect()
    }

    #[test]
    fn test_empty_index() {
        let idx = make_index(4);
        assert!(idx.is_empty());
        let results = idx.search(&vec![0.0; 4], 5);
        assert!(results.is_empty());
    }

    #[test]
    fn test_insert_one() {
        let idx = make_index(4);
        let v = vec![1.0, 2.0, 3.0, 4.0];
        idx.insert(42, v.clone());
        assert_eq!(idx.len(), 1);
        let results = idx.search(&v, 5);
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].0, 42);
        assert!(results[0].1 < 1e-6);
    }

    #[test]
    fn test_insert_multiple() {
        let idx = make_index(8);
        let vectors: Vec<Vec<f32>> = (0..50)
            .map(|i| {
                let mut v = vec![0.0; 8];
                v[0] = i as f32;
                v
            })
            .collect();

        for (i, v) in vectors.iter().enumerate() {
            idx.insert(i as u64, v.clone());
        }
        assert_eq!(idx.len(), 50);
    }

    #[test]
    fn test_nearest_neighbor() {
        let idx = make_index(4);

        // 插入 3 个向量: [0,0,0,0], [1,1,1,1], [10,10,10,10]
        idx.insert(1, vec![0.0, 0.0, 0.0, 0.0]);
        idx.insert(2, vec![1.0, 1.0, 1.0, 1.0]);
        idx.insert(3, vec![10.0, 10.0, 10.0, 10.0]);

        // 查询接近 [0.1, 0.1, 0.1, 0.1] → 期望 ID=1
        let query = vec![0.1, 0.1, 0.1, 0.1];
        let results = idx.search(&query, 3);
        assert_eq!(results[0].0, 1);
        // ID=2 应该是第二近
        assert_eq!(results[1].0, 2);
    }

    #[test]
    fn test_search_top_k() {
        let idx = make_index(8);
        for i in 0..100 {
            let mut v = vec![0.0; 8];
            v[0] = i as f32;
            idx.insert(i as u64, v);
        }
        let query = vec![0.0; 8];
        let results = idx.search(&query, 5);
        assert_eq!(results.len(), 5);
        // 最接近 [0,0,...] 的是 ID=0
        assert_eq!(results[0].0, 0);
    }

    #[test]
    fn test_remove() {
        let idx = make_index(4);
        idx.insert(1, vec![1.0; 4]);
        idx.insert(2, vec![2.0; 4]);
        assert_eq!(idx.len(), 2);
        idx.remove(1);
        let results = idx.search(&vec![1.0; 4], 5);
        // 应该还能找到 ID=2
        assert!(!results.is_empty());
        assert_eq!(results[0].0, 2);
    }

    #[test]
    fn test_cosine_distance() {
        let a = vec![1.0, 0.0, 0.0, 0.0];
        let b = vec![1.0, 0.0, 0.0, 0.0];
        let d = HnswIndex::cosine_distance(&a, &b);
        assert!((d - 0.0).abs() < 1e-6);

        let c = vec![-1.0, 0.0, 0.0, 0.0];
        let d = HnswIndex::cosine_distance(&a, &c);
        assert!((d - 2.0).abs() < 1e-6);
    }

    #[test]
    fn test_all_ids() {
        let idx = make_index(4);
        idx.insert(10, vec![1.0; 4]);
        idx.insert(20, vec![2.0; 4]);
        idx.insert(30, vec![3.0; 4]);
        let ids = idx.all_ids();
        assert_eq!(ids.len(), 3);
        assert!(ids.contains(&10));
        assert!(ids.contains(&20));
        assert!(ids.contains(&30));
    }

    #[test]
    fn test_large_dimension() {
        let dim = 128;
        let idx = HnswIndex::new(HnswConfig {
            dimension: dim,
            ..Default::default()
        });
        let v = random_vector(dim);
        idx.insert(1, v.clone());
        let results = idx.search(&v, 1);
        assert!(!results.is_empty());
        assert_eq!(results[0].0, 1);
    }

    #[test]
    fn test_remove_tombstone_updates_len() {
        let idx = make_index(4);
        for i in 0..5u64 {
            idx.insert(i, vec![i as f32; 4]);
        }
        assert_eq!(idx.len(), 5);
        assert!(idx.remove(2));
        // tombstone: len 立即反映删除
        assert_eq!(idx.len(), 4);
        assert!(!idx.all_ids().contains(&2));
        // 重复删除返回 false (幂等)
        assert!(!idx.remove(2));
    }

    #[test]
    fn test_remove_triggers_rebuild() {
        let idx = make_index(4);
        for i in 0..10u64 {
            idx.insert(i, vec![i as f32; 4]);
        }
        // 删除 4 个: 4/10 = 0.4 > 0.3 -> 触发 rebuild, 墓碑被压缩
        for i in 0..4u64 {
            idx.remove(i);
        }
        assert_eq!(idx.len(), 6);
        assert_eq!(idx.deleted_ratio(), 0.0);
        // 搜索仍正常
        let results = idx.search(&vec![9.0; 4], 3);
        assert!(!results.is_empty());
        assert_eq!(results[0].0, 9);
    }

    #[test]
    fn test_search_skips_deleted() {
        let idx = make_index(4);
        idx.insert(1, vec![1.0, 0.0, 0.0, 0.0]);
        idx.insert(2, vec![1.1, 0.0, 0.0, 0.0]);
        idx.insert(3, vec![9.0, 9.0, 9.0, 9.0]);
        idx.remove(1);
        let results = idx.search(&vec![1.0, 0.0, 0.0, 0.0], 5);
        // 被删节点不出现
        assert!(!results.iter().any(|(id, _)| *id == 1));
        assert_eq!(results[0].0, 2);
    }

    #[test]
    fn test_reinsert_resurrects_tombstone() {
        let idx = make_index(4);
        idx.insert(7, vec![1.0, 0.0, 0.0, 0.0]);
        idx.remove(7);
        assert_eq!(idx.len(), 0);
        // 重新插入同 id: 复活
        idx.insert(7, vec![1.0, 0.0, 0.0, 0.0]);
        assert_eq!(idx.len(), 1);
        let results = idx.search(&vec![1.0, 0.0, 0.0, 0.0], 5);
        assert_eq!(results[0].0, 7);
    }
}
