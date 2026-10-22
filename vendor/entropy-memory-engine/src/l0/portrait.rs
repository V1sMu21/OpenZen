use std::collections::HashMap;

use serde::{Deserialize, Serialize};

use crate::core::now_nanos;

/// Characters that carry no discriminating power in Chinese and would make
/// every pair of sentences look similar.
/// Particles and pronouns only. Content-bearing characters stay in the token
/// set even when they are frequent, because dropping them destroys meaning:
/// `不` used to be here, which made "我喜欢开会" and "我不喜欢开会" tokenize
/// identically (similarity 0.73) and the second one OVERWROTE the first —
/// the portrait recorded the opposite of what the user said (review A1).
/// `有` is kept for the same reason ("我有一只猫" vs "我没有猫").
const CJK_STOP_CHARS: &[char] = &[
    '的', '了', '呢', '吗', '吧', '啊', '我', '你', '他', '她', '是', '在', '和', '就', '也', '都',
    '很', '这', '那', '个', '们', '要', '会', '着', '过', '把', '被', '给', '对', '与',
];

/// Negation markers. Two statements whose polarity differs are never the same
/// fact, no matter how similar their tokens look.
const NEGATION_MARKERS: &[&str] = &[
    "不",
    "没",
    "无",
    "别",
    "非",
    "未",
    "don't",
    "doesn't",
    "not",
    "never",
    "no longer",
];

/// Does the statement carry a negation?
fn has_negation(s: &str) -> bool {
    let lower = s.to_lowercase();
    NEGATION_MARKERS.iter().any(|m| lower.contains(m))
}

fn is_cjk(c: char) -> bool {
    matches!(c as u32, 0x3400..=0x4DBF | 0x4E00..=0x9FFF | 0xF900..=0xFAFF)
}

/// Tokenize for lexical comparison, CJK-aware.
///
/// The upstream implementation split on `!is_alphanumeric()`, but every Han
/// character *is* alphanumeric — so a whole Chinese sentence became a single
/// token and any two different sentences scored a Jaccard similarity of 0.0.
/// Contradiction detection, confidence promotion/demotion and fact dedup were
/// therefore dead for Chinese input ("我住在杭州" vs "我住在北京" → 0.0), which
/// is exactly the language this product is used in (round5 P0-5 (5)).
///
/// CJK runs become character bigrams plus non-stop unigrams; Latin words stay
/// whole words.
pub fn tokenize_for_similarity(s: &str) -> std::collections::HashSet<String> {
    let mut out = std::collections::HashSet::new();
    for word in s.split(|c: char| !c.is_alphanumeric()) {
        if word.is_empty() {
            continue;
        }
        if word.chars().any(is_cjk) {
            let chars: Vec<char> = word.chars().collect();
            if chars.len() == 1 {
                out.insert(word.to_lowercase());
                continue;
            }
            for pair in chars.windows(2) {
                out.insert(pair.iter().collect::<String>());
            }
            for c in &chars {
                if !CJK_STOP_CHARS.contains(c) {
                    out.insert(c.to_string());
                }
            }
        } else {
            let w = word.to_lowercase();
            if w.chars().count() >= 2 {
                out.insert(w);
            }
        }
    }
    out
}

/// Similarity over [`tokenize_for_similarity`] tokens.
///
/// `max(Jaccard, 0.8 · containment)`, with two guards.
///
/// Plain Jaccard punishes a short statement fully contained in a longer one
/// ("我住在杭州" inside "我住在杭州，喜欢龙井"), which is the normal shape of
/// a portrait fact gaining detail — those should merge.
///
/// But raw containment also merges statements that differ in one content word:
/// "我有一只猫" vs "我有一只狗" scored 0.61 and the dog was silently dropped
/// (review A3). Two guards:
///   1. containment only counts when the smaller token set is a real SUBSET of
///      the larger one (a pure containment relation), not merely overlapping;
///   2. opposite polarity never merges (review A1).
pub fn lexical_compatibility(a: &str, b: &str) -> f32 {
    if has_negation(a) != has_negation(b) {
        return 0.0;
    }
    let ta = tokenize_for_similarity(a);
    let tb = tokenize_for_similarity(b);
    if ta.is_empty() || tb.is_empty() {
        return 0.0;
    }
    let inter = ta.intersection(&tb).count();
    let union = ta.union(&tb).count();
    let jaccard = if union == 0 {
        0.0
    } else {
        inter as f32 / union as f32
    };
    let (small, large) = if ta.len() <= tb.len() {
        (&ta, &tb)
    } else {
        (&tb, &ta)
    };
    let containment = if small.is_empty() || !small.is_subset(large) {
        0.0
    } else {
        inter as f32 / small.len() as f32
    };
    jaccard.max(containment * 0.8)
}

