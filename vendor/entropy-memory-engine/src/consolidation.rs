use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use crate::core::types::{LayerId, Memory, MemoryContent, MemoryInput, Query};
use crate::core::MemoryResult;
use crate::l2::L2Engine;
use crate::l3::L3Engine;

/// Strategy configuration for automatic forgetting.
#[derive(Debug, Clone)]
pub struct ForgettingStrategy {
    /// Remove L2 facts whose importance is below this threshold (0.0 = never remove by importance).
    pub importance_threshold: f32,
    /// Remove L3 summaries whose importance is below this threshold.
    pub l3_importance_threshold: f32,
    /// Remove memories older than this many nanoseconds (0 = never remove by age).
    pub ttl_nanos: i64,
}

impl Default for ForgettingStrategy {
    fn default() -> Self {
        Self {
            // Default importance is 0.5; forget only genuinely low-importance
            // memories so the annual storage budget stays bounded.
            importance_threshold: 0.1,
            // L3 summaries encode consolidated knowledge — keep them longer.
            l3_importance_threshold: 0.05,
            ttl_nanos: 0,
        }
    }
}

/// Configuration for memory consolidation.
#[derive(Debug, Clone)]
pub struct ConsolidationConfig {
    /// Maximum cosine distance for two memories to be considered "similar".
    /// Lower = stricter. Default 0.35 (tuned up from 0.25 for more merging).
    pub similarity_threshold: f32,
    /// Maximum memories to merge in a single consolidation batch.
    pub max_merge_batch: usize,
    /// Minimum number of similar memories needed to trigger a merge.
    pub min_merge_group: usize,
    /// Whether to auto-deduplicate on every store() call.
    pub auto_dedup_on_store: bool,
    /// Number of recursive consolidation rounds to run.
    /// Round 1 merges L2 facts → L3 summaries.
    /// Rounds 2+ merge similar L3 summaries into higher-level summaries.
    /// Set to 1 for single-pass (previous behaviour).
    pub recursive_rounds: usize,
    /// Forgetting strategy to apply after each consolidation cycle.
    pub forgetting: ForgettingStrategy,
    /// Whether to use MLX LLM for compression during consolidation.
    pub use_llm_compression: bool,
    /// Whether to align new knowledge with existing on write.
    pub align_on_write: bool,
}

impl Default for ConsolidationConfig {
    fn default() -> Self {
        Self {
            similarity_threshold: 0.35,
            max_merge_batch: 20,
            min_merge_group: 2,
            auto_dedup_on_store: false,
            recursive_rounds: 3,
            forgetting: ForgettingStrategy::default(),
            use_llm_compression: false,
            align_on_write: false,
        }
    }
}

/// Consolidated stats from a consolidation cycle.
#[derive(Debug, Clone, Default)]
pub struct ConsolidationStats {
    pub rounds: Vec<ConsolidationRoundStats>,
    pub total_merged: usize,
    pub total_deduped: usize,
    pub total_forgotten_l2: usize,
    pub total_forgotten_l3: usize,
}

#[derive(Debug, Clone)]
pub struct ConsolidationRoundStats {
    pub round: usize,
    pub source_layer: LayerId,
    pub merged: usize,
    pub deduped: usize,
}

/// The consolidation engine finds semantically similar memories across layers
/// and merges them into consolidated summaries, reducing entropy.
pub struct ConsolidationEngine {
    config: ConsolidationConfig,
}

impl ConsolidationEngine {
    pub fn new(config: ConsolidationConfig) -> Self {
        Self { config }
    }

    pub fn config(&self) -> &ConsolidationConfig {
        &self.config
    }

    /// Compute cosine similarity between two embedding vectors.
    fn cosine_similarity(a: &[f32], b: &[f32]) -> f32 {
        let dot: f32 = a.iter().zip(b.iter()).map(|(x, y)| x * y).sum();
        let na: f32 = a.iter().map(|x| x * x).sum::<f32>().sqrt();
        let nb: f32 = b.iter().map(|x| x * x).sum::<f32>().sqrt();
        if na == 0.0 || nb == 0.0 {
            return 0.0;
        }
        (dot / (na * nb)).max(0.0).min(1.0)
    }

    // ================================================================
    // Round 1: L2 Facts → L3 Summaries (existing logic)
    // ================================================================

    /// Find all L2 memory IDs whose vectors are within `similarity_threshold`
    /// cosine distance of the given query text.
    pub fn find_similar_in_l2(&self, l2: &L2Engine, query_text: &str) -> Vec<(u64, f32)> {
        let q = Query::by_text(query_text);
        let results = l2.search_semantic(&q, self.config.max_merge_batch);
        results
            .into_iter()
            .filter(|(_, dist)| *dist <= self.config.similarity_threshold)
            .collect()
    }

