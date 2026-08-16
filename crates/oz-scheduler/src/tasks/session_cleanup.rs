//! Session cleanup task — removes expired idle sessions and archives them.

use std::path::PathBuf;
use std::time::Duration;

use crate::task::{ScheduledTask, TaskContext, TaskError};

pub struct SessionCleanup {
    pub max_idle_days: i64,
    pub interval_secs: u64,
}

impl Default for SessionCleanup {
    fn default() -> Self {
        SessionCleanup {
            max_idle_days: 7,
            interval_secs: 3600, // 1 hour
        }
    }
}

impl SessionCleanup {
    pub fn new(max_idle_days: i64) -> Self {
        SessionCleanup {
            max_idle_days,
            interval_secs: 3600,
        }
    }
}

#[async_trait::async_trait]
impl ScheduledTask for SessionCleanup {
    fn name(&self) -> &str {
        "session_cleanup"
    }

    fn interval(&self) -> Duration {
        Duration::from_secs(self.interval_secs)
    }

    async fn execute(&self, ctx: &TaskContext) -> Result<(), TaskError> {
        // Prefer the in-process pruner when the host app provided one: it
        // owns the authoritative in-memory session map.
        if let Some(pruner) = &ctx.session_pruner {
            let removed = (pruner.0)(self.max_idle_days);
            if removed > 0 {
                tracing::info!(
                    "[scheduler] session_cleanup: pruned {removed} expired sessions (in-process)"
                );
            }
            return Ok(());
        }

        let working_dir = ctx.working_dir.as_deref().unwrap_or(".");
        let sessions_path = PathBuf::from(working_dir)
            .join("openzen")
            .join("sessions.json");

        if !sessions_path.exists() {
            return Ok(());
        }

        let data = std::fs::read_to_string(&sessions_path)
            .map_err(|e| TaskError::Custom(format!("read sessions: {e}")))?;

        let mut sessions: serde_json::Value = serde_json::from_str(&data)
            .map_err(|e| TaskError::Custom(format!("parse sessions: {e}")))?;

        let now = chrono::Utc::now();
        let threshold = now - chrono::Duration::days(self.max_idle_days);
        let mut removed = 0u32;

        if let Some(map) = sessions.as_object_mut() {
            let keys_to_remove: Vec<String> = map
                .iter()
                .filter_map(|(id, sess)| {
                    let created = sess
                        .get("info")
                        .and_then(|i| i.get("created_at"))
                        .and_then(|c| c.as_str())
                        .and_then(|c| chrono::DateTime::parse_from_rfc3339(c).ok());
                    let status = sess
                        .get("status")
                        .and_then(|s| s.as_str())
                        .unwrap_or("idle");
                    // serde serializes SessionStatus variants capitalized
                    // ("Idle"/"Stopped") — the old lowercase compare never
                    // matched, so nothing was ever removed.
                    let is_idle = status.eq_ignore_ascii_case("idle")
                        || status.eq_ignore_ascii_case("stopped");
                    match created {
                        Some(d) if d < threshold && is_idle => Some(id.clone()),
                        _ => None,
                    }
                })
                .collect();

            for id in &keys_to_remove {
                map.remove(id);
                removed += 1;
            }
        }

        if removed > 0 {
            let json = serde_json::to_string_pretty(&sessions)
                .map_err(|e| TaskError::Custom(format!("serialize sessions: {e}")))?;
            std::fs::write(&sessions_path, json)
                .map_err(|e| TaskError::Custom(format!("write sessions: {e}")))?;
            tracing::info!("[scheduler] session_cleanup: removed {removed} expired sessions");
        }

        Ok(())
    }
}
