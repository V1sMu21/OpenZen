use std::sync::Arc;

use serde::Serialize;
use uuid::Uuid;

use oz_core_types::supervision::now_ms;

use crate::error::{Result, StewardError};

use super::spec::{GateResult, GoalAttempt, GoalReport, GoalStatus};
use super::store::GoalStore;

#[derive(Debug, Clone, Copy, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum GoalDecision {
    Continue,
    Verify,
    Achieved,
    Paused,
    Blocked,
    Exhausted,
    Abandoned,
}

#[derive(Debug, Clone)]
pub struct GoalRuntimeConfig {
    pub lease_ttl_ms: i64,
}

impl Default for GoalRuntimeConfig {
    fn default() -> Self {
        Self {
            lease_ttl_ms: 30_000,
        }
    }
}

pub struct GoalRuntime {
    store: Arc<GoalStore>,
    runner_id: String,
    lease_ttl_ms: i64,
}

impl GoalRuntime {
    pub fn new(store: Arc<GoalStore>, runner_id: impl Into<String>) -> Self {
        Self {
            store,
            runner_id: runner_id.into(),
            lease_ttl_ms: 30_000,
        }
    }

    pub fn with_config(mut self, config: GoalRuntimeConfig) -> Self {
        self.lease_ttl_ms = config.lease_ttl_ms.max(1_000);
        self
    }

    pub fn store(&self) -> &Arc<GoalStore> {
        &self.store
    }

    /// Start a draft/paused goal. Fails if another runner holds a live lease.
    pub fn start(&self, goal_id: &str) -> Result<GoalStatus> {
        let goal = self
            .store
            .get(goal_id)?
            .ok_or_else(|| StewardError::Invalid(format!("goal not found: {goal_id}")))?;
        if goal.status.is_terminal() {
            return Err(StewardError::Invalid("goal is terminal".into()));
        }
        if !self
            .store
            .acquire_lease(goal_id, &self.runner_id, self.lease_ttl_ms)?
        {
            return Err(StewardError::Invalid(
                "another runner holds a live goal lease".into(),
            ));
        }
        let g = self
            .store
            .set_status(goal_id, GoalStatus::Active, "started")?;
        if g.wall_started_at.is_none() {
            self.store.update_counters(
                goal_id,
                g.attempts,
                g.consecutive_failures,
                g.no_progress_rounds,
                g.tokens_used,
                Some(now_ms()),
            )?;
        }
        Ok(GoalStatus::Active)
    }

    pub fn heartbeat(&self, goal_id: &str) -> Result<bool> {
        self.store
            .heartbeat_lease(goal_id, &self.runner_id, self.lease_ttl_ms)
    }

    pub fn pause(&self, goal_id: &str, reason: &str) -> Result<GoalStatus> {
        self.store
            .set_status(goal_id, GoalStatus::Paused, reason)
            .map(|g| g.status)
    }

    pub fn stop(&self, goal_id: &str, reason: &str) -> Result<GoalStatus> {
        let status = self
            .store
            .set_status(goal_id, GoalStatus::Abandoned, reason)?
            .status;
        let _ = self.store.release_lease(goal_id, &self.runner_id);
        Ok(status)
    }