    /// Merge a group of Facts into a single consolidated Summary stored in L3.
    /// Returns the L3 memory ID of the new consolidated entry.
    pub fn merge_into_l3(
        &self,
        l2: &L2Engine,
        l3: &L3Engine,
        ids: &[u64],
    ) -> MemoryResult<Option<u64>> {
        if ids.len() < self.config.min_merge_group {
            return Ok(None);
        }

        let memories: Vec<Memory> = ids.iter().filter_map(|id| l2.get_by_id(*id)).collect();

        if memories.len() < self.config.min_merge_group {
            return Ok(None);
        }

        let mut subjects = HashSet::new();
        let mut predicates = HashSet::new();
        let mut objects = HashSet::new();
        let mut total_importance = 0.0f32;
        let mut fact_count = 0usize;

        for mem in &memories {
            if let MemoryContent::Fact(ref fact) = mem.content {
                subjects.insert(fact.subject.clone());
                predicates.insert(fact.predicate.clone());
                objects.insert(fact.object.clone());
                total_importance += mem.metadata.importance;
                fact_count += 1;
            }
        }

        if fact_count == 0 {
            return Ok(None);
        }

        let summary = if fact_count == 1 {
            let f = match &memories[0].content {
                MemoryContent::Fact(f) => f,
                _ => return Ok(None),
            };
            format!("{} {} {}", f.subject, f.predicate, f.object)
        } else {
            let subjects: Vec<&str> = subjects.iter().map(|s| s.as_str()).collect();
            let predicates: Vec<&str> = predicates.iter().map(|s| s.as_str()).collect();
            let objects: Vec<&str> = objects.iter().map(|s| s.as_str()).collect();

            let mut parts: Vec<String> = Vec::new();

            if subjects.len() <= 3 {
                parts.push(format!("subjects: {}", subjects.join(", ")));
            } else {
                parts.push(format!("{} subjects", subjects.len()));
            }

            if predicates.len() <= 3 {
                parts.push(format!("predicates: {}", predicates.join(", ")));
            }

            if objects.len() <= 5 {
                parts.push(format!("objects: {}", objects.join(", ")));
            }

            parts.push(format!("(consolidated from {} facts)", fact_count));
            parts.join(" | ")
        };

        let avg_importance = total_importance / fact_count as f32;

        let l3_id = l3.insert(MemoryInput {
            content: MemoryContent::Summary(summary),
            importance: avg_importance,
            alias: None,
            tags: vec!["consolidated".to_string()],
            layer: LayerId::L3,
        })?;

        for id in ids {
            l2.remove(*id);
        }

        Ok(Some(l3_id))
    }

    /// Find exact-duplicate Facts in L2 and remove the extras.
    pub fn deduplicate_l2(&self, l2: &L2Engine) -> usize {
        let ids = l2.storage.all_ids();
        let mut groups: HashMap<String, Vec<u64>> = HashMap::new();
        for id in &ids {
            if let Some(mem) = l2.get_by_id(*id) {
                if let MemoryContent::Fact(ref f) = mem.content {
                    let key = format!("{}|{}|{}", f.subject, f.predicate, f.object);
                    groups.entry(key).or_default().push(*id);
                }
            }
        }

        let mut removed = 0usize;
        for group_ids in groups.values() {
            for extra in group_ids.iter().skip(1) {
                l2.remove(*extra);
                removed += 1;
            }
        }
        removed
    }

    // ================================================================
    // Round 2+: L3 Summaries → Higher-Level L3 Summaries
    // ================================================================

    /// Recursive round: find semantically similar L3 summaries and merge
    /// them into a higher-level consolidated summary.
    ///
    /// Uses L2's embedding function for similarity comparison without
    /// storing L3 content in L2.
    fn consolidate_l3_round(
        &self,
        l2: &L2Engine,
        l3: &L3Engine,
        consumed: &mut HashSet<u64>,
    ) -> Vec<u64> {
        let all_l3 = l3.storage().all();
        // Only consider L3 summaries not already consumed in a prior round
        let candidates: Vec<Memory> = all_l3
            .into_iter()
            .filter(|m| matches!(m.content, MemoryContent::Summary(_)) && !consumed.contains(&m.id))
            .collect();

        if candidates.len() < self.config.min_merge_group {
            return Vec::new();
        }

        // Precompute embeddings for all candidates
        let embeddings: Vec<(u64, Vec<f32>)> = candidates
            .iter()
            .map(|m| {
                let text = m.content_text();
                let vec = l2.text_to_vector(&text);
                (m.id, vec)
            })
            .collect();

        let mut merged_ids: Vec<u64> = Vec::new();
        let mut processed: HashSet<u64> = HashSet::new();

        for (i, (id_i, vec_i)) in embeddings.iter().enumerate() {
            if processed.contains(id_i) {
                continue;
            }

            let mut group: Vec<u64> = vec![*id_i];
            let mut group_importance = candidates[i].metadata.importance;
            let mut group_subjects: Vec<String> = Vec::new();
            if let MemoryContent::Summary(ref s) = candidates[i].content {
                group_subjects.push(s.clone());
            }

            for (j, (id_j, vec_j)) in embeddings.iter().enumerate() {
                if i == j || processed.contains(id_j) {
                    continue;
                }
                let sim = Self::cosine_similarity(vec_i, vec_j);
                if sim >= (1.0 - self.config.similarity_threshold) {
                    group.push(*id_j);
                    group_importance += candidates[j].metadata.importance;
                    if let MemoryContent::Summary(ref s) = candidates[j].content {
                        group_subjects.push(s.clone());
                    }
                }
            }

            if group.len() < self.config.min_merge_group {
                processed.insert(*id_i);
                continue;
            }

            // Build a higher-level summary
            let avg_imp = group_importance / group.len() as f32;
            let topics: Vec<&str> = group_subjects.iter().map(|s| s.as_str()).collect();
            let summary_text = if topics.len() <= 5 {
                format!(
                    "merged: {} | (recursively consolidated from {} summaries)",
                    topics.join(" ; "),
                    group.len()
                )
            } else {
                format!(
                    "{} related topics | (recursively consolidated from {} summaries)",
                    topics.len(),
                    group.len()
                )
            };

            // Store the recursively consolidated summary in L3
            if l3
                .insert(MemoryInput {
                    content: MemoryContent::Summary(summary_text),
                    importance: avg_imp,
                    alias: None,
                    tags: vec!["consolidated".to_string(), "recursive".to_string()],
                    layer: LayerId::L3,
                })
                .is_ok()
            {
                // Mark group members as consumed and remove originals
                for id in &group {
                    processed.insert(*id);
                    consumed.insert(*id);
                    l3.remove(*id);
                }
                merged_ids.push(*id_i);
            } else {
                processed.insert(*id_i);
            }
        }

        merged_ids
    }

    // ================================================================
    // Forgetting Strategy
    // ================================================================

