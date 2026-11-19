use std::sync::Arc;

use crate::core::now_nanos;
use crate::l0::soul::{SoulHandle, SoulModel};

fn truncate_chars(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        return s.to_string();
    }
    let mut out: String = s.chars().take(max.saturating_sub(1)).collect();
    out.push('…');
    out
}

/// 提示词注入器：把灵魂模型组装成系统前缀。
///
/// P4 升级：四段式前缀
///   1. 身份与情境   —— SoulCore.identity + SoulState（我在哪、状态如何）
///   2. 生命叙事     —— LifeNarrative 最新章节摘要 + 当前弧线
///   3. 画像与关系   —— 自画像 / 用户画像 / 信任-亲密边界
///   4. 准则与指令   —— 良知原则 + 行为约束（注入位置：系统提示）
pub struct PromptInjector {
    soul: SoulHandle,
}

impl PromptInjector {
    pub fn new(soul: SoulHandle) -> Self {
        Self { soul }
    }

    /// 组装四段式系统前缀。
    pub fn build_system_prefix(&self) -> String {
        self.build_system_prefix_with_query("")
    }

    /// Same, but the portrait segment is ranked against the CURRENT topic.
    ///
    /// Without a query the portrait injected the three OLDEST facts — real
    /// data showed three opening pleasantries being injected while the user
    /// was asking about an unrelated project (round5 P0-5 (2)).
    pub fn build_system_prefix_with_query(&self, query: &str) -> String {
        let model = self.soul.read().unwrap();
        format!(
            "You are an AI with a persistent soul-model. Here is your current self-state:\n\n{}\n\n{}\n\n{}\n\n{}\n\n",
            segment_identity(&model),
            segment_narrative(&model),
            segment_portrait(&model, query),
            segment_directives(&model),
        )
    }

    /// 替换内部持有的灵魂模型（进程内升级用）。
    pub fn update_model(&self, new_soul: SoulHandle) {
        let data = new_soul.read().unwrap().clone();
        let mut current = self.soul.write().unwrap();
        *current = data;
    }

    pub fn soul(&self) -> SoulHandle {
        Arc::clone(&self.soul)
    }
}

/// 段1：身份与情境
fn segment_identity(model: &SoulModel) -> String {
    let mut parts = vec![format!(
        "[Identity] {}",
        truncate_chars(&model.core.identity, 40)
    )];
    // A focus older than a week is stale context, not current work.
    const FOCUS_TTL_NANOS: i64 = 7 * 24 * 3600 * 1_000_000_000;
    let focus_fresh = model.state.last_updated_nanos > 0
        && now_nanos().saturating_sub(model.state.last_updated_nanos) <= FOCUS_TTL_NANOS;
    if !model.state.focus.is_empty() && focus_fresh {
        parts.push(format!(
            "[Focus] {}",
            truncate_chars(&model.state.focus, 80)
        ));
    }
    parts.push(format!(
        "[State] mood={} energy={:.2} confidence={:.2}",
        model.state.mood, model.state.energy, model.state.confidence
    ));
    parts.join("\n")
}

/// 段2：生命叙事
fn segment_narrative(model: &SoulModel) -> String {
    let mut parts = vec!["[Narrative]".to_string()];
    let latest = model.narrative.latest_summary();
    if !latest.is_empty() {
        parts.push(format!("latest: {}", truncate_chars(latest, 300)));
    }
    if !model.narrative.current_arc.is_empty() {
        parts.push(format!(
            "arc: {}",
            truncate_chars(&model.narrative.current_arc, 120)
        ));
    }
    if parts.len() == 1 {
        parts.push("(narrative not yet formed)".into());
    }
    parts.join("\n")
}