/// 画像（自画像与用户画像共用同一结构）
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Portrait {
    /// 一段话画像
    pub summary: String,
    /// 特质 → 置信度 (0~1)
    pub traits: HashMap<String, f32>,
    /// 关键事实（带证据链接）
    pub facts: Vec<PortraitFact>,
    /// 整体置信度
    pub confidence: f32,
    pub last_updated_nanos: i64,
}

/// 画像事实：必须带证据（记忆 ID），保证可溯源、可被推翻
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct PortraitFact {
    pub statement: String,
    /// 随支持/反对证据浮动（贝叶斯式）
    pub confidence: f32,
    /// 支持证据：L2/L3 记忆 ID
    pub supporting_ids: Vec<u64>,
    /// 反对证据
    pub contradicting_ids: Vec<u64>,
}

/// 关系画像：「仁者以天地万物为一体」
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Relationship {
    /// 0.0 ~ 1.0
    pub trust: f32,
    /// 0.0 ~ 1.0
    pub intimacy: f32,
    /// 0.0 ~ 1.0（1 = 边界清晰）
    pub boundary: f32,
    /// 我眼中的我们："陪伴者/协作者/学生..."
    pub role: String,
    pub last_updated_nanos: i64,
}

impl Portrait {
    pub fn new() -> Self {
        let now = now_nanos();
        Self {
            summary: String::new(),
            traits: HashMap::new(),
            facts: Vec::new(),
            confidence: 0.0,
            last_updated_nanos: now,
        }
    }

    /// 找到指定 statement 的事实。
    pub fn find(&self, statement: &str) -> Option<&PortraitFact> {
        self.facts.iter().find(|f| f.statement == statement)
    }

    pub fn find_mut(&mut self, statement: &str) -> Option<&mut PortraitFact> {
        self.facts.iter_mut().find(|f| f.statement == statement)
    }

    /// 插入新事实，返回是否新增（重复 statement 则不新增）。
    /// Insert a fact, merging with a near-duplicate instead of adding one.
    ///
    /// Exact-string dedup meant "我住在杭州" and "我住在 杭州。" were two facts,
    /// so the portrait filled up with rephrasings of the same thing (round5
    /// P0-5 (1): 185 facts, median length 42 chars). A similarity match keeps
    /// the higher-confidence wording and the union of evidence.
    pub fn insert_fact(&mut self, fact: PortraitFact) -> bool {
        if let Some(existing) = self
            .facts
            .iter_mut()
            // 0.7, not 0.6: "我有一只猫" vs "我有一只狗" scores exactly 0.60 on
            // bigram Jaccard (six of ten tokens are shared function words), and
            // at 0.6 the dog was silently never stored (review A3). Genuine
            // detail gains ("我住在杭州" → "我住在杭州，喜欢龙井") reach 0.8 via
            // the containment term, so they still merge.
            .find(|f| lexical_compatibility(&f.statement, &fact.statement) >= 0.7)
        {
            // Keep the more specific wording, never the opposite of it: an
            // overwrite is only allowed when the new statement ADDS detail
            // (the old one is contained in it). Otherwise the longer of the
            // two wins and the shorter is kept as evidence of the earlier
            // wording (review A1: a merge must not delete what the user said).
            let new_is_richer = tokenize_for_similarity(&fact.statement)
                .is_superset(&tokenize_for_similarity(&existing.statement));
            if new_is_richer || fact.confidence > existing.confidence + 0.15 {
                if !new_is_richer
                    && existing.statement.chars().count() > fact.statement.chars().count()
                {
                    // Different wording at similar confidence: keep both.
                    let mut kept = fact.clone();
                    kept.confidence = kept.confidence.min(existing.confidence);
                    let statement = kept.statement.clone();
                    self.facts.push(kept);
                    tracing::debug!("[erme] kept both wordings: {statement}");
                    self.last_updated_nanos = now_nanos();
                    return true;
                }
                existing.statement = fact.statement;
                existing.confidence = fact.confidence;
            }
            for id in fact.supporting_ids {
                if !existing.supporting_ids.contains(&id) {
                    existing.supporting_ids.push(id);
                }
            }
            for id in fact.contradicting_ids {
                if !existing.contradicting_ids.contains(&id) {
                    existing.contradicting_ids.push(id);
                }
            }
            self.last_updated_nanos = now_nanos();
            return false;
        }
        if self.find(&fact.statement).is_some() {
            return false;
        }
        self.last_updated_nanos = now_nanos();
        self.facts.push(fact);
        true
    }

