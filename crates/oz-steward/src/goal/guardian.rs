//! Goal guardian: turns goal state + gate evidence into a Steward verdict.
//! It never mutates goal state; the runtime owns completion.

use std::sync::Arc;

use serde::Serialize;

use oz_core_types::supervision::Verdict;

use crate::error::Result;

use super::spec::GoalStatus;
use super::store::GoalStore;

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct GoalGuardianReport {
    pub goal_id: String,
    pub status: GoalStatus,
    pub verdict: Verdict,
    pub evidence_ids: Vec<String>,
    pub summary: String,
    pub next_actions: Vec<String>,
}

pub struct GoalGuardian {
    store: Arc<GoalStore>,
}

impl GoalGuardian {
    pub fn new(store: Arc<GoalStore>) -> Self {
        Self { store }
    }

    pub fn inspect(&self, goal_id: &str) -> Result<Option<GoalGuardianReport>> {
        let Some(goal) = self.store.get(goal_id)? else {
            return Ok(None);
        };
        let attempt = self.store.attempts(goal_id, 1)?.into_iter().next();
        let evidence_ids = attempt
            .as_ref()
            .map(|a| a.evidence_ids.clone())
            .unwrap_or_default();
        let verdict = match goal.status {
            GoalStatus::Achieved => Verdict::OnTrack,
            GoalStatus::Abandoned | GoalStatus::Exhausted => Verdict::OffTrack,
            GoalStatus::Blocked => Verdict::Blocked,
            GoalStatus::Verifying => Verdict::Stalled,
            GoalStatus::Active | GoalStatus::Paused => {
                if attempt
                    .as_ref()
                    .map(|a| a.outcome == "failed")
                    .unwrap_or(false)
                {
                    Verdict::AtRisk
                } else {
                    Verdict::OnTrack
                }
            }
            GoalStatus::Draft => Verdict::AtRisk,
        };
        let summary = match (&goal.status, &attempt) {
            (GoalStatus::Achieved, _) => "goal achieved with gate evidence".into(),
            (GoalStatus::Verifying, Some(a)) => {
                format!(
                    "goal is waiting for human/review acceptance after attempt {}",
                    a.attempt_no
                )
            }
            (GoalStatus::Blocked, Some(a)) => format!(
                "goal blocked at attempt {}: {}",
                a.attempt_no,
                a.failure_class.as_deref().unwrap_or("unknown")
            ),
            (_, Some(a)) => format!("attempt {} outcome={}", a.attempt_no, a.outcome),
            (_, None) => "goal has no recorded attempt yet".into(),
        };
        Ok(Some(GoalGuardianReport {
            goal_id: goal.id,
            status: goal.status,
            verdict,
            evidence_ids,
            summary,
            next_actions: match goal.status {
                GoalStatus::Verifying => vec!["Accept or reject the gate evidence.".into()],
                GoalStatus::Blocked => vec!["Resolve the blocker, then resume.".into()],
                GoalStatus::Exhausted => vec!["Create a new version with more budget.".into()],
                _ => vec!["Continue the bounded attempt loop.".into()],
            },
        }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::goal::spec::{GoalAttempt, GoalGate, GoalSpec};

    #[test]
    fn guardian_reports_blocked_and_achieved() {
        let store = Arc::new(GoalStore::open_memory().unwrap());
        let mut s = GoalSpec::new("g1", "goal", "obj");
        s.gates = vec![GoalGate::Human {
            prompt: "ok".into(),
        }];
        store.create(s).unwrap();
        let g = GoalGuardian::new(store.clone());
        assert_eq!(g.inspect("g1").unwrap().unwrap().verdict, Verdict::AtRisk);
        store.set_status("g1", GoalStatus::Active, "start").unwrap();
        store
            .set_status("g1", GoalStatus::Blocked, "blocked")
            .unwrap();
        let report = g.inspect("g1").unwrap().unwrap();
        assert_eq!(report.verdict, Verdict::Blocked);
        assert_eq!(report.status, GoalStatus::Blocked);
        assert!(report.evidence_ids.is_empty());
        // An achieved goal is on_track and its evidence is stable.
        let store2 = Arc::new(GoalStore::open_memory().unwrap());
        let mut s2 = GoalSpec::new("g2", "goal", "obj");
        s2.gates = vec![GoalGate::Command {
            command: "true".into(),
            timeout_secs: 5,
            retries: 0,
        }];
        store2.create(s2).unwrap();
        store2
            .record_attempt(&GoalAttempt {
                goal_id: "g2".into(),
                attempt_no: 1,
                started_at: 1,
                ended_at: 2,
                summary: "ok".into(),
                outcome: "passed".into(),
                failure_class: None,
                checkpoint_id: None,
                tokens: 1,
                gates: vec![],
                evidence_ids: vec!["e1".into()],
            })
            .unwrap();
        store2
            .set_status("g2", GoalStatus::Active, "start")
            .unwrap();
        store2
            .set_status("g2", GoalStatus::Verifying, "gates")
            .unwrap();
        store2
            .set_status("g2", GoalStatus::Achieved, "done")
            .unwrap();
        let r = GoalGuardian::new(store2).inspect("g2").unwrap().unwrap();
        assert_eq!(r.verdict, Verdict::OnTrack);
        assert_eq!(r.evidence_ids, vec!["e1".to_string()]);
    }
}
