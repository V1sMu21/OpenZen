use std::sync::Arc;

use crate::l0::soul::{SoulHandle, SoulModel};

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
        let model = self.soul.read().unwrap();
        format!(
            "You are an AI with a persistent soul-model. Here is your current self-state:\n\n{}\n\n{}\n\n{}\n\n{}\n\n",
            segment_identity(&model),
            segment_narrative(&model),
            segment_portrait(&model),
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
    let mut parts = vec![format!("[Identity] {}", model.core.identity)];
    if !model.state.focus.is_empty() {
        parts.push(format!("[Focus] {}", model.state.focus));
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
        parts.push(format!("latest: {}", latest));
    }
    if !model.narrative.current_arc.is_empty() {
        parts.push(format!("arc: {}", model.narrative.current_arc));
    }
    if parts.len() == 1 {
        parts.push("(narrative not yet formed)".into());
    }
    parts.join("\n")
}

/// 段3：画像与关系
fn segment_portrait(model: &SoulModel) -> String {
    let mut parts = vec!["[Portrait]".to_string()];
    let self_line = model.self_portrait.summary_line();
    if !self_line.is_empty() {
        parts.push(format!("self: {}", self_line));
    }
    let user_line = model.user_portrait.summary_line();
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