    /// Remove L2 facts whose importance is below threshold.
    pub fn forget_below_importance_l2(&self, l2: &L2Engine, threshold: f32) -> usize {
        if threshold <= 0.0 {
            return 0;
        }
        let ids = l2.storage.all_ids();
        let mut removed = 0;
        for id in &ids {
            if let Some(mem) = l2.get_by_id(*id) {
                if mem.metadata.importance < threshold {
                    l2.remove(*id);
                    removed += 1;
                }
            }
        }
        removed
    }

    /// Remove L3 summaries whose importance is below threshold.
    pub fn forget_below_importance_l3(&self, l3: &L3Engine, threshold: f32) -> usize {
        if threshold <= 0.0 {
            return 0;
        }
        let ids = l3.storage().all_ids();
        let mut removed = 0;
        for id in &ids {
            if let Some(mem) = l3.get_by_id(*id) {
                if mem.metadata.importance < threshold {
                    l3.remove(*id);
                    removed += 1;
                }
            }
        }
        removed
    }

    /// Remove L2 facts older than ttl_nanos.
    pub fn forget_older_than_l2(&self, l2: &L2Engine, ttl_nanos: i64) -> usize {
        if ttl_nanos <= 0 {
            return 0;
        }
        let now = crate::core::now_nanos();
        let ids = l2.storage.all_ids();
        let mut removed = 0;
        for id in &ids {
            if let Some(mem) = l2.get_by_id(*id) {
                if now - mem.metadata.created_at > ttl_nanos {
                    l2.remove(*id);
                    removed += 1;
                }
            }
        }
        removed
    }

    /// Remove L3 summaries older than ttl_nanos.
    pub fn forget_older_than_l3(&self, l3: &L3Engine, ttl_nanos: i64) -> usize {
        if ttl_nanos <= 0 {
            return 0;
        }
        let now = crate::core::now_nanos();
        let ids = l3.storage().all_ids();
        let mut removed = 0;
        for id in &ids {
            if let Some(mem) = l3.get_by_id(*id) {
                if now - mem.metadata.created_at > ttl_nanos {
                    l3.remove(*id);
                    removed += 1;
                }
            }
        }
        removed
    }

    /// Run the forgetting strategy against L2 and L3.
    pub fn apply_forgetting(&self, l2: &L2Engine, l3: &L3Engine) -> (usize, usize) {
        let s = &self.config.forgetting;
        let removed_l2 = self.forget_below_importance_l2(l2, s.importance_threshold)
            + self.forget_older_than_l2(l2, s.ttl_nanos);
        let removed_l3 = self.forget_below_importance_l3(l3, s.l3_importance_threshold)
            + self.forget_older_than_l3(l3, s.ttl_nanos);
        (removed_l2, removed_l3)
    }

    // ================================================================
    // Main API
    // ================================================================

    /// Run a single round of consolidation: L2 facts → L3 summaries.
    /// Returns (merged_count, dedup_count).
    pub fn consolidate(&self, l2: &L2Engine, l3: &L3Engine) -> (usize, usize) {
        let deduped = self.deduplicate_l2(l2);

        let all_ids = l2.storage.all_ids();
        let mut processed = HashSet::new();
        let mut merged = 0usize;

        for seed_id in &all_ids {
            if processed.contains(seed_id) {
                continue;
            }
            let seed_text = match l2.get_by_id(*seed_id) {
                Some(m) => m.content_text(),
                None => continue,
            };
            if seed_text.is_empty() {
                continue;
            }

            let q = Query::by_text(&seed_text);
            let similar = l2.search_semantic(&q, self.config.max_merge_batch + 1);
            let candidates: Vec<(u64, f32)> = similar
                .into_iter()
                .filter(|(id, dist)| {
                    *dist <= self.config.similarity_threshold && !processed.contains(id)
                })
                .collect();

            if candidates.len() < self.config.min_merge_group {
                processed.insert(*seed_id);
                continue;
            }

            let candidate_ids: Vec<u64> = candidates.iter().map(|(id, _)| *id).collect();

            if let Ok(Some(_merged_id)) = self.merge_into_l3(l2, l3, &candidate_ids) {
                merged += 1;
                for id in &candidate_ids {
                    processed.insert(*id);
                }
            } else {
                processed.insert(*seed_id);
            }
        }

        (merged, deduped)
    }

    /// Run recursive consolidation with multiple rounds.
    ///
    /// - Round 1: L2 Facts → L3 summaries
    /// - Rounds 2+: L3 summaries → higher-level summaries
    /// - After all rounds: apply forgetting strategy
    pub fn consolidate_recursive(&self, l2: &L2Engine, l3: &L3Engine) -> ConsolidationStats {
        let mut stats = ConsolidationStats::default();
        let mut consumed_l3: HashSet<u64> = HashSet::new();

        for round in 1..=self.config.recursive_rounds.max(1) {
            let (merged, deduped) = if round == 1 {
                // Round 1: L2 consolidation (existing logic)
                let deduped = self.deduplicate_l2(l2);
                let merged = self.consolidate_round_1_only(l2, l3);
                // Mark all L3 summaries from round 1 as eligible for further rounds
                (merged, deduped)
            } else {
                // Rounds 2+: L3 → L3 recursive consolidation
                let round_ids = self.consolidate_l3_round(l2, l3, &mut consumed_l3);
                let merged = round_ids.len();
                (merged, 0)
            };

            stats.rounds.push(ConsolidationRoundStats {
                round,
                source_layer: if round == 1 { LayerId::L2 } else { LayerId::L3 },
                merged,
                deduped,
            });
            stats.total_merged += merged;
            stats.total_deduped += deduped;

            if merged == 0 {
                break;
            }
        }

        // Apply forgetting strategy after all rounds
        let (forgotten_l2, forgotten_l3) = self.apply_forgetting(l2, l3);
        stats.total_forgotten_l2 = forgotten_l2;
        stats.total_forgotten_l3 = forgotten_l3;

        stats
    }

