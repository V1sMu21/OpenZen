use std::collections::HashSet;
use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use tokio::sync::mpsc;

use crate::consolidation::ConsolidationStats;
use crate::core::types::{KnowledgeSource, MemoryMeta, Query};
use crate::l0::narrative::{NarrativeBuilder, NarrativeConfig};
use crate::l0::portrait::PortraitFact;
use crate::l0::soul::SoulHandle;
use crate::memory_store::MemoryStore;
use crate::phase1::types::default_factuality;
use crate::phase1::{ConflictResolution, ConflictResolver};
use crate::phase2::RamblingEngine;
use crate::phase4::{AnchorResult, RealityAnchor};
use crate::phase5::observer::BehaviorEvent;
use crate::phase5::BehaviorObserver;

/// 反思回路事件：由 `MemoryStore` 内部埋点产生，经无界 channel 投递。
#[derive(Debug, Clone)]
pub enum ReflectionEvent {
    MemoryStored {
        memory_id: u64,
        source: KnowledgeSource,
    },
    MemoryRecalled {
        memory_id: u64,
        context: String,
    },
    UserStatement {
        text: String,
        tags: Vec<String>,
    },
    ConsolidationDone {
        stats: ConsolidationStats,
    },
    Idle,
}

#[derive(Debug, Clone)]
pub struct ReflectionConfig {
    /// 攒多少事件再更新一次画像（默认 20）
    pub min_events_before_update: usize,
    /// 空闲内省间隔（秒，默认 300）
    pub idle_interval_secs: u64,
    /// 置信度浮动步长（默认 0.05）
    pub confidence_learning_rate: f32,
    /// 好奇心缺口阈值（默认 0.3）
    pub curiosity_threshold: f32,
    /// 画像事实最低置信度（低于则移除，默认 0.2）
    pub min_fact_confidence: f32,
    /// 从记忆中抽取新画像事实的 importance 门槛（默认 0.6）
    pub extract_importance_threshold: f32,
}

impl Default for ReflectionConfig {
    fn default() -> Self {
        Self {
            min_events_before_update: 20,
            idle_interval_secs: 300,
            confidence_learning_rate: 0.05,
            curiosity_threshold: 0.3,
            min_fact_confidence: 0.2,
            extract_importance_threshold: 0.6,
        }
    }
}

/// 反思回路：省察克治。
///
/// 订阅 `MemoryStore` 的事件流，执行三步更新：
/// Step1 事实核对（证据 → 置信度浮动，复用 ConflictResolver 判定）；
/// Step2 好奇心缺口（未知维度 → 联想目标，喂给 RamblingEngine）；
/// Step3 画像更新（SSS 有趣的猜想 + RealityAnchor 无冲突 → 写入画像事实）。
pub struct ReflectionEngine {
    soul: SoulHandle,
    store: Arc<MemoryStore>,
    observer: Arc<BehaviorObserver>,
    rambling: Option<Arc<RamblingEngine>>,
    reality_anchor: Option<Arc<RealityAnchor>>,
    conflict: Arc<ConflictResolver>,
    narrative: NarrativeBuilder,
    tx: mpsc::UnboundedSender<ReflectionEvent>,
    rx: Mutex<Option<mpsc::UnboundedReceiver<ReflectionEvent>>>,
    pending_events: AtomicUsize,
    persist_path: Mutex<Option<PathBuf>>,
    config: ReflectionConfig,
}

impl ReflectionEngine {
    pub fn new(
        soul: SoulHandle,
        store: Arc<MemoryStore>,
        observer: Arc<BehaviorObserver>,
        conflict: Arc<ConflictResolver>,
        config: ReflectionConfig,
    ) -> Self {
        let (tx, rx) = mpsc::unbounded_channel();
        Self {
            soul,
            store,
            observer,
            rambling: None,
            reality_anchor: None,
            conflict,
            narrative: NarrativeBuilder::new(NarrativeConfig::default()),
            tx,
            rx: Mutex::new(Some(rx)),
            pending_events: AtomicUsize::new(0),
            persist_path: Mutex::new(None),
            config,
        }
    }

    pub fn with_rambling(mut self, rambling: Arc<RamblingEngine>) -> Self {
        self.rambling = Some(rambling);
        self
    }

    pub fn with_reality_anchor(mut self, anchor: Arc<RealityAnchor>) -> Self {
        self.reality_anchor = Some(anchor);
        self
    }

