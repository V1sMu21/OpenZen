use std::collections::HashMap;

use serde::{Deserialize, Serialize};

use crate::core::now_nanos;

/// 核心良知：几乎不变。对应「无善无恶心之体」。
///
/// `SoulCore` 承载身份锚点、原则与价值观权重。它的更新速率极慢——
/// 只有跨越多轮记忆流的强证据才能改写原则（见 `ReflectionEngine`）。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct SoulCore {
    /// 身份锚点："我是什么"
    pub identity: String,
    /// 原则列表（不可轻易改写）
    pub principles: Vec<String>,
    /// 价值观 → 权重 [0.0, 1.0]
    pub value_weights: HashMap<String, f32>,
    /// 创建时间（纳秒）
    pub created_at_nanos: i64,
}

impl SoulCore {
    pub fn new(identity: impl Into<String>) -> Self {
        Self {
            identity: identity.into(),
            principles: Vec::new(),
            value_weights: HashMap::new(),
            created_at_nanos: now_nanos(),
        }
    }

    /// 顶层的良知原则，用于注入提示词。
    pub fn principle_summary(&self) -> String {
        if self.principles.is_empty() {
            self.identity.clone()
        } else {
            self.principles.join("; ")
        }
    }
}

impl Default for SoulCore {
    fn default() -> Self {
        Self {
            identity: "未命名的记忆体".into(),
            principles: vec!["知行合一".into(), "诚实优先".into()],
            value_weights: HashMap::from([
                ("truth".into(), 0.8_f32),
                ("curiosity".into(), 0.7_f32),
                ("autonomy".into(), 0.5_f32),
            ]),
            created_at_nanos: now_nanos(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_new_core_has_identity() {
        let core = SoulCore::new("测试体");
        assert_eq!(core.identity, "测试体");
        assert!(core.created_at_nanos > 0);
    }

    #[test]
    fn test_default_has_principles() {
        let core = SoulCore::default();
        assert!(!core.principles.is_empty());
        assert!(core.principle_summary().contains("知行合一"));
    }

    #[test]
    fn test_principle_summary_falls_back_to_identity() {
        let core = SoulCore {
            principles: Vec::new(),
            identity: "无名".into(),
            ..Default::default()
        };
        assert_eq!(core.principle_summary(), "无名");
    }

    #[test]
    fn test_serde_roundtrip() {
        let core = SoulCore::new("往返测试");
        let json = serde_json::to_string(&core).unwrap();
        let back: SoulCore = serde_json::from_str(&json).unwrap();
        assert_eq!(back, core);
    }
}