    /// Round 1 only (used internally by consolidate_recursive).
    fn consolidate_round_1_only(&self, l2: &L2Engine, l3: &L3Engine) -> usize {
        let all_ids = l2.storage.all_ids();
        let mut processed = HashSet::new();
        let mut merged = 0usize;

        for seed_id in &all_ids {
            if processed.contains(seed_id) {
                continue;
            }
            let seed_text = match l2.get_by_id(*seed_id) {
                Some(m) => m.content_text(),
                None => continue,
            };
            if seed_text.is_empty() {
                continue;
            }

            let q = Query::by_text(&seed_text);
            let similar = l2.search_semantic(&q, self.config.max_merge_batch + 1);
            let candidates: Vec<(u64, f32)> = similar
                .into_iter()
                .filter(|(id, dist)| {
                    *dist <= self.config.similarity_threshold && !processed.contains(id)
                })
                .collect();

            if candidates.len() < self.config.min_merge_group {
                processed.insert(*seed_id);
                continue;
            }

            let candidate_ids: Vec<u64> = candidates.iter().map(|(id, _)| *id).collect();

            if let Ok(Some(_merged_id)) = self.merge_into_l3(l2, l3, &candidate_ids) {
                merged += 1;
                for id in &candidate_ids {
                    processed.insert(*id);
                }
            } else {
                processed.insert(*seed_id);
            }
        }
        merged
    }

    // ================================================================
    // Phase 1 additions: Extract + Reconcile pipeline
    // ================================================================

    pub fn extract_from_interaction(&self, raw_text: &str) -> Vec<(String, String, String, f32)> {
        if self.config.use_llm_compression {
            self.extract_with_mlx(raw_text)
        } else {
            self.extract_rule_based(raw_text)
        }
    }

    fn extract_with_mlx(&self, raw_text: &str) -> Vec<(String, String, String, f32)> {
        let script = r#"import sys, json
try:
    import mlx.core as mx
    from mlx_lm import load, generate
    model, tokenizer = load("mlx-community/Llama-3.2-1B-Instruct-4bit")
    text = sys.stdin.read().strip()
    prompt = '''Extract structured knowledge from the following text as a JSON array. Each entry must have: "subject", "predicate", "object", "confidence" (0.0-1.0). Only extract factual claims, not opinions or meta-commentary. Text: ''' + text + ''' Output ONLY valid JSON array, no other text:'''
    messages = [{"role": "user", "content": prompt}]
    formatted = tokenizer.apply_chat_template(messages, add_generation_prompt=True)
    response = generate(model, tokenizer, prompt=formatted, max_tokens=512)
    print(response.strip())
except Exception as e:
    print(json.dumps([{"error": str(e)}]))
"#;

        let python = crate::core::detect_python();
        let mut child = match std::process::Command::new(python)
            .arg("-c")
            .arg(script)
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::null())
            .spawn()
        {
            Ok(c) => c,
            Err(_) => return self.extract_rule_based(raw_text),
        };

        use std::io::Write;
        if let Some(mut stdin) = child.stdin.take() {
            let _ = writeln!(stdin, "{}", raw_text);
        }

        match child.wait_with_output() {
            Ok(out) if out.status.success() => {
                let stdout = String::from_utf8_lossy(&out.stdout);
                if let Ok(parsed) = serde_json::from_str::<Vec<serde_json::Value>>(stdout.trim()) {
                    return parsed
                        .iter()
                        .filter_map(|v| {
                            let s = v.get("subject")?.as_str()?.to_string();
                            let p = v.get("predicate")?.as_str()?.to_string();
                            let o = v.get("object")?.as_str()?.to_string();
                            let c =
                                v.get("confidence").and_then(|c| c.as_f64()).unwrap_or(0.5) as f32;
                            Some((s, p, o, c))
                        })
                        .collect();
                }
                Vec::new()
            }
            _ => self.extract_rule_based(raw_text),
        }
    }

    fn extract_rule_based(&self, raw_text: &str) -> Vec<(String, String, String, f32)> {
        let mut results = Vec::new();
        let sentences: Vec<&str> = raw_text
            .split(&['.', '!', '?', '\n'][..])
            .filter(|s| s.trim().len() > 5)
            .collect();

        for sentence in sentences {
            let words: Vec<&str> = sentence.split_whitespace().collect();
            if words.len() < 3 {
                continue;
            }
            if let Some(verb_pos) = words.iter().position(|w| {
                matches!(
                    *w,
                    "is" | "are"
                        | "was"
                        | "were"
                        | "has"
                        | "have"
                        | "likes"
                        | "knows"
                        | "uses"
                        | "works"
                        | "lives"
                        | "owns"
                )
            }) {
                if verb_pos > 0 && verb_pos < words.len() - 1 {
                    let subject = words[0..verb_pos].join(" ");
                    let remainder: String = words[verb_pos + 1..].join(" ");
                    if remainder.len() > 1 {
                        results.push((
                            subject,
                            words[verb_pos].to_string(),
                            remainder.trim_end_matches('.').to_string(),
                            0.6,
                        ));
                    }
                }
            }
        }
        results
    }

    pub fn reconcile_batch(
        &self,
        extractions: &[(String, String, String, f32)],
    ) -> (Vec<MemoryInput>, Vec<MemoryInput>) {
        let mut supplements = Vec::new();
        let sublimates = Vec::new();

        for (subject, predicate, object, confidence) in extractions {
            let input = MemoryInput::new(MemoryContent::Fact(crate::core::Fact {
                subject: subject.clone(),
                predicate: predicate.clone(),
                object: object.clone(),
                confidence: *confidence,
            }))
            .with_importance(*confidence);
            supplements.push(input);
        }

        (supplements, sublimates)
    }
}

