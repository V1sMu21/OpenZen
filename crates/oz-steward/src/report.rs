//! Evidence-backed reports. An "achieved" report requires a verification
//! signal; without one the honest status is `unverified`.

use serde::{Deserialize, Serialize};
use serde_json::Value;
use uuid::Uuid;

use oz_core_types::supervision::{now_ms, StepRecord, TicketStatus};

use crate::contract::TaskContract;
use crate::error::Result;
use crate::goal::spec::{GateResult, GoalStatus};
use crate::store::Store;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReportStatus {
    Achieved,
    Unverified,
    Blocked,
    Exhausted,
    Abandoned,
    Failed,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GateReport {
    pub name: String,
    pub passed: bool,
    pub detail: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Report {
    pub id: String,
    pub ticket_id: String,
    pub status: ReportStatus,
    pub summary: String,
    pub goal: String,
    pub evidence_ids: Vec<String>,
    pub gates: Vec<GateReport>,
    pub attempts: u32,
    pub last_verdict: Option<String>,
    pub next_actions: Vec<String>,
    pub created_at: i64,
}

impl Report {
    /// Short IM/notification summary, hard-capped at 100 words.
    pub fn im_summary(&self) -> String {
        let base = format!(
            "{:?} · {} · {} evidence · {}",
            self.status,
            self.goal,
            self.evidence_ids.len(),
            self.next_actions.first().cloned().unwrap_or_default()
        );
        let words = base.split_whitespace().collect::<Vec<_>>();
        if words.len() <= 100 {
            base
        } else {
            format!("{}…", words[..100].join(" "))
        }
    }

    pub fn to_markdown(&self) -> String {
        let gates = if self.gates.is_empty() {
            "- (no gate results)".to_string()
        } else {
            self.gates
                .iter()
                .map(|g| {
                    format!(
                        "- {}: {} — {}",
                        g.name,
                        if g.passed { "PASS" } else { "FAIL" },
                        g.detail
                    )
                })
                .collect::<Vec<_>>()
                .join("\n")
        };
        format!(
            "# Steward report · {}\n\n- Status: {:?}\n- Goal: {}\n- Attempts: {}\n- Evidence: {}\n\n## Gates\n{}\n\n## Next\n{}\n",
            self.ticket_id,
            self.status,
            self.goal,
            self.attempts,
            self.evidence_ids.len(),
            gates,
            self.next_actions
                .iter()
                .map(|a| format!("- {a}"))
                .collect::<Vec<_>>()
                .join("\n")
        )
    }
}

pub struct ReportComposer {
    store: std::sync::Arc<Store>,
}

impl ReportComposer {
    pub fn new(store: std::sync::Arc<Store>) -> Self {
        Self { store }
    }

    /// `verified` must come from the runtime (gate execution), never from the
    /// model. `gates` may be empty for a human acceptance, in which case the
    /// caller must still pass `verified=true` explicitly.
    pub fn compose(
        &self,
        ticket_id: &str,
        verified: bool,
        gates: Vec<GateReport>,
    ) -> Result<Report> {
        let snapshot = self.store.snapshot(ticket_id)?;
        let contract = self.store.latest_contract(ticket_id)?;
        let mut steps = self.store.steps(ticket_id, 200)?;
        steps.reverse();
        let evidence_ids = steps.iter().map(|s| s.id.clone()).collect::<Vec<_>>();
        // A bare `verified=true` with no gate result is not evidence. Require
        // at least one explicit gate (a human acceptance is a gate too).
        let verified = verified && !gates.is_empty();
        let status = report_status(snapshot.as_ref().map(|s| s.status), verified);
        let summary = make_summary(contract.as_ref(), &steps, status, &evidence_ids);
        let report = Report {
            id: Uuid::new_v4().to_string(),
            ticket_id: ticket_id.into(),
            status,
            summary,
            goal: contract
                .as_ref()
                .map(|c| c.goal.clone())
                .unwrap_or_default(),
            evidence_ids,
            gates,
            attempts: steps.iter().map(|s| s.turn).max().unwrap_or(0),
            last_verdict: snapshot
                .as_ref()
                .and_then(|s| s.last_verdict.map(|v| format!("{v:?}"))),
            next_actions: next_actions(status),
            created_at: now_ms(),
        };
        self.store.save_report(&report)?;
        Ok(report)
    }

    /// Compose from Goal Mode's runtime evidence. This is the single Steward
    /// report path for goals: a goal cannot be `achieved` unless every gate
    /// result exists and passed.
    pub fn compose_goal(
        &self,
        ticket_id: &str,
        goal: &str,
        status: GoalStatus,
        gates: &[GateResult],
        attempts: u32,
    ) -> Result<Report> {
        let gate_reports = gates
            .iter()
            .map(|g| GateReport {
                name: format!("gate {}", g.gate_index),
                passed: g.passed && !g.pending,
                detail: format!(
                    "exit={:?} {}",
                    g.exit_code,
                    g.output.chars().take(500).collect::<String>()
                ),
            })
            .collect::<Vec<_>>();
        let evidence_ids = gates
            .iter()
            .map(|g| g.evidence_id.clone())
            .collect::<Vec<_>>();
        let all_passed = !gate_reports.is_empty() && gate_reports.iter().all(|g| g.passed);
        let report_status = match status {
            GoalStatus::Achieved if all_passed => ReportStatus::Achieved,
            GoalStatus::Achieved => ReportStatus::Unverified,
            GoalStatus::Blocked => ReportStatus::Blocked,
            GoalStatus::Exhausted => ReportStatus::Exhausted,
            GoalStatus::Abandoned => ReportStatus::Abandoned,
            _ => ReportStatus::Unverified,
        };
        let report = Report {
            id: Uuid::new_v4().to_string(),
            ticket_id: ticket_id.into(),
            status: report_status,
            summary: format!(
                "{report_status:?}: {goal} — {attempts} attempts, {} gate evidence",
                evidence_ids.len()
            ),
            goal: goal.into(),
            evidence_ids,
            gates: gate_reports,
            attempts,
            last_verdict: Some(format!("{status:?}")),
            next_actions: next_actions(report_status),
            created_at: now_ms(),
        };
        self.store.save_report(&report)?;
        Ok(report)
    }

    pub fn latest(&self, ticket_id: &str) -> Result<Option<Report>> {
        self.store.latest_report(ticket_id)
    }
}

fn report_status(status: Option<TicketStatus>, verified: bool) -> ReportStatus {
    match status {
        Some(TicketStatus::Blocked) => ReportStatus::Blocked,
        Some(TicketStatus::Exhausted) => ReportStatus::Exhausted,
        Some(TicketStatus::Abandoned) => ReportStatus::Abandoned,
        Some(TicketStatus::Failed) => ReportStatus::Failed,
        _ if verified => ReportStatus::Achieved,
        _ => ReportStatus::Unverified,
    }
}

fn make_summary(
    contract: Option<&TaskContract>,
    steps: &[StepRecord],
    status: ReportStatus,
    evidence: &[String],
) -> String {
    let goal = contract.map(|c| c.goal.as_str()).unwrap_or("(no contract)");
    format!(
        "{status:?}: {goal} — {} steps, {} evidence ids{}",
        steps.len(),
        evidence.len(),
        if steps.is_empty() {
            "; nothing was verified"
        } else {
            ""
        }
    )
}

fn next_actions(status: ReportStatus) -> Vec<String> {
    match status {
        ReportStatus::Achieved => vec!["No action; report is immutable.".into()],
        ReportStatus::Unverified => vec![
            "Run the acceptance gate or provide human acceptance.".into(),
            "Do not claim done until evidence exists.".into(),
        ],
        ReportStatus::Blocked => vec!["Resolve the named blocker, then resume.".into()],
        ReportStatus::Exhausted => {
            vec!["Extend the budget as a new contract version, or take over.".into()]
        }
        ReportStatus::Abandoned => vec!["Reopen as a new ticket if needed.".into()],
        ReportStatus::Failed => vec!["Inspect the failure evidence; retry or abandon.".into()],
    }
}

/// Stable JSON view for Tauri.
pub fn report_json(report: &Report) -> Value {
    serde_json::to_value(report).unwrap_or(Value::Null)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::contract::{Acceptance, RiskLevel};
    use oz_core_types::supervision::{Phase, StepStatus};

    fn setup() -> (std::sync::Arc<Store>, ReportComposer) {
        let store = std::sync::Arc::new(Store::open_memory().unwrap());
        store.ensure_ticket("t1", "sess", "title").unwrap();
        let c = TaskContract::new(
            "t1",
            "ship it",
            vec![Acceptance {
                id: "a".into(),
                description: "tests pass".into(),
            }],
            RiskLevel::Low,
            true,
        )
        .unwrap();
        store.save_contract(&c).unwrap();
        (store.clone(), ReportComposer::new(store))
    }

    fn step(id: &str) -> StepRecord {
        StepRecord {
            id: id.into(),
            ticket_id: "t1".into(),
            session_id: "sess".into(),
            turn: 1,
            phase: Phase::Acting,
            at: 1,
            tool: None,
            status: StepStatus::Succeeded,
            artifacts: vec![],
            targets: vec![],
            plan_hash: None,
            git_sha: None,
            tokens_in: 0,
            tokens_out: 0,
        }
    }

    #[test]
    fn no_verification_is_unverified() {
        let (store, composer) = setup();
        store.record_step(&step("s1")).unwrap();
        let r = composer.compose("t1", false, vec![]).unwrap();
        assert_eq!(r.status, ReportStatus::Unverified);
        assert!(r.to_markdown().contains("Unverified"));
        assert!(composer.latest("t1").unwrap().is_some());
    }

    #[test]
    fn verified_with_evidence_is_achieved() {
        let (store, composer) = setup();
        store.record_step(&step("s1")).unwrap();
        let r = composer
            .compose(
                "t1",
                true,
                vec![GateReport {
                    name: "cargo test".into(),
                    passed: true,
                    detail: "exit 0".into(),
                }],
            )
            .unwrap();
        assert_eq!(r.status, ReportStatus::Achieved);
        assert_eq!(r.evidence_ids, vec!["s1".to_string()]);
        assert!(r.to_markdown().contains("cargo test"));
    }

    #[test]
    fn goal_report_requires_gate_evidence() {
        use crate::goal::spec::GateResult;
        let (_store, composer) = setup();
        let gate = GateResult {
            gate_index: 0,
            passed: true,
            pending: false,
            exit_code: Some(0),
            output: "ok".into(),
            duration_ms: 1,
            evidence_id: "gate:0:x".into(),
        };
        let r = composer
            .compose_goal(
                "t1",
                "goal",
                crate::goal::spec::GoalStatus::Achieved,
                &[gate],
                1,
            )
            .unwrap();
        assert_eq!(r.status, ReportStatus::Achieved);
        assert_eq!(r.evidence_ids, vec!["gate:0:x".to_string()]);
        let unverified = composer
            .compose_goal(
                "t1",
                "goal",
                crate::goal::spec::GoalStatus::Achieved,
                &[],
                1,
            )
            .unwrap();
        assert_eq!(unverified.status, ReportStatus::Unverified);
    }

    #[test]
    fn im_summary_is_bounded() {
        let (store, composer) = setup();
        store.record_step(&step("s1")).unwrap();
        let r = composer.compose("t1", false, vec![]).unwrap();
        assert!(r.im_summary().split_whitespace().count() <= 101);
    }

    #[test]
    fn verified_without_gates_is_unverified() {
        let (store, composer) = setup();
        store.record_step(&step("s1")).unwrap();
        let r = composer.compose("t1", true, vec![]).unwrap();
        assert_eq!(r.status, ReportStatus::Unverified);
    }

    #[test]
    fn blocked_ticket_stays_blocked_even_if_verified_flag_is_true() {
        let (store, composer) = setup();
        store
            .set_status("t1", TicketStatus::Blocked, Phase::Acting, None)
            .unwrap();
        let r = composer.compose("t1", true, vec![]).unwrap();
        assert_eq!(r.status, ReportStatus::Blocked);
    }
}