    /// Record one bounded attempt and decide what the runner should do next.
    /// Gate results must come from the runtime, never from the model.
    pub async fn complete_attempt(
        &self,
        goal_id: &str,
        summary: &str,
        tokens: u64,
        checkpoint_id: Option<String>,
        gates: Vec<GateResult>,
    ) -> Result<GoalDecision> {
        if !self.heartbeat(goal_id)? {
            return Err(StewardError::Invalid(
                "goal lease lost; refusing a second runner".into(),
            ));
        }
        let goal = self
            .store
            .get(goal_id)?
            .ok_or_else(|| StewardError::Invalid(format!("goal not found: {goal_id}")))?;
        if !matches!(goal.status, GoalStatus::Active | GoalStatus::Verifying) {
            return Err(StewardError::Invalid(format!(
                "goal is not runnable: {:?}",
                goal.status
            )));
        }
        let now = now_ms();
        let attempt_no = goal.attempts + 1;
        let pending = gates.iter().any(|g| g.pending);
        let all_passed = !gates.is_empty() && gates.iter().all(|g| g.passed) && !pending;
        let failure_class = if all_passed {
            None
        } else if pending {
            Some("needs_human".to_string())
        } else {
            Some("gate_failed".to_string())
        };
        let evidence_ids = gates
            .iter()
            .map(|g| g.evidence_id.clone())
            .collect::<Vec<_>>();
        let attempt = GoalAttempt {
            goal_id: goal_id.into(),
            attempt_no,
            started_at: goal.wall_started_at.unwrap_or(now),
            ended_at: now,
            summary: summary.into(),
            outcome: if all_passed {
                "passed".into()
            } else if pending {
                "pending".into()
            } else {
                "failed".into()
            },
            failure_class: failure_class.clone(),
            checkpoint_id,
            tokens,
            gates: gates.clone(),
            evidence_ids,
        };
        self.store.record_attempt(&attempt)?;

        let tokens_used = goal.tokens_used.saturating_add(tokens);
        let wall_started_at = goal.wall_started_at.unwrap_or(now);
        let wall_exhausted =
            now.saturating_sub(wall_started_at) >= goal.spec.budget.max_wall_secs as i64 * 1_000;
        let token_exhausted = tokens_used > goal.spec.budget.max_tokens;
        let attempts_exhausted = attempt_no >= goal.spec.budget.max_attempts;

        let decision = if all_passed {
            // Command/file gates passed. A human/review gate is recorded as
            // pending, so it reaches verifying and stops; otherwise the goal
            // is achieved by the runtime, not by the model.
            let needs_human = goal.spec.require_human_acceptance
                || goal
                    .spec
                    .gates
                    .iter()
                    .any(|g| g.has_human() || g.has_review());
            self.store.update_counters(
                goal_id,
                attempt_no,
                goal.consecutive_failures,
                goal.no_progress_rounds,
                tokens_used,
                Some(wall_started_at),
            )?;
            self.store
                .set_status(goal_id, GoalStatus::Verifying, "gates passed")?;
            if needs_human {
                self.store.set_status(
                    goal_id,
                    GoalStatus::Verifying,
                    "gates passed; acceptance required",
                )?;
                GoalDecision::Verify
            } else {
                self.store
                    .set_status(goal_id, GoalStatus::Achieved, "all gates passed")?;
                let _ = self.store.release_lease(goal_id, &self.runner_id);
                self.write_report(goal_id, GoalStatus::Achieved, "all gates passed", &gates)?;
                GoalDecision::Achieved
            }
        } else if pending {
            self.store
                .set_status(goal_id, GoalStatus::Verifying, "human/review gate pending")?;
            GoalDecision::Verify
        } else if wall_exhausted || token_exhausted || attempts_exhausted {
            self.store
                .set_status(goal_id, GoalStatus::Exhausted, "budget exhausted")?;
            let _ = self.store.release_lease(goal_id, &self.runner_id);
            self.write_report(goal_id, GoalStatus::Exhausted, "budget exhausted", &gates)?;
            GoalDecision::Exhausted
        } else {
            let failures = goal.consecutive_failures + 1;
            let no_progress = if gates.iter().all(|g| !g.passed) {
                goal.no_progress_rounds + 1
            } else {
                0
            };
            let status = if failures >= goal.spec.budget.max_consecutive_failures
                || no_progress >= goal.spec.budget.max_no_progress_rounds
            {
                GoalStatus::Blocked
            } else if goal.spec.auto_continue {
                GoalStatus::Active
            } else {
                GoalStatus::Paused
            };
            self.store.update_counters(
                goal_id,
                attempt_no,
                failures,
                no_progress,
                tokens_used,
                Some(wall_started_at),
            )?;
            self.store.set_status(
                goal_id,
                status,
                if status == GoalStatus::Blocked {
                    "failure/no-progress limit reached"
                } else if status == GoalStatus::Paused {
                    "auto_continue disabled"
                } else {
                    "attempt failed; continuing"
                },
            )?;
            if status == GoalStatus::Blocked {
                self.write_report(
                    goal_id,
                    GoalStatus::Blocked,
                    "failure/no-progress limit",
                    &gates,
                )?;
                GoalDecision::Blocked
            } else if status == GoalStatus::Paused {
                GoalDecision::Paused
            } else {
                GoalDecision::Continue
            }
        };
        if all_passed || pending {
            self.store.update_counters(
                goal_id,
                attempt_no,
                goal.consecutive_failures,
                goal.no_progress_rounds,
                tokens_used,
                Some(wall_started_at),
            )?;
        }
        Ok(decision)
    }

    /// User accepted a Human gate / review. Only valid while verifying.
    pub fn accept(&self, goal_id: &str, detail: &str) -> Result<GoalStatus> {
        let goal = self
            .store
            .get(goal_id)?
            .ok_or_else(|| StewardError::Invalid(format!("goal not found: {goal_id}")))?;
        if goal.status != GoalStatus::Verifying {
            return Err(StewardError::Invalid("goal is not verifying".into()));
        }
        self.store
            .set_status(goal_id, GoalStatus::Achieved, detail)?;
        let _ = self.store.release_lease(goal_id, &self.runner_id);
        self.write_report(
            goal_id,
            GoalStatus::Achieved,
            detail,
            &self
                .store
                .attempts(goal_id, 1)?
                .first()
                .map(|a| a.gates.clone())
                .unwrap_or_default(),
        )?;
        Ok(GoalStatus::Achieved)
    }