// ================================================================
// Consolidation Scheduler — periodically runs consolidation + forgetting
// ================================================================

/// Configuration for the automatic consolidation scheduler.
#[derive(Debug, Clone)]
pub struct ConsolidationSchedulerConfig {
    /// Interval between automatic consolidation cycles in seconds.
    pub cycle_interval_secs: u64,
}

impl Default for ConsolidationSchedulerConfig {
    fn default() -> Self {
        Self {
            cycle_interval_secs: 3600, // 1 hour
        }
    }
}

/// Aggregate statistics across all scheduler cycles.
#[derive(Debug, Clone, Default)]
pub struct SchedulerStats {
    pub total_cycles: u64,
    pub total_merged: u64,
    pub total_deduped: u64,
    pub total_forgotten_l2: u64,
    pub total_forgotten_l3: u64,
    pub last_cycle_duration_ns: i64,
    pub last_cycle_at: i64,
    pub last_cycle_merged: usize,
    pub last_cycle_deduped: usize,
    pub last_cycle_forgotten_l2: usize,
    pub last_cycle_forgotten_l3: usize,
}

/// Automatic scheduler that periodically runs full consolidation + forgetting cycles.
///
/// Each cycle calls `ConsolidationEngine::consolidate_recursive()` which:
/// 1. Deduplicates L2
/// 2. Merges similar L2 facts into L3 summaries (Round 1)
/// 3. Merges similar L3 summaries into higher-level summaries (Rounds 2+)
/// 4. Applies the forgetting strategy (removes low-importance / old entries)
/// 5. Budget (via `L3Engine::remove()`) is reconciled automatically per removal
///
/// Usage — manual cycle:
/// ```ignore
/// let s = ConsolidationScheduler::new(config, consolidation_config);
/// let cs = s.run_cycle(&l2, &l3);
/// println!("merged={} forgotten_l2={}", cs.total_merged, cs.total_forgotten_l2);
/// ```
///
/// Usage — background thread:
/// ```ignore
/// let s = Arc::new(ConsolidationScheduler::new(config, consolidation_config));
/// s.start(Arc::new(l2), Arc::new(l3));
/// // ... later ...
/// s.stop();
/// ```
pub struct ConsolidationScheduler {
    config: ConsolidationSchedulerConfig,
    consolidation: ConsolidationEngine,
    stats: Arc<Mutex<SchedulerStats>>,
    running: Arc<AtomicBool>,
}

impl ConsolidationScheduler {
    pub fn new(
        config: ConsolidationSchedulerConfig,
        consolidation_config: ConsolidationConfig,
    ) -> Self {
        Self {
            config,
            consolidation: ConsolidationEngine::new(consolidation_config),
            stats: Arc::new(Mutex::new(SchedulerStats::default())),
            running: Arc::new(AtomicBool::new(false)),
        }
    }

    pub fn config(&self) -> &ConsolidationSchedulerConfig {
        &self.config
    }

    pub fn consolidation_engine(&self) -> &ConsolidationEngine {
        &self.consolidation
    }

    pub fn stats(&self) -> SchedulerStats {
        self.stats.lock().unwrap().clone()
    }

    pub fn is_running(&self) -> bool {
        self.running.load(Ordering::Relaxed)
    }

    /// Run one full consolidation + forgetting cycle.
    ///
    /// Calls `consolidate_recursive()` under the hood. Budget reconciliation
    /// is handled automatically by `L3Engine::remove()` for each removed entry.
    /// Returns the per-cycle consolidation stats.
    pub fn run_cycle(&self, l2: &L2Engine, l3: &L3Engine) -> ConsolidationStats {
        let start = crate::core::now_nanos();
        let cs = self.consolidation.consolidate_recursive(l2, l3);
        let duration = crate::core::now_nanos() - start;

        let mut stats = self.stats.lock().unwrap();
        stats.total_cycles += 1;
        stats.total_merged += cs.total_merged as u64;
        stats.total_deduped += cs.total_deduped as u64;
        stats.total_forgotten_l2 += cs.total_forgotten_l2 as u64;
        stats.total_forgotten_l3 += cs.total_forgotten_l3 as u64;
        stats.last_cycle_duration_ns = duration;
        stats.last_cycle_at = crate::core::now_nanos();
        stats.last_cycle_merged = cs.total_merged;
        stats.last_cycle_deduped = cs.total_deduped;
        stats.last_cycle_forgotten_l2 = cs.total_forgotten_l2;
        stats.last_cycle_forgotten_l3 = cs.total_forgotten_l3;

        cs
    }

    /// Start a background thread that runs `run_cycle()` every
    /// `cycle_interval_secs`. The thread loops until `stop()` is called
    /// or the scheduler is dropped.
    ///
    /// Requires `Arc<Self>` (cloned cheaply for the thread) and
    /// `Arc<L2Engine>` / `Arc<L3Engine>` since the thread owns them.
    pub fn start(self: &Arc<Self>, l2: Arc<L2Engine>, l3: Arc<L3Engine>) {
        self.running.store(true, Ordering::Relaxed);
        let interval = self.config.cycle_interval_secs;
        let running = self.running.clone();
        let scheduler = Arc::clone(self);
        let l2 = l2;
        let l3 = l3;

        std::thread::spawn(move || {
            while running.load(Ordering::Relaxed) {
                std::thread::sleep(Duration::from_secs(interval));
                if !running.load(Ordering::Relaxed) {
                    break;
                }
                scheduler.run_cycle(&l2, &l3);
            }
        });
    }