    pub fn with_persist_path(self, path: PathBuf) -> Self {
        *self.persist_path.lock().unwrap() = Some(path);
        self
    }

    /// 非阻塞投递事件（不拖慢 store 热路径）。
    pub fn notify(&self, event: ReflectionEvent) -> bool {
        self.tx.send(event).is_ok()
    }

    pub fn soul(&self) -> SoulHandle {
        Arc::clone(&self.soul)
    }

    pub fn config(&self) -> &ReflectionConfig {
        &self.config
    }

    pub fn pending_count(&self) -> usize {
        self.pending_events.load(Ordering::Relaxed)
    }

    /// 消费事件队列。返回本批处理的事件数。
    pub fn process_pending(&self) -> usize {
        let events: Vec<ReflectionEvent> = {
            let mut guard = self.rx.lock().unwrap();
            let mut events = Vec::new();
            if let Some(rx) = guard.as_mut() {
                while let Ok(ev) = rx.try_recv() {
                    events.push(ev);
                }
            }
            events
        };
        let drained = events.len();
        if drained == 0 {
            return 0;
        }

        let mut full_cycle = false;
        for ev in events {
            match ev {
                ReflectionEvent::MemoryStored { memory_id, source } => {
                    self.step1_fact_check(memory_id, source);
                }
                ReflectionEvent::UserStatement { text, tags } => {
                    self.handle_user_statement(&text, &tags);
                }
                ReflectionEvent::ConsolidationDone { .. } => {
                    self.rebuild_narrative();
                }
                ReflectionEvent::Idle => full_cycle = true,
                ReflectionEvent::MemoryRecalled { memory_id, context } => {
                    self.handle_recall(memory_id, &context);
                }
            }
        }

        let pending = self.pending_events.fetch_add(drained, Ordering::Relaxed) + drained;
        if full_cycle || pending >= self.config.min_events_before_update {
            let gaps = self.curiosity_gaps();
            self.step3_apply_conjectures(&gaps);
            self.pending_events.store(0, Ordering::Relaxed);
            self.persist();
        }
        drained
    }

    /// 立即执行完整内省循环（Step2 + Step3 + 叙事刷新）。
    pub fn run_full_cycle(&self) {
        let gaps = self.curiosity_gaps();
        self.step3_apply_conjectures(&gaps);
        self.rebuild_narrative();
        self.persist();
    }

    // ---- Step1：事实核对（置信度贝叶斯式浮动） ----

    fn step1_fact_check(&self, memory_id: u64, source: KnowledgeSource) {
        let text = self
            .store
            .router()
            .l2()
            .get_by_id(memory_id)
            .or_else(|| self.store.router().l3().get_by_id(memory_id))
            .map(|m| m.content_text())
            .unwrap_or_default();
        if text.is_empty() {
            return;
        }
        let importance = self
            .store
            .router()
            .l2()
            .get_by_id(memory_id)
            .map(|m| m.metadata.importance)
            .unwrap_or(0.5);
        let lr = self.config.confidence_learning_rate;
        let min_conf = self.config.min_fact_confidence;

        let mut soul = self.soul.write().unwrap();
        let mut changed = false;

        changed |= self.step1_check_portrait(
            &mut soul.user_portrait,
            memory_id,
            source,
            &text,
            lr,
            min_conf,
        );
        changed |= self.step1_check_portrait(
            &mut soul.self_portrait,
            memory_id,
            source,
            &text,
            lr,
            min_conf,
        );

        // 抽取新事实：外部输入且 importance 达门槛、且不与已有事实重叠
        if source == KnowledgeSource::ExternalInput
            && importance >= self.config.extract_importance_threshold
            && !soul
                .user_portrait
                .facts
                .iter()
                .any(|f| lexical_compatibility(&text, &f.statement) > 0.3)
        {
            let fact = PortraitFact {
                statement: text.chars().take(120).collect(),
                confidence: default_factuality(source),
                supporting_ids: vec![memory_id],
                contradicting_ids: Vec::new(),
            };
            if soul.user_portrait.insert_fact(fact) {
                changed = true;
            }
        }

        if changed {
            soul.bump_version();
        }
    }

