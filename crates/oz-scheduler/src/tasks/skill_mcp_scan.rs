//! Knowledge scan task — detects and marks stale skills/SOPs.

use std::path::PathBuf;
use std::time::Duration;

use crate::task::{ScheduledTask, TaskContext, TaskError};

pub struct SkillMcpScan {
    pub interval_secs: u64,
}

impl Default for SkillMcpScan {
    fn default() -> Self {
        SkillMcpScan { interval_secs: 21600 } // 6 hours
    }
}

#[async_trait::async_trait]
impl ScheduledTask for SkillMcpScan {
    fn name(&self) -> &str {
        "skill_mcp_scan"
    }

    fn interval(&self) -> Duration {
        Duration::from_secs(self.interval_secs)
    }

    async fn execute(&self, ctx: &TaskContext) -> Result<(), TaskError> {
        let working_dir = ctx.working_dir.as_deref().unwrap_or(".");
        let skill_mcp_dir = ctx.skill_mcp_dir.as_deref().unwrap_or(working_dir);

        let store = oz_skill_mcp::SkillMcpStore::new(
            &PathBuf::from(working_dir),
            Some(PathBuf::from(skill_mcp_dir)),
        );

        // Scan all skills for quality issues
        let skill_count = store.skill_count();
        if skill_count == 0 {
            return Ok(());
        }

        let skills = store.skills.list();
        let mut stale = 0u32;

        for skill in skills {
            if skill.quality < 0.2 {
                stale += 1;
                tracing::debug!(
                    "[scheduler] low-quality skill detected: {} (quality={:.2})",
                    skill.name, skill.quality
                );
            }
        }

        // Scan SOPs
        let sop_count = store.sop_count();
        let now = chrono::Utc::now();
        let threshold = now - chrono::Duration::days(90);
        let mut stale_sops = 0u32;

        if sop_count > 0 {
            let sops = store.sops.all();
            for sop in sops {
                let updated_dt = chrono::DateTime::parse_from_rfc3339(&sop.metadata.updated_at).ok()
                    .map(|d| d.with_timezone(&chrono::Utc));
                if let Some(updated) = updated_dt {
                    if updated < threshold {
                        stale_sops += 1;
                    }
                }
            }
        }

        if stale > 0 || stale_sops > 0 {
            tracing::info!(
                "[scheduler] skill_mcp_scan: {stale} low-quality skills, {stale_sops} stale SOPs out of {skill_count} skills / {sop_count} SOPs"
            );
        }

        Ok(())
    }
}
