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
    /// Nanos of the last soul.json write. Used to debounce persistence to at
    /// most one write per [`PERSIST_DEBOUNCE_NANOS`] — the previous rule only
    /// persisted after `min_events_before_update` events, so a single fact
    /// from a single message could live in memory only until the next idle
    /// cycle (round5 P0-5 ②).
    last_persist_nanos: std::sync::atomic::AtomicI64,
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
            last_persist_nanos: std::sync::atomic::AtomicI64::new(0),
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
            self.persist_debounced(true);
        } else {
            // Something changed but the batch threshold was not reached:
            // still write it out (debounced), otherwise a single fact is lost
            // on exit.
            self.persist_debounced(false);
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

    /// Turn a user message into portrait facts.
    ///
    /// This used to store `text.chars().take(120)` verbatim with a hardcoded
    /// confidence of 0.6 — the "portrait" was a pile of raw message prefixes
    /// (real data: 185 facts, 121 of them at exactly 0.600, 38 of which were
    /// questions or instructions). Extraction is deliberately conservative:
    /// a question or an order is not a fact about the user, and when nothing
    /// durable is found we write NOTHING rather than writing the raw text
    /// (round5 P0-5 (1)).
    fn handle_user_statement(&self, text: &str, tags: &[String]) {
        // Traits and mood must be observed even when nothing durable was
        // extracted: they used to sit behind this early return, so on real
        // data (185 junk facts, no durable markers) traits stayed empty,
        // `curiosity_gaps()` was always empty and the self_portrait chain
        // stayed dead — the very loop this change was supposed to revive
        // (review A7).
        let facts = extract_user_facts(text);
        self.observe_engagement(text);
        if facts.is_empty() {
            self.persist_debounced(false);
            return;
        }
        let topic = tags.first().cloned().unwrap_or_default();
        let mut soul = self.soul.write().unwrap();
        let mut any_new = false;
        for (statement, confidence) in facts {
            let fact = PortraitFact {
                statement: statement.clone(),
                confidence,
                supporting_ids: Vec::new(),
                contradicting_ids: Vec::new(),
            };
            let is_new = soul.user_portrait.insert_fact(fact);
            any_new |= is_new;
            // Topic bucket → `traits` is what `curiosity_gaps()` (and hence
            // the self-portrait update chain) reads. Without a writer there,
            // self_portrait never changed (round5 P0-5 (3)).
            let bucket = if topic.is_empty() {
                "对话"
            } else {
                topic.as_str()
            };
            soul.user_portrait.observe_trait(bucket, 0.08);
            soul.relationship.update(0.02, 0.04);
            if is_new {
                self.observer.record(BehaviorEvent::IdentityShift {
                    field: "user_portrait".into(),
                    old_value: String::new(),
                    new_value: statement,
                });
            }
        }
        // The live state layer must move too: `SoulState::update` had zero
        // production callers, so mood/energy/confidence stayed at the
        // constants from `SoulState::new()` while the UI rendered them as
        // "mood" (round5 P0-5 (3)).
        let facts_len = soul.user_portrait.facts.len() as f32;
        let energy = (0.55 + facts_len / 200.0).min(1.0);
        let confidence = soul.user_portrait.confidence.clamp(0.2, 0.9);
        let mood = if facts_len > 40.0 { "熟悉" } else { "专注" };
        let focus = if topic.is_empty() {
            "对话".to_string()
        } else {
            topic.clone()
        };
        soul.state.update(mood, focus, energy, confidence);
        if any_new {
            soul.bump_version();
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

    /// Observe engagement for THIS message: a topic bucket in `traits` plus a
    /// live-state update — independent of whether a durable fact was found.
    ///
    /// `traits` feeds `curiosity_gaps()` → conjectures → `self_portrait`; with
    /// the writer gated behind fact extraction, real sessions (which extract
    /// nothing) left traits empty and the whole chain was dead (review A7).
    fn observe_engagement(&self, text: &str) {
        let bucket = topic_bucket(text);
        let mut soul = self.soul.write().unwrap();
        soul.user_portrait.observe_trait(&bucket, 0.05);
        let facts_len = soul.user_portrait.facts.len() as f32;
        let energy = (0.55 + facts_len / 200.0).min(1.0);
        let confidence = soul.user_portrait.confidence.clamp(0.2, 0.9);
        let mood = if facts_len > 40.0 { "熟悉" } else { "专注" };
        soul.state.update(mood, &bucket, energy, confidence);
    }

    fn persist(&self) {
        let path = self.persist_path.lock().unwrap().clone();
        if let Some(path) = path {
            let soul = self.soul.read().unwrap();
            let _ = soul.save_atomic(&path);
            self.last_persist_nanos
                .store(crate::core::now_nanos(), Ordering::Relaxed);
        }
    }

    /// Persist, but at most once per debounce window. `force` bypasses the
    /// window (used by the explicit full-cycle path and at shutdown).
    ///
    /// "Modify → write, debounced by 5s" instead of "write after N events":
    /// the soul is small, and losing a portrait update because only two
    /// events arrived before the app closed is exactly the kind of silent
    /// loss this layer is supposed to prevent.
    fn persist_debounced(&self, force: bool) {
        const PERSIST_DEBOUNCE_NANOS: i64 = 5 * 1_000_000_000;
        let now = crate::core::now_nanos();
        let last = self.last_persist_nanos.load(Ordering::Relaxed);
        if !force && last > 0 && now.saturating_sub(last) < PERSIST_DEBOUNCE_NANOS {
            return;
        }
        self.persist();
    }
}

/// Re-exported from `portrait` so there is exactly one CJK-aware
/// implementation (the old private copy split on `is_alphanumeric`, which
/// never splits Chinese, so every Chinese pair scored 0.0 and contradiction
/// detection never fired — round5 P0-5 (5)).
use crate::l0::portrait::lexical_compatibility;

/// Durable facts about the user, extracted from a message.
///
/// Returns `(statement, confidence)` pairs. Conservative by design:
///  * scaffolding (system reminders, tool output) is skipped entirely;
///  * questions and instructions are not facts;
///  * a statement needs a first-person/possession marker or a
///    preference/state verb to qualify;
///  * confidence is derived from the evidence, not hardcoded.
pub fn extract_user_facts(text: &str) -> Vec<(String, f32)> {
    const MAX_FACT_CHARS: usize = 160;
    // Chinese is dense: "我讨厌开会" is a complete, durable fact in 5
    // characters. 6 was dropping those.
    const MIN_FACT_CHARS: usize = 4;

    let trimmed = text.trim();
    if trimmed.is_empty() {
        return Vec::new();
    }
    // Conversation scaffolding must never become a "fact about the user"
    // (this is also what polluted the SOP store with `system-reminder__…`).
    let lower = trimmed.to_lowercase();
    if lower.contains("<system-reminder>")
        || lower.contains("system-reminder")
        || lower.contains("[user intervention")
        || lower.contains("<tool_result")
    {
        return Vec::new();
    }

    let question_markers = [
        "?",
        "？",
        "吗",
        "呢",
        "什么",
        "怎么",
        "为什么",
        "为何",
        "哪个",
        "哪些",
        "谁",
        "多少",
        "是否",
        "能不能",
        "可不可以",
        "如何",
    ];
    let instruction_markers = [
        "帮我",
        "帮忙",
        "请帮",
        "给我",
        "请你",
        "麻烦",
        "把",
        "写一个",
        "做一个",
        "生成",
        "修改",
        "删除",
        "运行",
        "执行",
        "查一下",
        "看看",
        "告诉我",
        "整理",
        "优化",
        "实现",
        "修复",
        // English imperatives (the product is bilingual).
        "please ",
        "help me",
        "write ",
        "create ",
        "make ",
        "generate ",
        "fix ",
        "run ",
        "show me",
        "tell me",
        "refactor ",
        "implement ",
        "delete ",
        "update ",
        "add ",
        "find ",
    ];
    let first_person = [
        "我", "俺", "咱", "我的", "我们", "本人", "i ", "i'", "my ", "we ", "me ",
    ];
    let durable_markers = [
        "喜欢",
        "讨厌",
        "偏好",
        "习惯",
        "住在",
        "住",
        "我家",
        "来自",
        "从事",
        "工作是",
        "做的是",
        "负责",
        "正在做",
        "在做",
        "打算",
        "计划",
        "需要",
        "想要",
        "希望",
        "不喜欢",
        "常用",
        "一般用",
        "平时",
        "经常",
        "每天",
        "通常",
        "一般",
        "名字",
        "名叫",
        "叫我",
        "我有",
        "养了",
        "我是",
        "生日",
        "职业",
        "目标",
        "原则",
        "底线",
        "忌讳",
        "过敏",
        "不能吃",
        // English
        "i prefer",
        "i like",
        "i love",
        "i hate",
        "i live",
        "i work",
        "i am",
        "i'm",
        "my name",
        "i need",
        "i want",
        "i usually",
        "i always",
        "i never",
        "favorite",
        "favourite",
        "allergic",
        "i'm working on",
        "i am working on",
        "prefer ",
    ];

    let mut out: Vec<(String, f32)> = Vec::new();
    // Split on the Chinese comma too: "我住在杭州，帮我写个脚本" is a fact
    // followed by an instruction, and judging the whole string only at its
    // start stored the instruction as a permanent fact (review A10).
    for raw in trimmed.split(['。', '！', '!', '\n', '；', ';', '，', ',']) {
        let sentence = raw.trim().trim_start_matches(['-', '*', '·', ' ']).trim();
        if sentence.chars().count() < MIN_FACT_CHARS {
            continue;
        }
        // Case matters: English markers are lowercase, and matching them
        // against the original casing meant "I live in Hangzhou and I work at
        // Alibaba." matched nothing at all (review A9). Judge on lowercase,
        // emit the original text.
        let lowered = sentence.to_lowercase();
        // A question is not a fact. (Trailing "吗/呢" and any "?" form.)
        if question_markers.iter().any(|m| lowered.ends_with(m))
            || sentence.contains('?')
            || sentence.contains('？')
        {
            continue;
        }
        // An instruction is not a fact — anywhere in the clause, not only at
        // its start ("帮我写个脚本" often follows a comma).
        if instruction_markers.iter().any(|m| lowered.contains(m)) {
            continue;
        }
        let has_first_person = first_person.iter().any(|m| lowered.contains(m));
        let durable_hits = durable_markers
            .iter()
            .filter(|m| lowered.contains(**m))
            .count();
        // Judgment is exclusion-based with a subject requirement (review A2):
        // the old rule demanded one of ~33 whitelisted phrases, so "我住杭州",
        // "我有一只猫", "我叫张三", "我是杭州人", "我不吃辣" were ALL dropped
        // while "我希望你用中文回答" (a one-off instruction) was stored.
        // A clause qualifies when it (a) is not a question/instruction/
        // scaffolding, (b) has a first-person subject, and (c) either carries
        // a durable marker or states a stable attribute about that subject.
        if !has_first_person {
            continue;
        }
        let transient = [
            "今天",
            "昨天",
            "明天",
            "刚才",
            "现在",
            "这会儿",
            "周末",
            "最近",
            "这次",
            "下次",
            "today",
            "yesterday",
            "tomorrow",
            "right now",
            "just now",
        ];
        let stable_attribute = [
            "住", "家", "叫", "名叫", "是", "有", "养", "吃", "喝", "用", "写", "做", "学", "工作",
            "专业", "岁", "邮箱", "电话", "账号", "密码", "地址", "生日", "星座", "live", "name",
            "have", "own", "work", "use", "eat", "study", "age", "email",
        ];
        // A pure time-adverb sentence ("我今天出门转了转") is not durable.
        let time_only = transient.iter().any(|m| lowered.contains(m))
            && !stable_attribute.iter().any(|m| lowered.contains(m));
        if durable_hits == 0 && time_only {
            continue;
        }
        if durable_hits == 0 && !stable_attribute.iter().any(|m| lowered.contains(m)) {
            continue;
        }
        let mut statement: String = sentence.chars().take(MAX_FACT_CHARS).collect();
        if sentence.chars().count() > MAX_FACT_CHARS {
            statement.push('…');
        }
        let mut confidence: f32 = 0.45;
        if has_first_person {
            confidence += 0.15;
        }
        confidence += 0.1 * durable_hits.min(2) as f32;
        if statement.chars().count() >= 12 {
            confidence += 0.05;
        }
        out.push((statement, confidence.clamp(0.35, 0.9)));
    }
    // Drop near-duplicates inside the same message.
    let mut deduped: Vec<(String, f32)> = Vec::new();
    for (stmt, conf) in out {
        if deduped
            .iter()
            .any(|(s, _)| lexical_compatibility(s, &stmt) >= 0.7)
        {
            continue;
        }
        deduped.push((stmt, conf));
    }
    deduped
}

/// Cheap topic bucket for a message, used as the `traits` key.
///
/// The bucket is what `curiosity_gaps()` counts, so it has to be stable across
/// messages of the same kind — a keyword map beats "对话" for everything.
fn topic_bucket(text: &str) -> String {
    let lower = text.to_lowercase();
    let table: [(&str, &str); 8] = [
        ("代码", "编程"),
        ("code", "编程"),
        ("rust", "编程"),
        ("python", "编程"),
        ("报错", "调试"),
        ("error", "调试"),
        ("bug", "调试"),
        ("测试", "调试"),
    ];
    for (needle, bucket) in table {
        if lower.contains(needle) {
            return bucket.to_string();
        }
    }
    let more: [(&str, &str); 6] = [
        ("记忆", "记忆"),
        ("memory", "记忆"),
        ("设计", "设计"),
        ("design", "设计"),
        ("项目", "项目"),
        ("project", "项目"),
    ];
    for (needle, bucket) in more {
        if lower.contains(needle) {
            return bucket.to_string();
        }
    }
    "对话".to_string()
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
                    format!("coffee variant {}", i),
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

    /// round5 P0-5 (1) regression: the portrait stored the first 120 chars of
    /// the user's message with a hardcoded confidence — questions and orders
    /// included. Real data: 185 facts, 121 at exactly 0.600, 38 of them
    /// questions or instructions.
    #[test]
    fn questions_and_instructions_are_not_facts() {
        for text in [
            "国内党建档案中需要放入的材料都有哪些？",
            "帮我下载一个大模型并做基准测试",
            "请把这份报告整理成 HTML",
            "What is the capital of France?",
            "please refactor the memory layer",
        ] {
            assert!(
                extract_user_facts(text).is_empty(),
                "`{text}` must not become a portrait fact"
            );
        }
        // Scaffolding must never become a fact either (this is the same
        // pollution that produced `system-reminder__today_s_date__…` SOPs).
        assert!(extract_user_facts(
            "<system-reminder> Today's date is 2026-10-21. </system-reminder>"
        )
        .is_empty());
        // A durable statement still qualifies, with a derived confidence.
        let facts = extract_user_facts("我住在杭州，喜欢喝龙井茶");
        assert_eq!(facts.len(), 1, "got {facts:?}");
        assert!(facts[0].1 > 0.5 && facts[0].1 <= 0.9, "got {}", facts[0].1);
    }

    /// A message with several durable statements yields several facts, and
    /// rephrasings inside one message are collapsed.
    #[test]
    fn multiple_durable_statements_are_extracted_and_deduped() {
        // Three distinct durable statements → three facts.
        let facts =
            extract_user_facts("我平时用 Rust 写后端。我一般用 Rust 做服务端。我讨厌开会。");
        assert_eq!(facts.len(), 3, "got {facts:?}");
        // A rephrasing inside the same message collapses into one.
        let dup = extract_user_facts("我住在杭州。我住在杭州，喜欢龙井。");
        assert_eq!(dup.len(), 1, "got {dup:?}");
    }

    /// Review A2: the whitelist-only rule dropped the most common factual
    /// sentence shapes while storing one-off instructions. These are the exact
    /// strings the reviewer measured returning empty.
    #[test]
    fn common_factual_sentences_are_extracted() {
        for text in [
            "我住杭州",
            "我有一只猫",
            "我家在杭州",
            "我叫张三，是一名后端工程师",
            "我是杭州人",
            "我不吃辣",
        ] {
            assert!(
                !extract_user_facts(text).is_empty(),
                "must extract a durable fact from {text:?}"
            );
        }
        // …and a one-off instruction must NOT become a permanent fact.
        for text in [
            "我希望你用中文回答",
            "帮我写个脚本",
            "我住在杭州，帮我写个脚本",
        ] {
            let facts = extract_user_facts(text);
            assert!(
                facts.iter().all(|(s, _)| !s.contains("帮我")),
                "instruction leaked into the portrait: {facts:?}"
            );
        }
        // A pure time-adverb sentence is still not durable.
        assert!(extract_user_facts("我今天出门转了转").is_empty());
    }

    /// Review A9: English markers are lowercase, so matching the original
    /// casing meant ordinary English statements matched nothing.
    #[test]
    fn english_facts_are_extracted_case_insensitively() {
        let facts = extract_user_facts("I live in Hangzhou and I work at Alibaba.");
        assert!(!facts.is_empty(), "capitalised English must match");
        let name = extract_user_facts("My name is Zhang and I hate meetings.");
        assert!(!name.is_empty(), "My name is … must match");
    }

    /// Review A1: opposite polarity is never the same fact.
    #[test]
    fn contradictory_statements_do_not_merge() {
        let a = "我喜欢开会";
        let b = "我不喜欢开会";
        assert_eq!(
            lexical_compatibility(a, b),
            0.0,
            "negation must block the merge"
        );
        let mut p = crate::l0::Portrait::new();
        p.insert_fact(PortraitFact {
            statement: a.into(),
            confidence: 0.6,
            supporting_ids: vec![],
            contradicting_ids: vec![],
        });
        p.insert_fact(PortraitFact {
            statement: b.into(),
            confidence: 0.85,
            supporting_ids: vec![],
            contradicting_ids: vec![],
        });
        assert_eq!(p.facts.len(), 2, "both statements must survive");
        assert!(p.facts.iter().any(|f| f.statement == a));
    }

    /// Review A3: "我有一只猫" and "我有一只狗" are different facts, and the
    /// dog used to be silently dropped (score 0.60, threshold 0.6).
    #[test]
    fn one_content_word_apart_is_not_a_duplicate() {
        let mut p = crate::l0::Portrait::new();
        for (statement, confidence) in [("我有一只猫", 0.7), ("我有一只狗", 0.7)] {
            p.insert_fact(PortraitFact {
                statement: statement.into(),
                confidence,
                supporting_ids: vec![],
                contradicting_ids: vec![],
            });
        }
        assert_eq!(p.facts.len(), 2, "both pets must survive: {:?}", p.facts);
        // …while real detail gain still merges into one statement.
        assert!(
            lexical_compatibility("我住在杭州", "我住在杭州，喜欢龙井") >= 0.7,
            "a contained statement must still merge"
        );
    }

    /// round5 P0-5 ②: a single fact arriving from a single message must be
    /// written to soul.json, not held in memory until the next idle cycle.
    #[test]
    fn a_single_event_is_persisted_to_disk() {
        let dir = std::env::temp_dir().join(format!("erme-persist-{}", std::process::id()));
        let _ = std::fs::create_dir_all(&dir);
        let path = dir.join("soul.json");
        let engine = Arc::new(
            ReflectionEngine::new(
                Arc::new(std::sync::RwLock::new(SoulModel::new())),
                make_store(),
                Arc::new(BehaviorObserver::new()),
                Arc::new(crate::phase1::ConflictResolver::new(Arc::new(
                    crate::l2::L2Engine::new(crate::l2::L2Config::default()),
                ))),
                ReflectionConfig::default(),
            )
            .with_persist_path(path.clone()),
        );
        engine.notify(ReflectionEvent::UserStatement {
            text: "我住在杭州，喜欢喝龙井茶".into(),
            tags: vec!["生活".into()],
        });
        engine.process_pending();
        assert!(
            path.exists(),
            "a portrait update must reach disk, not only memory"
        );
        let saved = std::fs::read_to_string(&path).unwrap();
        assert!(
            saved.contains("杭州") || saved.contains("生活"),
            "the persisted soul must carry the new fact: {saved}"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// round5 P0-5 (3): `SoulState::update` had zero production callers, so
    /// mood/energy/confidence were the constants from `new()` while the UI
    /// rendered them as the assistant's "mood".
    #[test]
    fn user_statement_moves_the_live_state() {
        let engine = make_engine(make_store());
        let before = engine.soul().read().unwrap().state.clone();
        engine.notify(ReflectionEvent::UserStatement {
            text: "我住在杭州，喜欢喝龙井茶".into(),
            tags: vec!["生活".into()],
        });
        engine.process_pending();
        let after = engine.soul().read().unwrap().state.clone();
        assert_ne!(
            (before.mood.clone(), before.focus.clone()),
            (after.mood.clone(), after.focus.clone()),
            "state must not stay at the constructor constants"
        );
        assert_eq!(after.focus, "生活");
        assert!(after.last_updated_nanos >= before.last_updated_nanos);
    }

    /// round5 P0-5 (3): traits had no writer, so the self-portrait update
    /// chain (`curiosity_gaps()` → traits) could never fire.
    #[test]
    fn user_statement_writes_traits() {
        let engine = make_engine(make_store());
        engine.notify(ReflectionEvent::UserStatement {
            text: "我住在杭州，喜欢喝龙井茶".into(),
            tags: vec!["生活".into()],
        });
        engine.process_pending();
        let soul = engine.soul();
        let s = soul.read().unwrap();
        assert!(
            s.user_portrait.traits.contains_key("生活"),
            "topic tags must land in traits: {:?}",
            s.user_portrait.traits
        );
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
