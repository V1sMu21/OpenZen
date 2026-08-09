use std::path::Path;
use std::sync::{Arc, RwLock};

#[derive(Debug, Clone)]
pub enum BehaviorEvent {
    SelfCorrection {
        old_id: u64,
        new_id: u64,
        trigger: String,
    },
    CrossDomainLink {
        domain_a: String,
        domain_b: String,
    },
    IdentityShift {
        field: String,
        old_value: String,
        new_value: String,
    },
    InsightGenerated {
        depth: f32,
        statement: String,
    },
}

pub struct BehaviorObserver {
    event_log: Arc<RwLock<Vec<BehaviorEvent>>>,
}

impl BehaviorObserver {
    pub fn new() -> Self {
        Self {
            event_log: Arc::new(RwLock::new(Vec::new())),
        }
    }

    pub fn record(&self, event: BehaviorEvent) {
        if let Ok(mut log) = self.event_log.write() {
            log.push(event);
        }
    }

    pub fn export_json(&self, path: &Path) -> std::io::Result<()> {
        let log = self.event_log.read().unwrap();
        let events: Vec<serde_json::Value> = log
            .iter()
            .map(|e| match e {
                BehaviorEvent::SelfCorrection {
                    old_id,
                    new_id,
                    trigger,
                } => serde_json::json!({
                    "type": "self_correction",
                    "old_id": old_id,
                    "new_id": new_id,
                    "trigger": trigger,
                }),
                BehaviorEvent::CrossDomainLink { domain_a, domain_b } => {
                    serde_json::json!({
                        "type": "cross_domain_link",
                        "domain_a": domain_a,
                        "domain_b": domain_b,
                    })
                }
                BehaviorEvent::IdentityShift {
                    field,
                    old_value,
                    new_value,
                } => serde_json::json!({
                    "type": "identity_shift",
                    "field": field,
                    "old_value": old_value,
                    "new_value": new_value,
                }),
                BehaviorEvent::InsightGenerated { depth, statement } => {
                    serde_json::json!({
                        "type": "insight_generated",
                        "depth": depth,
                        "statement": statement,
                    })
                }
            })
            .collect();

        let json = serde_json::to_string_pretty(&events)?;
        std::fs::write(path, json)?;
        Ok(())
    }

    pub fn detect_emergence(&self) -> Vec<String> {
        let log = self.event_log.read().unwrap();
        let mut signals = Vec::new();

        let self_corrections: usize = log
            .iter()
            .filter(|e| matches!(e, BehaviorEvent::SelfCorrection { .. }))
            .count();
        let cross_domain: usize = log
            .iter()
            .filter(|e| matches!(e, BehaviorEvent::CrossDomainLink { .. }))
            .count();
        let identity_shifts: usize = log
            .iter()
            .filter(|e| matches!(e, BehaviorEvent::IdentityShift { .. }))
            .count();
        let insights: usize = log
            .iter()
            .filter(|e| matches!(e, BehaviorEvent::InsightGenerated { .. }))
            .count();

        if self_corrections >= 3 {
            signals.push(format!(
                "Emergence signal: {} self-corrections detected (autonomous error fixing)",
                self_corrections
            ));
        }
        if cross_domain >= 2 {
            signals.push(format!(
                "Emergence signal: {} cross-domain links (creative association)",
                cross_domain
            ));
        }
        if identity_shifts >= 1 {
            signals.push(format!(
                "Emergence signal: {} identity shifts (value evolution)",
                identity_shifts
            ));
        }
        if insights >= 5 {
            signals.push(format!(
                "Emergence signal: {} insights generated (autonomous discovery)",
                insights
            ));
        }

        signals
    }

    pub fn event_count(&self) -> usize {
        self.event_log.read().unwrap().len()
    }

    pub fn clear(&self) {
        self.event_log.write().unwrap().clear();
    }
}

impl Default for BehaviorObserver {
    fn default() -> Self {
        Self::new()
    }
}

impl Clone for BehaviorObserver {
    fn clone(&self) -> Self {
        Self {
            event_log: Arc::clone(&self.event_log),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_record_and_count() {
        let obs = BehaviorObserver::new();
        assert_eq!(obs.event_count(), 0);

        obs.record(BehaviorEvent::InsightGenerated {
            depth: 0.8,
            statement: "test insight".into(),
        });
        assert_eq!(obs.event_count(), 1);
    }

    #[test]
    fn test_detect_emergence_self_corrections() {
        let obs = BehaviorObserver::new();
        for i in 0..3 {
            obs.record(BehaviorEvent::SelfCorrection {
                old_id: i,
                new_id: i + 10,
                trigger: "test".into(),
            });
        }
        let signals = obs.detect_emergence();
        assert!(!signals.is_empty());
        assert!(signals[0].contains("self-corrections"));
    }

    #[test]
    fn test_detect_emergence_none() {
        let obs = BehaviorObserver::new();
        let signals = obs.detect_emergence();
        assert!(signals.is_empty());
    }

    #[test]
    fn test_export_json() {
        let obs = BehaviorObserver::new();
        obs.record(BehaviorEvent::CrossDomainLink {
            domain_a: "rust".into(),
            domain_b: "memory".into(),
        });

        let path = std::env::temp_dir().join("test_observer_export.json");
        obs.export_json(&path).unwrap();
        assert!(path.exists());
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn test_clear() {
        let obs = BehaviorObserver::new();
        obs.record(BehaviorEvent::InsightGenerated {
            depth: 1.0,
            statement: "x".into(),
        });
        assert_eq!(obs.event_count(), 1);
        obs.clear();
        assert_eq!(obs.event_count(), 0);
    }
}