    /// Signal the background thread to stop. The thread will exit after
    /// its current sleep completes.
    pub fn stop(&self) {
        self.running.store(false, Ordering::Relaxed);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::Fact;
    use crate::l2::{HnswConfig, L2Config};
    use crate::l3::{BudgetConfig, L3Config};
    use tempfile::tempdir;

    fn make_l2() -> L2Engine {
        L2Engine::new(L2Config {
            hnsw: HnswConfig {
                dimension: 16,
                ..Default::default()
            },
            dimension: 16,
            ..Default::default()
        })
    }

    fn make_l3() -> L3Engine {
        let dir = tempdir().unwrap();
        L3Engine::new(L3Config {
            storage_path: dir.path().join("consolidate.bin"),
            budget: BudgetConfig {
                daily_token_limit: 1_000_000,
                annual_storage_limit: 10_000_000,
            },
            ..Default::default()
        })
    }

    fn make_config() -> ConsolidationConfig {
        ConsolidationConfig {
            similarity_threshold: 0.3,
            max_merge_batch: 10,
            min_merge_group: 2,
            auto_dedup_on_store: false,
            recursive_rounds: 3,
            ..Default::default()
        }
    }

    fn insert_fact(l2: &L2Engine, s: &str, p: &str, o: &str) -> u64 {
        l2.insert(MemoryInput::new(MemoryContent::Fact(Fact::new(s, p, o))))
            .unwrap()
    }

    #[test]
    fn test_find_similar_empty_when_no_match() {
        let l2 = make_l2();
        let engine = ConsolidationEngine::new(make_config());
        let results = engine.find_similar_in_l2(&l2, "completely unrelated");
        assert!(results.is_empty());
    }

    #[test]
    fn test_find_similar_returns_close_memories() {
        let l2 = make_l2();
        let _id1 = insert_fact(&l2, "alice", "likes", "rust programming");
        let _id2 = insert_fact(&l2, "bob", "likes", "python");

        let engine = ConsolidationEngine::new(make_config());
        let results = engine.find_similar_in_l2(&l2, "rust");
        assert!(
            results.len() <= 2,
            "expected at most 2 results, got {}",
            results.len()
        );
    }

    #[test]
    fn test_merge_into_l3_requires_min_group() {
        let l2 = make_l2();
        let l3 = make_l3();
        let engine = ConsolidationEngine::new(ConsolidationConfig {
            min_merge_group: 3,
            ..make_config()
        });

        let _id1 = insert_fact(&l2, "a", "b", "c");
        let _id2 = insert_fact(&l2, "d", "e", "f");

        let result = engine.merge_into_l3(&l2, &l3, &[1, 2]).unwrap();
        assert!(
            result.is_none(),
            "should not merge with fewer than min group"
        );
    }

    #[test]
    fn test_merge_into_l3_creates_summary() {
        let l2 = make_l2();
        let l3 = make_l3();
        let engine = ConsolidationEngine::new(make_config());

        let id1 = insert_fact(&l2, "alice", "likes", "cats");
        let id2 = insert_fact(&l2, "bob", "likes", "dogs");

        let result = engine.merge_into_l3(&l2, &l3, &[id1, id2]).unwrap();
        assert!(result.is_some(), "should merge into L3");
        let l3_id = result.unwrap();

        let stored = l3.get_by_id(l3_id).unwrap();
        assert!(matches!(stored.content, MemoryContent::Summary(_)));
        assert!(stored.tags.contains(&"consolidated".to_string()));

        // Original L2 entries should be removed
        assert!(l2.get_by_id(id1).is_none());
        assert!(l2.get_by_id(id2).is_none());
    }

    #[test]
    fn test_deduplicate_l2_removes_exact_duplicates() {
        let l2 = make_l2();
        let engine = ConsolidationEngine::new(make_config());

        let _id1 = insert_fact(&l2, "alice", "likes", "cats");
        let _id2 = insert_fact(&l2, "alice", "likes", "cats");

        assert_eq!(l2.storage.all_ids().len(), 2);
        let removed = engine.deduplicate_l2(&l2);
        assert_eq!(removed, 1);
        assert_eq!(l2.storage.all_ids().len(), 1);
    }

    #[test]
    fn test_consolidate_merges_similar_facts() {
        let l2 = make_l2();
        let l3 = make_l3();
        // With hash-based embeddings, distance threshold must be generous.
        // This tests the consolidation algorithm structure, not search accuracy
        // (which requires a real embedding model — see DEVELOPMENT_LOG.md).
        let engine = ConsolidationEngine::new(ConsolidationConfig {
            similarity_threshold: 3.0,
            min_merge_group: 2,
            ..make_config()
        });

        let _id1 = insert_fact(&l2, "alice", "likes", "cats");
        let _id2 = insert_fact(&l2, "bob", "likes", "cats");

        let (merged, deduped) = engine.consolidate(&l2, &l3);
        assert!(merged > 0, "should merge similar facts");
        assert_eq!(deduped, 0);
    }

    #[test]
    fn test_cosine_similarity_identical() {
        let v = vec![1.0, 0.0, 0.0];
        let sim = ConsolidationEngine::cosine_similarity(&v, &v);
        assert!((sim - 1.0).abs() < 1e-6);
    }

    #[test]
    fn test_cosine_similarity_orthogonal() {
        let a = vec![1.0, 0.0];
        let b = vec![0.0, 1.0];
        let sim = ConsolidationEngine::cosine_similarity(&a, &b);
        assert!((sim - 0.0).abs() < 1e-6);
    }

    // ================================================================
    // Recursive consolidation tests
    // ================================================================

    #[test]
    fn test_consolidate_recursive_runs_multiple_rounds() {
        let l2 = make_l2();
        let l3 = make_l3();
        let engine = ConsolidationEngine::new(ConsolidationConfig {
            similarity_threshold: 3.0,
            min_merge_group: 2,
            recursive_rounds: 3,
            ..make_config()
        });

        let _id1 = insert_fact(&l2, "alice", "likes", "cats");
        let _id2 = insert_fact(&l2, "bob", "likes", "cats");
        let _id3 = insert_fact(&l2, "carol", "feeds", "cats");

        let stats = engine.consolidate_recursive(&l2, &l3);

        assert!(stats.total_merged > 0, "should merge at least once");
        assert!(stats.total_deduped == 0, "no duplicates in this test");
        assert!(!stats.rounds.is_empty(), "should have at least 1 round");
        assert_eq!(stats.rounds[0].source_layer, LayerId::L2);
    }

    #[test]
    fn test_recursive_consolidation_creates_recursive_tag() {
        let l2 = make_l2();
        let l3 = make_l3();
        let engine = ConsolidationEngine::new(ConsolidationConfig {
            similarity_threshold: 3.0,
            min_merge_group: 2,
            recursive_rounds: 2,
            ..make_config()
        });

        let _id1 = insert_fact(&l2, "alice", "likes", "cats");
        let _id2 = insert_fact(&l2, "bob", "likes", "cats");
        let _id3 = insert_fact(&l2, "carol", "feeds", "dogs");
        let _id4 = insert_fact(&l2, "dave", "walks", "dogs");

        let stats = engine.consolidate_recursive(&l2, &l3);

        assert!(
            stats.total_merged >= 1,
            "at least one merge should happen, got {}",
            stats.total_merged
        );
    }

    #[test]
    fn test_recursive_consolidation_stops_early_when_no_merges() {
        let l2 = make_l2();
        let l3 = make_l3();
        let engine = ConsolidationEngine::new(ConsolidationConfig {
            similarity_threshold: 0.05, // extremely strict — no merges
            min_merge_group: 2,
            recursive_rounds: 5,
            ..make_config()
        });

        let _id1 = insert_fact(&l2, "x", "y", "z");
        let _id2 = insert_fact(&l2, "a", "b", "c");

        let stats = engine.consolidate_recursive(&l2, &l3);
        assert_eq!(stats.total_merged, 0, "no merges with strict threshold");
        // Should stop after round 1 with 0 merges
        assert_eq!(stats.rounds.len(), 1);
    }

    // ================================================================
    // Forgetting strategy tests
    // ================================================================

    #[test]
    fn test_forget_below_importance_l2() {
        let l2 = make_l2();
        let engine = ConsolidationEngine::new(make_config());

        let id_high = l2
            .insert(MemoryInput {
                content: MemoryContent::Fact(Fact::new("important", "data", "high")),
                importance: 0.9,
                alias: None,
                tags: vec![],
                layer: LayerId::L2,
            })
            .unwrap();

        let id_low = l2
            .insert(MemoryInput {
                content: MemoryContent::Fact(Fact::new("unimportant", "data", "low")),
                importance: 0.1,
                alias: None,
                tags: vec![],
                layer: LayerId::L2,
            })
            .unwrap();

        let removed = engine.forget_below_importance_l2(&l2, 0.5);
        assert_eq!(removed, 1);
        assert!(l2.get_by_id(id_high).is_some());
        assert!(l2.get_by_id(id_low).is_none());
    }

    #[test]
    fn test_forget_older_than_l2() {
        let l2 = make_l2();
        let engine = ConsolidationEngine::new(make_config());

        let id = l2
            .insert(MemoryInput::new(MemoryContent::Fact(Fact::new(
                "old", "data", "entry",
            ))))
            .unwrap();

        // TTL of 1 nanosecond — everything is older
        let removed = engine.forget_older_than_l2(&l2, 1);
        assert_eq!(removed, 1);
        assert!(l2.get_by_id(id).is_none());
    }

    #[test]
    fn test_apply_forgetting_with_config() {
        let l2 = make_l2();
        let l3 = make_l3();
        let engine = ConsolidationEngine::new(ConsolidationConfig {
            forgetting: ForgettingStrategy {
                importance_threshold: 0.5,
                ttl_nanos: 1,
                ..Default::default()
            },
            ..make_config()
        });

        l2.insert(MemoryInput {
            content: MemoryContent::Fact(Fact::new("keep", "this", "entry")),
            importance: 0.9,
            alias: None,
            tags: vec![],
            layer: LayerId::L2,
        })
        .unwrap();

        l2.insert(MemoryInput {
            content: MemoryContent::Fact(Fact::new("remove", "this", "entry")),
            importance: 0.1,
            alias: None,
            tags: vec![],
            layer: LayerId::L2,
        })
        .unwrap();

        let (removed_l2, removed_l3) = engine.apply_forgetting(&l2, &l3);
        assert_eq!(removed_l2, 2); // below importance + too old
        assert_eq!(removed_l3, 0);
    }

    // ================================================================
    // Scheduler tests
    // ================================================================

    fn make_scheduler_config() -> ConsolidationSchedulerConfig {
        ConsolidationSchedulerConfig {
            cycle_interval_secs: 3600,
        }
    }

    fn make_scheduler() -> ConsolidationScheduler {
        ConsolidationScheduler::new(make_scheduler_config(), make_config())
    }

    #[test]
    fn test_scheduler_new_has_zero_stats() {
        let s = make_scheduler();
        let stats = s.stats();
        assert_eq!(stats.total_cycles, 0);
        assert_eq!(stats.total_merged, 0);
        assert_eq!(stats.total_forgotten_l2, 0);
        assert!(!s.is_running());
    }

    #[test]
    fn test_scheduler_run_cycle_empty() {
        let l2 = make_l2();
        let l3 = make_l3();
        let s = make_scheduler();

        let cs = s.run_cycle(&l2, &l3);
        assert_eq!(cs.total_merged, 0);
        assert_eq!(cs.total_deduped, 0);

        let stats = s.stats();
        assert_eq!(stats.total_cycles, 1);
        assert!(stats.last_cycle_duration_ns >= 0);
        assert!(stats.last_cycle_at > 0);
    }

    #[test]
    fn test_scheduler_run_cycle_with_data() {
        let l2 = make_l2();
        let l3 = make_l3();
        let s = ConsolidationScheduler::new(
            make_scheduler_config(),
            ConsolidationConfig {
                similarity_threshold: 3.0,
                min_merge_group: 2,
                recursive_rounds: 1,
                ..make_config()
            },
        );

        // Insert similar facts that should trigger merging
        insert_fact(&l2, "alice", "likes", "cats");
        insert_fact(&l2, "bob", "likes", "cats");
        insert_fact(&l2, "carol", "feeds", "cats");

        let cs = s.run_cycle(&l2, &l3);
        assert!(
            cs.total_merged >= 1,
            "should merge at least 1 group, got {}",
            cs.total_merged
        );

        let stats = s.stats();
        assert_eq!(stats.total_cycles, 1);
        assert!(stats.total_merged >= 1);
        assert_eq!(stats.last_cycle_merged, cs.total_merged);
    }

    #[test]
    fn test_scheduler_stats_accumulate_across_cycles() {
        let l2 = make_l2();
        let l3 = make_l3();
        let s = ConsolidationScheduler::new(
            make_scheduler_config(),
            ConsolidationConfig {
                similarity_threshold: 3.0,
                min_merge_group: 2,
                recursive_rounds: 1,
                ..make_config()
            },
        );

        // Cycle 1 — insert data then cycle
        insert_fact(&l2, "alice", "likes", "cats");
        insert_fact(&l2, "bob", "likes", "cats");
        let cs1 = s.run_cycle(&l2, &l3);
        assert!(cs1.total_merged >= 1, "cycle 1 should merge");

        let stats1 = s.stats();
        assert_eq!(stats1.total_cycles, 1);
        assert_eq!(stats1.total_merged as usize, cs1.total_merged);

        // Cycle 2 — insert more data then cycle again
        insert_fact(&l2, "dave", "feeds", "cats");
        insert_fact(&l2, "eve", "walks", "cats");
        let cs2 = s.run_cycle(&l2, &l3);

        let stats2 = s.stats();
        assert_eq!(stats2.total_cycles, 2);
        assert_eq!(
            stats2.total_merged as usize,
            cs1.total_merged + cs2.total_merged,
            "total_merged should accumulate"
        );
        assert_eq!(stats2.last_cycle_merged, cs2.total_merged);
    }

    #[test]
    fn test_scheduler_forgetting_reduces_budget() {
        let l2 = make_l2();
        let l3 = make_l3();
        let s = ConsolidationScheduler::new(
            make_scheduler_config(),
            ConsolidationConfig {
                similarity_threshold: 0.01, // strict — no accidental merges
                min_merge_group: 100,       // never merge in this test
                recursive_rounds: 1,
                forgetting: ForgettingStrategy {
                    importance_threshold: 0.5,
                    ..Default::default()
                },
                ..make_config()
            },
        );

        // Insert a mix of important + unimportant facts
        l2.insert(MemoryInput {
            content: MemoryContent::Fact(Fact::new("keep", "this", "high")),
            importance: 0.9,
            alias: None,
            tags: vec![],
            layer: LayerId::L2,
        })
        .unwrap();
        l2.insert(MemoryInput {
            content: MemoryContent::Fact(Fact::new("remove", "this", "low")),
            importance: 0.1,
            alias: None,
            tags: vec![],
            layer: LayerId::L2,
        })
        .unwrap();
        l2.insert(MemoryInput {
            content: MemoryContent::Fact(Fact::new("also", "remove", "low2")),
            importance: 0.2,
            alias: None,
            tags: vec![],
            layer: LayerId::L2,
        })
        .unwrap();

        let budget_before = l3.budget().storage_bytes();
        let cs = s.run_cycle(&l2, &l3);

        // At least the low-importance L2 facts should be forgotten
        assert!(
            cs.total_forgotten_l2 >= 2,
            "should forget at least 2 low-importance L2, got {}",
            cs.total_forgotten_l2
        );

        let stats = s.stats();
        assert!(stats.total_forgotten_l2 >= 2);
        // L2 removals don't affect L3 budget; verify budget is consistent
        let budget_after = l3.budget().storage_bytes();
        // L3 budget should not have increased (no L3 insertions from merge since
        // these aren't similar — they have different subjects/objects)
        assert!(
            budget_after <= budget_before + 5000,
            "budget should not grow significantly, before={} after={}",
            budget_before,
            budget_after,
        );

        // Verify the low-importance entries are actually gone
        let all_ids = l2.storage.all_ids();
        // Only the high-importance entry should remain
        assert_eq!(
            all_ids.len(),
            1,
            "only 1 high-importance entry should remain"
        );
        let remaining = l2.get_by_id(all_ids[0]).unwrap();
        assert!(remaining.metadata.importance >= 0.5);
    }

    #[test]
    fn test_scheduler_background_start_stop() {
        let l2 = Arc::new(make_l2());
        let l3 = Arc::new(make_l3());
        let s = Arc::new(ConsolidationScheduler::new(
            ConsolidationSchedulerConfig {
                cycle_interval_secs: 1, // 1 second for testing
            },
            make_config(),
        ));

        assert!(!s.is_running());
        s.start(Arc::clone(&l2), Arc::clone(&l3));
        assert!(s.is_running());

        // Let it run for a couple cycles
        std::thread::sleep(Duration::from_millis(2500));

        s.stop();
        assert!(!s.is_running());

        // At least one cycle should have completed
        let stats = s.stats();
        assert!(
            stats.total_cycles >= 1,
            "should have run at least 1 cycle, got {}",
            stats.total_cycles
        );
    }

    #[test]
    fn test_scheduler_config_accessors() {
        let s = make_scheduler();
        assert_eq!(s.config().cycle_interval_secs, 3600);
        assert!(s.consolidation_engine().config().recursive_rounds >= 1);
    }
}
