use std::collections::HashMap;

use serde::{Deserialize, Serialize};

use crate::core::now_nanos;

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
    pub fn insert_fact(&mut self, fact: PortraitFact) -> bool {
        if self.find(&fact.statement).is_some() {
            return false;
        }
        self.last_updated_nanos = now_nanos();
        self.facts.push(fact);
        true
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
        assert_eq!(p.facts[0].confidence, 0.7);
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
}