    pub fn write_report(
        &self,
        goal_id: &str,
        status: GoalStatus,
        summary: &str,
        gates: &[GateResult],
    ) -> Result<GoalReport> {
        let goal = self
            .store
            .get(goal_id)?
            .ok_or_else(|| StewardError::Invalid(format!("goal not found: {goal_id}")))?;
        let evidence_ids = gates
            .iter()
            .map(|g| g.evidence_id.clone())
            .collect::<Vec<_>>();
        let report = GoalReport {
            id: Uuid::new_v4().to_string(),
            goal_id: goal_id.into(),
            status,
            attempts: goal.attempts,
            gates: gates.to_vec(),
            evidence_ids,
            summary: summary.into(),
            next_actions: vec![
                "Inspect gate evidence before editing the contract.".into(),
                "A new goal version is required to change acceptance.".into(),
            ],
            created_at: now_ms(),
        };
        self.store.save_report(&report)?;
        Ok(report)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::goal::spec::{GateResult, GoalGate, GoalSpec};
    use crate::goal::store::GoalStore;

    fn goal(store: &GoalStore) -> String {
        let mut s = GoalSpec::new("g1", "goal", "do it");
        s.gates = vec![GoalGate::Command {
            command: "true".into(),
            timeout_secs: 5,
            retries: 0,
        }];
        s.budget.max_attempts = 5;
        s.budget.max_consecutive_failures = 4;
        s.budget.max_no_progress_rounds = 4;
        store.create(s).unwrap();
        "g1".into()
    }

    fn gate(passed: bool) -> GateResult {
        GateResult {
            gate_index: 0,
            passed,
            pending: false,
            exit_code: Some(if passed { 0 } else { 1 }),
            output: "out".into(),
            duration_ms: 1,
            evidence_id: "gate:0:x".into(),
        }
    }

    #[tokio::test]
    async fn failed_attempts_continue_then_achieve() {
        let store = Arc::new(GoalStore::open_memory().unwrap());
        let id = goal(&store);
        let rt = GoalRuntime::new(store, "r1");
        rt.start(&id).unwrap();
        assert_eq!(
            rt.complete_attempt(&id, "fail", 10, None, vec![gate(false)])
                .await
                .unwrap(),
            GoalDecision::Continue
        );
        assert_eq!(
            rt.complete_attempt(&id, "fail", 10, None, vec![gate(false)])
                .await
                .unwrap(),
            GoalDecision::Continue
        );
        assert_eq!(
            rt.complete_attempt(&id, "ok", 10, None, vec![gate(true)])
                .await
                .unwrap(),
            GoalDecision::Achieved
        );
    }

    #[tokio::test]
    async fn budget_exhausts_without_extra_attempt() {
        let store = Arc::new(GoalStore::open_memory().unwrap());
        let mut s = GoalSpec::new("g1", "goal", "do it");
        s.gates = vec![GoalGate::Command {
            command: "true".into(),
            timeout_secs: 5,
            retries: 0,
        }];
        s.budget.max_attempts = 1;
        store.create(s).unwrap();
        let rt = GoalRuntime::new(store, "r1");
        rt.start("g1").unwrap();
        assert_eq!(
            rt.complete_attempt("g1", "fail", 10, None, vec![gate(false)])
                .await
                .unwrap(),
            GoalDecision::Exhausted
        );
    }

    #[tokio::test]
    async fn human_gate_stops_at_verifying() {
        let store = Arc::new(GoalStore::open_memory().unwrap());
        let mut s = GoalSpec::new("g1", "goal", "do it");
        s.gates = vec![GoalGate::Human {
            prompt: "accept".into(),
        }];
        store.create(s).unwrap();
        let rt = GoalRuntime::new(store, "r1");
        rt.start("g1").unwrap();
        let gate = GateResult {
            gate_index: 0,
            passed: false,
            pending: true,
            exit_code: None,
            output: "accept".into(),
            duration_ms: 0,
            evidence_id: "gate:human".into(),
        };
        assert_eq!(
            rt.complete_attempt("g1", "wait", 1, None, vec![gate])
                .await
                .unwrap(),
            GoalDecision::Verify
        );
        assert_eq!(
            rt.store().get("g1").unwrap().unwrap().status,
            GoalStatus::Verifying
        );
        rt.accept("g1", "user approved").unwrap();
        assert_eq!(
            rt.store().get("g1").unwrap().unwrap().status,
            GoalStatus::Achieved
        );
    }
}