    /// 单个画像的事实核对：证据 → 置信度浮动，低置信事实移除。
    /// 返回是否发生任何变化。
    fn step1_check_portrait(
        &self,
        portrait: &mut crate::l0::Portrait,
        memory_id: u64,
        source: KnowledgeSource,
        text: &str,
        lr: f32,
        min_conf: f32,
    ) -> bool {
        let mut changed = false;
        let mut to_remove = Vec::new();
        for fact in portrait.facts.iter_mut() {
            let compatibility = lexical_compatibility(text, &fact.statement);
            if compatibility <= 0.0 {
                continue;
            }
            let new_meta = MemoryMeta {
                factuality: default_factuality(source),
                importance: 0.6,
                abstraction_level: 0.3,
                ..Default::default()
            };
            let old_meta = MemoryMeta {
                factuality: fact.confidence,
                importance: 0.5,
                abstraction_level: 0.3,
                ..Default::default()
            };
            match self.conflict.classify(&new_meta, &old_meta, compatibility) {
                ConflictResolution::Overturn => {
                    if !fact.contradicting_ids.contains(&memory_id) {
                        fact.contradicting_ids.push(memory_id);
                    }
                    fact.confidence = (fact.confidence - lr * 2.0).max(0.0);
                }
                ConflictResolution::Supplement | ConflictResolution::Sublimate => {
                    if !fact.supporting_ids.contains(&memory_id) {
                        fact.supporting_ids.push(memory_id);
                    }
                    fact.confidence = (fact.confidence + lr).min(1.0);
                }
            }
            changed = true;
            if fact.confidence < min_conf {
                to_remove.push(fact.statement.clone());
            }
        }
        for statement in to_remove {
            portrait.remove_fact(&statement);
            self.observer.record(BehaviorEvent::SelfCorrection {
                old_id: memory_id,
                new_id: 0,
                trigger: "portrait_fact_removed".into(),
            });
        }
        portrait.recompute_confidence();
        changed
    }

    // ---- Step2：好奇心缺口 ----

    /// 返回 gap 超过阈值的维度（traits 中置信度低的维度）。
    pub fn curiosity_gaps(&self) -> Vec<String> {
        let soul = self.soul.read().unwrap();
        let mut gaps: Vec<String> = soul
            .user_portrait
            .traits
            .iter()
            .filter(|(_, conf)| 1.0 - **conf > self.config.curiosity_threshold)
            .map(|(dim, _)| dim.clone())
            .collect();
        gaps.sort();
        gaps
    }

    // ---- Step3：画像更新 ----

    fn step3_apply_conjectures(&self, gap_dims: &[String]) {
        let Some(rambler) = &self.rambling else {
            return;
        };

        let mut seeds = Vec::new();
        for dim in gap_dims {
            let query = Query::by_text(dim);
            for (id, _) in self.store.router().l2().search_semantic(&query, 3) {
                seeds.push(id);
            }
        }
        let conjectures = if seeds.is_empty() {
            rambler.ramble()
        } else {
            rambler.ramble_with_seed(&seeds)
        };

        let anchor = self.reality_anchor.as_ref();
        let mut soul = self.soul.write().unwrap();
        let mut changed = false;
        for c in &conjectures {
            if c.sss_score <= 0.5 {
                continue;
            }
            if let Some(anchor) = anchor {
                if let AnchorResult::Anchored(_) = anchor.verify_against_anchors(c, &self.store) {
                    continue;
                }
            }
            let fact = PortraitFact {
                statement: c.statement.clone(),
                confidence: c.sss_score.clamp(0.2, 0.8),
                supporting_ids: vec![c.node_a, c.node_b],
                contradicting_ids: Vec::new(),
            };
            if soul.self_portrait.insert_fact(fact) {
                changed = true;
                self.observer.record(BehaviorEvent::InsightGenerated {
                    depth: c.sss_score,
                    statement: c.statement.clone(),
                });
            }
        }
        if changed {
            soul.bump_version();
        }
    }

    fn handle_user_statement(&self, text: &str, _tags: &[String]) {
        let statement: String = text.trim().chars().take(120).collect();
        if statement.is_empty() {
            return;
        }
        let mut soul = self.soul.write().unwrap();
        let fact = PortraitFact {
            statement: statement.clone(),
            confidence: 0.6,
            supporting_ids: Vec::new(),
            contradicting_ids: Vec::new(),
        };
        let is_new = soul.user_portrait.insert_fact(fact);
        soul.relationship.update(0.02, 0.04);
        if is_new {
            soul.bump_version();
            self.observer.record(BehaviorEvent::IdentityShift {
                field: "user_portrait".into(),
                old_value: String::new(),
                new_value: statement,
            });
        }
    }

