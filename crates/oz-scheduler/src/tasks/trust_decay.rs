//! Trust decay task — degrades workspace trust entries that have been inactive.

use std::path::PathBuf;
use std::time::Duration;

use crate::task::{ScheduledTask, TaskContext, TaskError};

pub struct TrustDecay {
    pub max_inactive_days: i64,
    pub interval_secs: u64,
}

impl Default for TrustDecay {
    fn default() -> Self {
        TrustDecay {
            max_inactive_days: 30,
            interval_secs: 3600, // 1 hour
        }
    }
}

#[async_trait::async_trait]
impl ScheduledTask for TrustDecay {
    fn name(&self) -> &str {
        "trust_decay"
    }

    fn interval(&self) -> Duration {
        Duration::from_secs(self.interval_secs)
    }

    async fn execute(&self, ctx: &TaskContext) -> Result<(), TaskError> {
        let working_dir = ctx.working_dir.as_deref().map(PathBuf::from);

        // The runtime reads PER-PROJECT trust stores
        // ({project_root}/openzen/trust.json); decaying only the
        // data-dir file never touched an actual decision. Decay every
        // registered project's store plus the data-dir one.
        let mut targets: Vec<PathBuf> = Vec::new();
        if let Some(ref wd) = working_dir {
            // projects.json lives in the data root next to sessions.
            if let Ok(raw) = std::fs::read_to_string(wd.join("projects.json")) {
                if let Ok(list) = serde_json::from_str::<serde_json::Value>(&raw) {
                    if let Some(arr) = list.as_array() {
                        for p in arr {
                            if let Some(root) = p.get("root_path").and_then(|v| v.as_str()) {
                                targets
                                    .push(PathBuf::from(root).join("openzen").join("trust.json"));
                            }
                        }
                    }
                }
            }
            targets.push(wd.join("openzen").join("trust.json"));
        }
        if let Some(tp) = ctx.trust_path.as_ref().map(PathBuf::from) {
            targets.push(tp);
        }

        let mut decayed = 0usize;
        for path in targets {
            if !path.exists() {
                continue;
            }
            let store = oz_safety::TrustStore::new(Some(path));
            store.decay_expired(self.max_inactive_days);
            decayed += 1;
        }

        tracing::info!(
            "[scheduler] trust_decay: scanned {decayed} trust store(s), degraded entries inactive > {} days",
            self.max_inactive_days
        );

        Ok(())
    }
}