    /// Facts ranked for the CURRENT topic instead of insertion order.
    ///
    /// Injection used to be `facts.iter().take(3)` — the three oldest
    /// statements, regardless of what the user is talking about, and without
    /// any confidence floor. Real data: the three injected facts were three
    /// opening pleasantries ("今天终于出太阳了…") while the user was asking
    /// about a completely different project (round5 P0-5 (2)).
    pub fn rank_relevant(&self, query: &str, k: usize) -> Vec<&PortraitFact> {
        let mut scored: Vec<(f32, &PortraitFact)> = self
            .facts
            .iter()
            .filter(|f| f.confidence >= 0.35)
            .map(|f| {
                let relevance = if query.trim().is_empty() {
                    0.0
                } else {
                    lexical_compatibility(query, &f.statement)
                };
                // Relevance dominates when there IS a query; confidence
                // breaks ties and orders the no-query case.
                (relevance * 0.7 + f.confidence * 0.3, f)
            })
            .collect();
        scored.sort_by(|a, b| b.0.partial_cmp(&a.0).unwrap_or(std::cmp::Ordering::Equal));
        scored.into_iter().take(k).map(|(_, f)| f).collect()
    }

    /// Record an observed trait (topic bucket) so `traits` is no longer a
    /// field nothing ever writes.
    ///
    /// `self_portrait`'s update chain runs through `curiosity_gaps()`, which
    /// reads `user_portrait.traits`; with no writer that chain was dead and
    /// the self-portrait stayed constant since 2026-08-09 (round5 P0-5 (3)).
    pub fn observe_trait(&mut self, name: &str, delta: f32) {
        if name.trim().is_empty() {
            return;
        }
        let entry = self.traits.entry(name.to_string()).or_insert(0.0);
        *entry = (*entry + delta).clamp(0.0, 1.0);
        self.last_updated_nanos = now_nanos();
        self.recompute_confidence();
    }

    /// Facts ranked by relevance, rendered as a single line.
    pub fn summary_line_with_query(&self, query: &str, k: usize) -> String {
        let mut parts: Vec<String> = self
            .traits
            .iter()
            .filter(|(_, c)| **c >= 0.5)
            .map(|(t, _)| t.clone())
            .collect();
        parts.sort();
        for f in self.rank_relevant(query, k) {
            parts.push(f.statement.clone());
        }
        if parts.is_empty() {
            self.summary.clone()
        } else {
            parts.join("；")
        }
    }

    /// 事实被推翻（置信度过低）时移除，返回是否移除。
    pub fn remove_fact(&mut self, statement: &str) -> bool {
        let before = self.facts.len();
        self.facts.retain(|f| f.statement != statement);
        if self.facts.len() != before {
            self.last_updated_nanos = now_nanos();
            true
        } else {
            false
        }
    }

    /// 特质置信度求和（画像总体置信度）。
    pub fn recompute_confidence(&mut self) {
        if self.traits.is_empty() {
            self.confidence = 0.0;
            return;
        }
        self.confidence = self.traits.values().sum::<f32>() / self.traits.len() as f32;
        self.last_updated_nanos = now_nanos();
    }

