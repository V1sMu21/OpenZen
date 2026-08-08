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
        let trust_path = ctx.trust_path.as_ref()
            .map(|p| PathBuf::from(p))
            .or_else(|| {
                ctx.working_dir.as_ref()
                    .map(|wd| PathBuf::from(wd).join("openzen").join("trust.json"))
            });

        let Some(trust_path) = trust_path else {
            return Ok(());
        };

        if !trust_path.exists() {
            return Ok(());
        }

        let store = oz_safety::TrustStore::new(Some(trust_path));
        store.decay_expired(self.max_inactive_days);

        tracing::info!(
            "[scheduler] trust_decay: scanned trust store, degraded entries inactive > {} days",
            self.max_inactive_days
        );

        Ok(())
    }
}