/// 段3：画像与关系
fn segment_portrait(model: &SoulModel, query: &str) -> String {
    const PORTRAIT_FACTS: usize = 2;
    const FACT_CHARS: usize = 80;
    let mut parts = vec!["[Portrait]".to_string()];
    let self_line = model
        .self_portrait
        .summary_line_bounded(PORTRAIT_FACTS, FACT_CHARS);
    if !self_line.is_empty() {
        parts.push(format!("self: {}", self_line));
    }
    let user_line =
        model
            .user_portrait
            .summary_line_with_query_bounded(query, PORTRAIT_FACTS, FACT_CHARS);
    if !user_line.is_empty() {
        parts.push(format!("user: {}", user_line));
    }
    parts.push(format!(
        "relationship: role={} trust={:.2} intimacy={:.2} boundary={:.2}",
        model.relationship.role,
        model.relationship.trust,
        model.relationship.intimacy,
        model.relationship.boundary,
    ));
    parts.join("\n")
}

/// 段4：准则与指令
fn segment_directives(model: &SoulModel) -> String {
    let principles = model.core.principle_summary();
    format!(
        "[Directives]\nprinciples: {}\nconstraints: 知行合一; 诚实优先; 不确定时明说; 尊重边界.",
        principles
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::RwLock;

    fn handle_with(identity: &str) -> SoulHandle {
        let mut model = SoulModel::new();
        model.core.identity = identity.into();
        model.state.update("平静", "rust", 0.8, 0.7);
        model.user_portrait.insert_fact(crate::l0::PortraitFact {
            statement: "prefers rust".into(),
            confidence: 0.8,
            supporting_ids: vec![1],
            contradicting_ids: vec![],
        });
        Arc::new(RwLock::new(model))
    }

    #[test]
    fn test_build_prefix_contains_identity() {
        let injector = PromptInjector::new(handle_with("test agent"));
        let prefix = injector.build_system_prefix();
        assert!(prefix.contains("test agent"));
        assert!(prefix.contains("[Identity]"));
        assert!(prefix.contains("[Narrative]"));
        assert!(prefix.contains("[Portrait]"));
        assert!(prefix.contains("[Directives]"));
        assert!(prefix.contains("prefers rust"));
    }

    #[test]
    fn test_update_model_swaps_soul() {
        let injector = PromptInjector::new(handle_with("old agent"));
        injector.update_model(handle_with("new agent"));
        let prefix = injector.build_system_prefix();
        assert!(prefix.contains("new agent"));
        assert!(!prefix.contains("old agent"));
    }
}

#[cfg(test)]
mod budget_tests {
    use super::*;
    use crate::l0::portrait::{PortraitFact, Relationship};
    use std::sync::{Arc, RwLock};

    fn fact(s: &str, c: f32) -> PortraitFact {
        PortraitFact {
            statement: s.into(),
            confidence: c,
            supporting_ids: vec![],
            contradicting_ids: vec![],
        }
    }

    #[test]
    fn prefix_never_contains_graph_ids_and_stays_bounded() {
        let mut soul = SoulModel::new();
        soul.core.identity = "尔雅".into();
        for i in 0..6 {
            soul.self_portrait.facts.push(fact(
                &format!("Node {i} and node {} may be related", i + 1),
                0.7,
            ));
            soul.user_portrait.facts.push(fact(
                &format!("用户偏好第 {i} 条很长的描述文字，用来测试预算"),
                0.8,
            ));
        }
        soul.narrative.chapters.clear();
        let handle = Arc::new(RwLock::new(soul));
        let prefix = PromptInjector::new(handle).build_system_prefix_with_query("测试");
        assert!(
            !prefix.contains("Node "),
            "graph ids must never be injected"
        );
        assert!(
            prefix.chars().count() <= 600,
            "prefix too long: {}",
            prefix.chars().count()
        );
    }

    #[test]
    fn stale_focus_is_not_injected() {
        let mut soul = SoulModel::new();
        soul.state.focus = "旧的 SOP".into();
        soul.state.last_updated_nanos = 1;
        let handle = Arc::new(RwLock::new(soul));
        let prefix = PromptInjector::new(handle).build_system_prefix();
        assert!(!prefix.contains("[Focus]"));
    }

    #[test]
    fn relationship_default_does_not_break_serde() {
        let r = Relationship::default();
        assert!(r.trust >= 0.0);
    }
}
