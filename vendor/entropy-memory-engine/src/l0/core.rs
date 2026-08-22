use std::collections::HashMap;

use serde::{Deserialize, Serialize};

use crate::core::now_nanos;

/// 未命名灵魂的占位身份（`SoulCore::default` 初始值）。
pub const DEFAULT_IDENTITY: &str = "未命名的记忆体";

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

    /// 是否仍停留在未命名占位（含空串）。
    pub fn is_unnamed(&self) -> bool {
        self.identity.is_empty() || self.identity == DEFAULT_IDENTITY
    }

    /// 由诞生时刻派生稳定名字（UTC 日期，细粒度到天即可）。
    pub fn birth_name(&self) -> String {
        format!("记忆体 · 醒于 {}", format_birth_date(self.created_at_nanos))
    }

    /// 未命名时用诞生时刻命名，避免身份锚点长期停留在占位符。
    pub fn name_if_unnamed(&mut self) {
        if self.is_unnamed() {
            self.identity = self.birth_name();
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

/// 从 Unix 纪元纳秒派生 UTC 公历日期（civil-from-days 算法，无第三方依赖；
/// 名字只需「哪天醒来」，不需要时区精度）。
fn format_birth_date(nanos: i64) -> String {
    let days = nanos.div_euclid(86_400_000_000_000);
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if m <= 2 { y + 1 } else { y };
    format!("{y:04}-{m:02}-{d:02}")
}

impl Default for SoulCore {
    fn default() -> Self {
        Self {
            identity: DEFAULT_IDENTITY.into(),
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