    fn handle_recall(&self, memory_id: u64, context: &str) {
        let mut soul = self.soul.write().unwrap();
        if !context.is_empty() {
            soul.state.focus = context.chars().take(60).collect();
        }
        soul.state.last_updated_nanos = crate::core::now_nanos();
        self.observer.record(BehaviorEvent::CrossDomainLink {
            domain_a: format!("recall:{}", memory_id),
            domain_b: context.chars().take(30).collect(),
        });
    }

    fn rebuild_narrative(&self) {
        let since = self.soul.read().unwrap().narrative.last_rebuilt_nanos;
        let narrative = self.narrative.build(self.store.router().l3(), since);
        let mut soul = self.soul.write().unwrap();
        if !narrative.chapters.is_empty() {
            soul.narrative = narrative;
            soul.bump_version();
        }
    }

    fn persist(&self) {
        let path = self.persist_path.lock().unwrap().clone();
        if let Some(path) = path {
            let soul = self.soul.read().unwrap();
            let _ = soul.save_atomic(&path);
        }
    }
}

fn lexical_compatibility(a: &str, b: &str) -> f32 {
    let tokenize = |s: &str| -> HashSet<String> {
        s.split(|c: char| !c.is_alphanumeric())
            .map(|w| w.to_lowercase())
            .filter(|w| w.len() >= 2)
            .collect()
    };
    let ta = tokenize(a);
    let tb = tokenize(b);
    if ta.is_empty() || tb.is_empty() {
        return 0.0;
    }
    let inter = ta.intersection(&tb).count();
    let union = ta.union(&tb).count();
    if union == 0 {
        0.0
    } else {
        inter as f32 / union as f32
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::consolidation::ConsolidationConfig;
    use crate::core::types::{Fact, MemoryContent, MemoryInput};
    use crate::l0::SoulModel;
    use crate::l1::L1Cache;
    use crate::l2::{HnswConfig, L2Config, L2Engine};
    use crate::l3::{BudgetConfig, L3Config, L3Engine};
    use std::sync::RwLock;
    use tempfile::tempdir;

    fn make_store() -> Arc<MemoryStore> {
        let l1 = L1Cache::builder().capacity(100).build();
        let l2 = L2Engine::new(L2Config {
            hnsw: HnswConfig {
                dimension: 16,
                ..Default::default()
            },
            dimension: 16,
            ..Default::default()
        });
        let dir = tempdir().unwrap();
        let l3 = L3Engine::new(L3Config {
            storage_path: dir.path().join("soul_reflect.bin"),
            budget: BudgetConfig {
                daily_token_limit: 1_000_000,
                annual_storage_limit: 10_000_000,
            },
            ..Default::default()
        });
        Arc::new(MemoryStore::new(
            l1,
            Arc::new(l2),
            l3,
            ConsolidationConfig::default(),
        ))
    }

    fn make_engine(store: Arc<MemoryStore>) -> Arc<ReflectionEngine> {
        let soul: SoulHandle = Arc::new(RwLock::new(SoulModel::new()));
        let observer = Arc::new(BehaviorObserver::new());
        let conflict = Arc::new(ConflictResolver::new(Arc::new(L2Engine::new(L2Config {
            hnsw: HnswConfig {
                dimension: 16,
                ..Default::default()
            },
            dimension: 16,
            ..Default::default()
        }))));
        let engine = Arc::new(ReflectionEngine::new(
            soul,
            store.clone(),
            observer,
            conflict,
            ReflectionConfig::default(),
        ));
        store.attach_soul(Arc::clone(&engine));
        engine
    }

    #[test]
    fn test_config_defaults() {
        let c = ReflectionConfig::default();
        assert_eq!(c.min_events_before_update, 20);
        assert_eq!(c.confidence_learning_rate, 0.05);
        assert_eq!(c.curiosity_threshold, 0.3);
    }

    #[test]
    fn test_step1_contradiction_drifts_and_removes() {
        let store = make_store();
        let engine = make_engine(store.clone());
        let soul = engine.soul();

        {
            let mut s = soul.write().unwrap();
            s.user_portrait.insert_fact(PortraitFact {
                statement: "user likes coffee".into(),
                confidence: 0.5,
                supporting_ids: Vec::new(),
                contradicting_ids: Vec::new(),
            });
        }

        // 连续 5 次「相似但相反」的记忆 → 每次 Overturn 使 confidence -2*lr
        for i in 0..5u64 {
            let id = store
                .store(MemoryInput::new(MemoryContent::Fact(Fact::new(
                    "user",
                    "dislikes",
                    &format!("coffee variant {}", i),
                ))))
                .unwrap();
            engine.notify(ReflectionEvent::MemoryStored {
                memory_id: id,
                source: KnowledgeSource::ExternalInput,
            });
            engine.process_pending();
        }

        let s = soul.read().unwrap();
        assert!(
            s.user_portrait.find("user likes coffee").is_none(),
            "fact must be removed once confidence drops below threshold"
        );
    }

    #[test]
    fn test_step1_support_raises_confidence() {
        let store = make_store();
        let engine = make_engine(store.clone());
        let soul = engine.soul();
        {
            let mut s = soul.write().unwrap();
            s.user_portrait.insert_fact(PortraitFact {
                statement: "user likes coffee".into(),
                confidence: 0.5,
                supporting_ids: Vec::new(),
                contradicting_ids: Vec::new(),
            });
        }
        let id = store
            .store(MemoryInput::new(MemoryContent::Fact(Fact::new(
                "user", "likes", "coffee",
            ))))
            .unwrap();
        engine.notify(ReflectionEvent::MemoryStored {
            memory_id: id,
            source: KnowledgeSource::ExternalInput,
        });
        engine.process_pending();

        let s = soul.read().unwrap();
        let fact = s.user_portrait.find("user likes coffee").unwrap();
        assert!(
            fact.confidence > 0.5,
            "supporting evidence must raise confidence, got {}",
            fact.confidence
        );
        assert!(fact.supporting_ids.contains(&id));
    }

    #[test]
    fn test_store_20_creates_user_facts() {
        let store = make_store();
        let engine = make_engine(store.clone());

        for i in 0..20 {
            let mut input = MemoryInput::new(MemoryContent::Fact(Fact::new(
                "user",
                "worked_on",
                &format!("project_{}", i),
            )));
            input.importance = 0.9;
            store.store(input).unwrap();
        }
        engine.process_pending();

        let soul_handle = engine.soul();
        let s = soul_handle.read().unwrap();
        assert!(
            !s.user_portrait.facts.is_empty(),
            "external high-importance memories must seed user portrait facts"
        );
        assert!(s.version >= 1);
    }

    #[test]
    fn test_curiosity_gaps_detects_unknown_dimensions() {
        let engine = make_engine(make_store());
        let soul = engine.soul();
        {
            let mut s = soul.write().unwrap();
            s.user_portrait.traits.insert("偏好".into(), 0.9);
            s.user_portrait.traits.insert("未知领域".into(), 0.1);
        }
        let gaps = engine.curiosity_gaps();
        assert!(gaps.contains(&"未知领域".to_string()));
        assert!(!gaps.contains(&"偏好".to_string()));
    }

    #[test]
    fn test_user_statement_updates_relationship() {
        let engine = make_engine(make_store());
        engine.notify(ReflectionEvent::UserStatement {
            text: "I prefer to work in the morning".into(),
            tags: vec!["preference".into()],
        });
        engine.process_pending();

        let soul_handle = engine.soul();
        let s = soul_handle.read().unwrap();
        assert!(
            s.user_portrait
                .find("I prefer to work in the morning")
                .is_some(),
            "user statement must become a portrait fact"
        );
        assert!(
            s.relationship.trust > 0.5,
            "relationship trust must rise after interaction"
        );
    }

    #[test]
    fn test_version_bumps_on_events() {
        let store = make_store();
        let engine = make_engine(store.clone());
        let v0 = engine.soul().read().unwrap().version;
        let mut input = MemoryInput::new(MemoryContent::Fact(Fact::new("user", "likes", "tea")));
        input.importance = 0.9;
        let id = store.store(input).unwrap();
        engine.notify(ReflectionEvent::MemoryStored {
            memory_id: id,
            source: KnowledgeSource::ExternalInput,
        });
        engine.process_pending();
        let v1 = engine.soul().read().unwrap().version;
        assert!(v1 > v0, "portrait changes must bump soul version");
    }

    #[test]
    fn test_process_pending_no_events() {
        let engine = make_engine(make_store());
        assert_eq!(engine.process_pending(), 0);
    }
}
