//! Per-language/per-task thresholds. The numbers are starting points, not
//! truth: `startlux-decide eval` (or the OpenZen labeled set) must replace
//! them for a project before auto-actions are enabled.

use std::collections::HashMap;

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Thresholds {
    pub none_probability: f32,
    pub auto_action: f32,
    pub min_confidence: f32,
}

impl Thresholds {
    pub const fn new(none_probability: f32, auto_action: f32, min_confidence: f32) -> Self {
        Self {
            none_probability,
            auto_action,
            min_confidence,
        }
    }

    pub fn can_auto_act(&self, confidence: f32) -> bool {
        confidence >= self.auto_action
    }
}

#[derive(Debug, Clone)]
pub struct CalibrationTable {
    default: Thresholds,
    by_language: HashMap<String, Thresholds>,
}

impl Default for CalibrationTable {
    fn default() -> Self {
        let mut by_language = HashMap::new();
        // English has the official Decision Index numbers; Chinese does not.
        // Chinese starts stricter so a bad answer is more likely to abstain.
        by_language.insert("en".into(), Thresholds::new(0.20, 0.85, 0.75));
        by_language.insert("zh".into(), Thresholds::new(0.25, 0.90, 0.80));
        Self {
            default: Thresholds::new(0.20, 0.85, 0.75),
            by_language,
        }
    }
}

impl CalibrationTable {
    pub fn for_language(&self, lang: &str) -> Thresholds {
        let lang = lang.trim().to_ascii_lowercase();
        self.by_language
            .get(&lang)
            .copied()
            .or_else(|| {
                // Accept "zh-CN"/"en-US" and other region tags.
                self.by_language
                    .iter()
                    .find(|(k, _)| lang.starts_with(k.as_str()))
                    .map(|(_, v)| *v)
            })
            .unwrap_or(self.default)
    }

    pub fn insert(&mut self, lang: impl Into<String>, thresholds: Thresholds) {
        self.by_language
            .insert(lang.into().trim().to_ascii_lowercase(), thresholds);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn region_tags_fall_back_to_language() {
        let t = CalibrationTable::default();
        assert_eq!(t.for_language("zh-CN"), t.for_language("zh"));
        assert_eq!(t.for_language("en-US"), t.for_language("en"));
        assert_eq!(t.for_language("fr"), t.for_language("en"));
    }

    #[test]
    fn chinese_is_stricter_than_english() {
        let t = CalibrationTable::default();
        assert!(t.for_language("zh").none_probability > t.for_language("en").none_probability);
        assert!(t.for_language("zh").auto_action > t.for_language("en").auto_action);
    }

    #[test]
    fn auto_action_requires_confidence() {
        let th = Thresholds::new(0.2, 0.85, 0.7);
        assert!(th.can_auto_act(0.85));
        assert!(!th.can_auto_act(0.84));
    }
}
