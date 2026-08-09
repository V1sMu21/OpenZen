use serde::{Deserialize, Serialize};

use crate::core::now_nanos;

/// 情境状态：易逝。对应「有善有恶意之动」
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct SoulState {
    /// 当前情绪基调（枚举或自由文本）
    pub mood: String,
    /// 当前关注焦点
    pub focus: String,
    /// 能量水平 0.0 ~ 1.0
    pub energy: f32,
    /// 整体自信度 0.0 ~ 1.0
    pub confidence: f32,
    pub last_updated_nanos: i64,
}

impl SoulState {
    pub fn new() -> Self {
        let now = now_nanos();
        Self {
            mood: "平静".into(),
            focus: String::new(),
            energy: 0.8,
            confidence: 0.5,
            last_updated_nanos: now,
        }
    }

    /// 由外部事件刷新情境状态（快变层）。
    pub fn update(
        &mut self,
        mood: impl Into<String>,
        focus: impl Into<String>,
        energy: f32,
        confidence: f32,
    ) {
        self.mood = mood.into();
        self.focus = focus.into();
        self.energy = energy.clamp(0.0, 1.0);
        self.confidence = confidence.clamp(0.0, 1.0);
        self.last_updated_nanos = now_nanos();
    }
}

impl Default for SoulState {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_new_state_defaults() {
        let s = SoulState::new();
        assert_eq!(s.mood, "平静");
        assert!(s.energy > 0.0 && s.energy <= 1.0);
        assert!(s.last_updated_nanos > 0);
    }

    #[test]
    fn test_update_clamps_ranges() {
        let mut s = SoulState::new();
        s.update("专注", "rust", 1.5, -0.5);
        assert_eq!(s.mood, "专注");
        assert_eq!(s.focus, "rust");
        assert_eq!(s.energy, 1.0);
        assert_eq!(s.confidence, 0.0);
    }

    #[test]
    fn test_serde_roundtrip() {
        let s = SoulState::new();
        let json = serde_json::to_string(&s).unwrap();
        let back: SoulState = serde_json::from_str(&json).unwrap();
        assert_eq!(back, s);
    }
}