    /// 人类可读画像摘要（高置信特质 + 代表事实）。
    pub fn summary_line(&self) -> String {
        if self.traits.is_empty() && self.facts.is_empty() {
            return self.summary.clone();
        }
        let mut parts: Vec<String> = self
            .traits
            .iter()
            .filter(|(_, c)| **c >= 0.5)
            .map(|(t, _)| t.clone())
            .collect();
        for f in self.facts.iter().take(3) {
            parts.push(f.statement.clone());
        }
        if parts.is_empty() {
            self.summary.clone()
        } else {
            parts.join("；")
        }
    }
}

impl Default for Portrait {
    fn default() -> Self {
        Self::new()
    }
}

impl Relationship {
    pub fn new(role: impl Into<String>) -> Self {
        Self {
            trust: 0.5,
            intimacy: 0.5,
            boundary: 0.8,
            role: role.into(),
            last_updated_nanos: now_nanos(),
        }
    }

    /// 用户陈述交互后更新关系（简单启发式）。
    pub fn update(&mut self, trust_delta: f32, intimacy_delta: f32) {
        self.trust = (self.trust + trust_delta).clamp(0.0, 1.0);
        self.intimacy = (self.intimacy + intimacy_delta).clamp(0.0, 1.0);
        self.last_updated_nanos = now_nanos();
    }
}

impl Default for Relationship {
    fn default() -> Self {
        Self::new("协作者")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fact(statement: &str, confidence: f32) -> PortraitFact {
        PortraitFact {
            statement: statement.into(),
            confidence,
            supporting_ids: Vec::new(),
            contradicting_ids: Vec::new(),
        }
    }

    #[test]
    fn test_insert_and_dedup() {
        let mut p = Portrait::new();
        assert!(p.insert_fact(fact("喜欢咖啡", 0.7)));
        assert!(!p.insert_fact(fact("喜欢咖啡", 0.9)));
        assert_eq!(p.facts.len(), 1);
        // Restating the same fact with stronger evidence raises its
        // confidence instead of being ignored (round5 P0-5 (1): the old
        // behavior kept the first, arbitrary value forever).
        assert_eq!(p.facts[0].confidence, 0.9);
    }

    #[test]
    fn test_remove_fact() {
        let mut p = Portrait::new();
        p.insert_fact(fact("喜欢咖啡", 0.7));
        assert!(p.remove_fact("喜欢咖啡"));
        assert!(!p.remove_fact("喜欢咖啡"));
        assert!(p.facts.is_empty());
    }

    #[test]
    fn test_recompute_confidence() {
        let mut p = Portrait::new();
        p.traits.insert("理性".into(), 0.8);
        p.traits.insert("好奇".into(), 0.6);
        p.recompute_confidence();
        assert!((p.confidence - 0.7).abs() < 1e-6);
    }

    #[test]
    fn test_summary_line_filters_low_confidence() {
        let mut p = Portrait::new();
        p.traits.insert("理性".into(), 0.9);
        p.traits.insert("冲动".into(), 0.1);
        p.insert_fact(fact("喜欢咖啡", 0.7));
        let line = p.summary_line();
        assert!(line.contains("理性"));
        assert!(!line.contains("冲动"));
        assert!(line.contains("喜欢咖啡"));
    }

    #[test]
    fn test_relationship_update() {
        let mut r = Relationship::new("协作者");
        r.update(0.1, 0.2);
        assert!((r.trust - 0.6).abs() < 1e-6);
        assert!((r.intimacy - 0.7).abs() < 1e-6);
        assert_eq!(r.boundary, 0.8);
    }

    #[test]
    fn test_serde_roundtrip() {
        let mut p = Portrait::new();
        p.insert_fact(fact("喜欢咖啡", 0.7));
        let json = serde_json::to_string(&p).unwrap();
        let back: Portrait = serde_json::from_str(&json).unwrap();
        assert_eq!(back, p);
    }

    /// round5 P0-5 (5) regression: the old tokenizer split on
    /// `!is_alphanumeric()`, and every Han character IS alphanumeric — so a
    /// whole Chinese sentence was one token and any two different Chinese
    /// sentences scored 0.0. Contradiction detection and confidence
    /// promotion/demotion were therefore dead for Chinese input.
    #[test]
    fn chinese_sentences_are_lexically_comparable() {
        let a = "我住在杭州，喜欢喝龙井茶";
        let b = "我住在北京，喜欢喝龙井茶";
        let sim = lexical_compatibility(a, b);
        assert!(
            sim > 0.3,
            "two rephrasings of the same fact must be comparable, got {sim}"
        );
        let unrelated = lexical_compatibility(a, "帮我下载一个大模型并做基准测试");
        assert!(
            sim > unrelated,
            "same-topic similarity ({sim}) must exceed unrelated ({unrelated})"
        );
        // English keeps working.
        assert!(lexical_compatibility("prefers rust", "prefers rust lang") > 0.3);
    }

    /// round5 P0-5 (1): exact-string dedup meant rephrasings became separate
    /// facts, so the portrait filled up with duplicates.
    #[test]
    fn insert_fact_merges_rephrasings() {
        let mut p = Portrait::new();
        let mk = |s: &str, c: f32| PortraitFact {
            statement: s.into(),
            confidence: c,
            supporting_ids: vec![],
            contradicting_ids: vec![],
        };
        assert!(p.insert_fact(mk("我住在杭州", 0.5)));
        assert!(
            !p.insert_fact(mk("我住在杭州，喜欢龙井", 0.8)),
            "a rephrasing must merge, not add a second fact"
        );
        assert_eq!(p.facts.len(), 1);
        assert_eq!(p.facts[0].confidence, 0.8, "the stronger wording wins");
        assert!(p.insert_fact(mk("我讨厌加班", 0.6)));
        assert_eq!(p.facts.len(), 2);
    }

    /// round5 P0-5 (2): injection was `facts.iter().take(3)` — the three
    /// OLDEST facts, regardless of topic, with no confidence floor.
    #[test]
    fn rank_relevant_prefers_the_current_topic() {
        let mut p = Portrait::new();
        for (s, c) in [
            ("今天终于出太阳了", 0.9),
            ("早上出门转了转，买了做饭的菜", 0.9),
            ("正在做 Krea-2 LoRA 训练", 0.7),
            ("吉林省党建档案需要整理材料", 0.7),
        ] {
            p.facts.push(PortraitFact {
                statement: s.into(),
                confidence: c,
                supporting_ids: vec![],
                contradicting_ids: vec![],
            });
        }
        let top = p.rank_relevant("Krea-2 LoRA 训练到第几步了", 1);
        assert_eq!(top.len(), 1);
        assert_eq!(
            top[0].statement, "正在做 Krea-2 LoRA 训练",
            "the on-topic fact must win over older high-confidence ones"
        );
        // Confidence floor: a low-confidence fact is never injected.
        p.facts.push(PortraitFact {
            statement: "可能喜欢蓝色".to_string(),
            confidence: 0.1,
            supporting_ids: vec![],
            contradicting_ids: vec![],
        });
        let all = p.rank_relevant("", 10);
        assert!(
            all.iter().all(|f| f.confidence >= 0.35),
            "facts below the confidence floor must not be injected"
        );
    }

    /// round5 P0-5 (3): `traits` had no writer, so `curiosity_gaps()` (and
    /// with it the self-portrait update chain) could never fire.
    #[test]
    fn observe_trait_writes_and_recomputes() {
        let mut p = Portrait::new();
        assert!(p.traits.is_empty());
        p.observe_trait("技术", 0.6);
        assert_eq!(p.traits.get("技术").copied().unwrap_or(0.0), 0.6);
        p.observe_trait("技术", 0.9); // clamped
        assert_eq!(p.traits.get("技术").copied().unwrap_or(0.0), 1.0);
        assert!(p.confidence > 0.0, "confidence follows the traits");
        assert!(p.last_updated_nanos > 0);
    }
}
