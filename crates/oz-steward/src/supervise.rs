//! The thin supervisor that ties the store, detectors and L1 nudge together.
//!
//! `evaluate` is deliberately synchronous and side-effect-bounded: it reads
//! the ledger, produces a judgment, records it, and at most records one L1
//! nudge. It never changes tool permissions, files, or the worker's prompt
//! beyond the explicit nudge text returned to the caller.

use std::sync::Arc;

use oz_core_types::supervision::{
    now_ms, InterventionRecord, JudgmentRecord, TicketStatus, Verdict,
};
use uuid::Uuid;

use crate::contract::TaskContract;
use crate::decision::{DecisionProvider, DecisionRequest, Question};
use crate::detect::{detect_all, Observation};
use crate::error::Result;
use crate::judge::{deterministic, max_verdict, nudge_text, Action, DeterministicJudgment};
use crate::store::Store;

pub struct Supervisor {
    store: Arc<Store>,
    stale_ms: i64,
}

impl Supervisor {
    pub fn new(store: Arc<Store>) -> Self {
        Self {
            store,
            stale_ms: 90_000,
        }
    }

    pub fn with_stale_ms(mut self, stale_ms: i64) -> Self {
        self.stale_ms = stale_ms.max(1);
        self
    }

    /// Evaluate a ticket. Returns `None` for terminal tickets (nothing to
    /// judge) or when no contract/steps exist yet.
    pub fn evaluate(
        &self,
        ticket_id: &str,
        claimed_done: bool,
        verified: bool,
    ) -> Result<Option<DeterministicJudgment>> {
        let Some(snapshot) = self.store.snapshot(ticket_id)? else {
            return Ok(None);
        };
        if snapshot.status.is_terminal() {
            return Ok(None);
        }
        let contract = self.store.latest_contract(ticket_id)?;
        if contract.is_none() && snapshot.step_count == 0 {
            return Ok(None);
        }
        let mut steps = self.store.steps(ticket_id, 50)?;
        steps.reverse(); // store returns newest-first; detectors want chronological
        let mut obs = Observation::new(&steps, now_ms());
        obs.contract = contract.as_ref();
        obs.heartbeat_at = snapshot.heartbeat_at;
        obs.active = matches!(
            snapshot.status,
            TicketStatus::Active | TicketStatus::Verifying | TicketStatus::Blocked
        );
        obs.stale_ms = self.stale_ms;
        obs.claimed_done = claimed_done;
        obs.verified = verified;

        let judgment = deterministic(detect_all(&obs));
        if judgment.verdict == Verdict::OnTrack {
            return Ok(Some(judgment));
        }

        let ids = judgment
            .findings
            .iter()
            .flat_map(|f| f.evidence_ids.clone())
            .collect::<Vec<_>>();
        self.store.record_judgment(&JudgmentRecord {
            id: Uuid::new_v4().to_string(),
            ticket_id: ticket_id.to_string(),
            at: now_ms(),
            provider: "rules".into(),
            question: "progress".into(),
            verdict: judgment.verdict,
            confidence: judgment.confidence,
            evidence_ids: ids.clone(),
            action: judgment.action.as_ref().map(|a| match a {
                Action::Nudge(_) => "L1_nudge".into(),
                Action::Escalate => "L2_escalate".into(),
            }),
        })?;

        match &judgment.action {
            Some(Action::Nudge(text)) => {
                self.store.record_intervention(&InterventionRecord {
                    id: Uuid::new_v4().to_string(),
                    ticket_id: ticket_id.to_string(),
                    at: now_ms(),
                    level: "L1".into(),
                    reason: text.clone(),
                    evidence_ids: ids,
                    result: "nudged".into(),
                })?;
            }
            Some(Action::Escalate) => {
                self.store.record_intervention(&InterventionRecord {
                    id: Uuid::new_v4().to_string(),
                    ticket_id: ticket_id.to_string(),
                    at: now_ms(),
                    level: "L2".into(),
                    reason: "deterministic critical finding; escalate for re-anchor".into(),
                    evidence_ids: ids,
                    result: "escalate_required".into(),
                })?;
            }
            None => {}
        }
        Ok(Some(judgment))
    }

    /// Ask a local typed-decision provider for a second opinion. The
    /// deterministic verdict is a floor: a model can add risk, never remove a
    /// critical finding.
    pub async fn semantic_evaluate(
        &self,
        ticket_id: &str,
        provider: &dyn DecisionProvider,
        claimed_done: bool,
        verified: bool,
    ) -> Result<Option<DeterministicJudgment>> {
        let Some(det) = self.evaluate(ticket_id, claimed_done, verified)? else {
            return Ok(None);
        };
        if matches!(det.verdict, Verdict::OffTrack | Verdict::Stalled) {
            return Ok(Some(det));
        }
        let contract = self.store.latest_contract(ticket_id)?;
        let mut steps = self.store.steps(ticket_id, 20)?;
        steps.reverse();
        let state = serde_json::json!({
            "goal": contract.as_ref().map(|c| c.goal.clone()).unwrap_or_default(),
            "acceptance": contract.as_ref().map(|c| c.acceptance.iter().map(|a| a.description.clone()).collect::<Vec<_>>()).unwrap_or_default(),
            "steps": steps.iter().map(|s| serde_json::json!({
                "turn": s.turn, "tool": s.tool.as_ref().map(|t| t.name.clone()),
                "status": format!("{:?}", s.status),
                "artifacts": s.artifacts.len(), "targets": s.targets.len(),
            })).collect::<Vec<_>>(),
            "deterministic_findings": det.findings.iter().map(|f| format!("{:?}: {}", f.kind, f.detail)).collect::<Vec<_>>(),
        });
        let req = DecisionRequest::new(state)
            .ask(
                "verdict",
                Question::choice(
                    "Is the worker on track to satisfy the contract?",
                    ["on_track", "at_risk", "off_track", "stalled", "blocked"],
                ),
            )
            .ask(
                "progress",
                Question::score("Progress toward acceptance", ["0", "25", "50", "75", "100"]),
            );
        let resp = provider.evaluate(&req).await?;
        let Some(answer) = resp.answers.get("verdict") else {
            return Ok(Some(det));
        };
        if answer.abstained(0.2) || answer.choice.is_none() {
            return Ok(Some(det));
        }
        let model_verdict = match answer.choice.as_deref() {
            Some("on_track") => Verdict::OnTrack,
            Some("at_risk") => Verdict::AtRisk,
            Some("off_track") => Verdict::OffTrack,
            Some("stalled") => Verdict::Stalled,
            Some("blocked") => Verdict::Blocked,
            _ => return Ok(Some(det)),
        };
        let final_verdict = max_verdict(det.verdict, model_verdict);
        let confidence = answer.confidence.unwrap_or(0.6).clamp(0.0, 1.0);
        let action = match final_verdict {
            Verdict::OffTrack | Verdict::Blocked => Some(Action::Escalate),
            Verdict::AtRisk | Verdict::Stalled => {
                Some(Action::Nudge(nudge_text(final_verdict, &det.findings)))
            }
            Verdict::OnTrack => None,
        };
        self.store.record_judgment(&JudgmentRecord {
            id: Uuid::new_v4().to_string(),
            ticket_id: ticket_id.to_string(),
            at: now_ms(),
            provider: provider.name().into(),
            question: "semantic_progress".into(),
            verdict: final_verdict,
            confidence,
            evidence_ids: det
                .findings
                .iter()
                .flat_map(|f| f.evidence_ids.clone())
                .collect(),
            action: action.as_ref().map(|a| match a {
                Action::Nudge(_) => "L1_nudge".into(),
                Action::Escalate => "L2_escalate".into(),
            }),
        })?;
        Ok(Some(DeterministicJudgment {
            verdict: final_verdict,
            confidence,
            findings: det.findings,
            action,
        }))
    }

    /// Sweep stale tickets and emit a stalled judgment for each.
    pub fn sweep(&self, now: i64) -> Result<Vec<DeterministicJudgment>> {
        let mut out = Vec::new();
        for ticket in self.store.sweep_stalled(now, self.stale_ms)? {
            let mut steps = self.store.steps(&ticket.id, 10)?;
            steps.reverse();
            let mut obs = Observation::new(&steps, now);
            obs.heartbeat_at = ticket.heartbeat_at;
            obs.stale_ms = self.stale_ms;
            let judgment = deterministic(detect_all(&obs));
            if judgment.verdict != Verdict::OnTrack {
                self.store.record_judgment(&JudgmentRecord {
                    id: Uuid::new_v4().to_string(),
                    ticket_id: ticket.id.clone(),
                    at: now,
                    provider: "watchdog".into(),
                    question: "stalled".into(),
                    verdict: judgment.verdict,
                    confidence: judgment.confidence,
                    evidence_ids: judgment
                        .findings
                        .iter()
                        .flat_map(|f| f.evidence_ids.clone())
                        .collect(),
                    action: Some("L1_nudge".into()),
                })?;
            }
            out.push(judgment);
        }
        Ok(out)
    }

    pub fn store(&self) -> &Arc<Store> {
        &self.store
    }

    pub fn contract(&self, ticket_id: &str) -> Result<Option<TaskContract>> {
        self.store.latest_contract(ticket_id)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::contract::{Acceptance, RiskLevel};
    use oz_core_types::supervision::{Phase, StepRecord, StepStatus, ToolRecord};

    fn setup() -> (Arc<Store>, Supervisor) {
        let store = Arc::new(Store::open_memory().unwrap());
        store.ensure_ticket("t1", "sess", "title").unwrap();
        let mut c = TaskContract::new(
            "t1",
            "only touch src",
            vec![Acceptance {
                id: "a".into(),
                description: "src changed".into(),
            }],
            RiskLevel::Low,
            true,
        )
        .unwrap();
        c.allowed_paths = vec!["src".into()];
        store.save_contract(&c).unwrap();
        let sup = Supervisor::new(store.clone());
        (store, sup)
    }

    fn step(id: &str, target: &str) -> StepRecord {
        StepRecord {
            id: id.into(),
            ticket_id: "t1".into(),
            session_id: "sess".into(),
            turn: 1,
            phase: Phase::Acting,
            at: now_ms(),
            tool: Some(ToolRecord {
                name: "write".into(),
                args_hash: format!("h-{id}"),
                output_hash: None,
                duration_ms: Some(1),
                status: StepStatus::Succeeded,
            }),
            status: StepStatus::Succeeded,
            artifacts: vec![],
            targets: vec![target.into()],
            plan_hash: None,
            git_sha: None,
            tokens_in: 1,
            tokens_out: 1,
        }
    }

    #[test]
    fn out_of_scope_write_escalates_and_records_judgment() {
        let (store, sup) = setup();
        store.record_step(&step("s1", "Cargo.toml")).unwrap();
        let j = sup.evaluate("t1", false, false).unwrap().unwrap();
        assert_eq!(j.verdict, Verdict::OffTrack);
        assert!(matches!(j.action, Some(Action::Escalate)));
        let inbox = store.inbox().unwrap();
        assert!(inbox.is_empty()); // blocked/stalled are inbox; off_track alone is not
        assert_eq!(
            store.snapshot("t1").unwrap().unwrap().last_verdict,
            Some(Verdict::OffTrack)
        );
    }

    #[test]
    fn on_track_run_has_no_judgment_noise() {
        let (store, sup) = setup();
        store.record_step(&step("s1", "src/a.rs")).unwrap();
        let j = sup.evaluate("t1", false, false).unwrap().unwrap();
        assert_eq!(j.verdict, Verdict::OnTrack);
    }

    #[test]
    fn terminal_ticket_is_skipped() {
        let (store, sup) = setup();
        store
            .set_status("t1", TicketStatus::Achieved, Phase::Done, None)
            .unwrap();
        assert!(sup.evaluate("t1", true, true).unwrap().is_none());
    }

    struct MockProvider {
        verdict: &'static str,
    }

    #[async_trait::async_trait]
    impl crate::decision::DecisionProvider for MockProvider {
        async fn evaluate(
            &self,
            _req: &crate::decision::DecisionRequest,
        ) -> crate::error::Result<crate::decision::DecisionResponse> {
            let mut answers = std::collections::BTreeMap::new();
            answers.insert(
                "verdict".to_string(),
                crate::decision::Answer {
                    choice: Some(self.verdict.into()),
                    confidence: Some(0.9),
                    ..crate::decision::Answer::default()
                },
            );
            Ok(crate::decision::DecisionResponse {
                answers,
                ..crate::decision::DecisionResponse::default()
            })
        }

        fn name(&self) -> &'static str {
            "mock"
        }
    }

    #[tokio::test]
    async fn semantic_model_cannot_erase_a_critical_finding() {
        let (store, sup) = setup();
        store.record_step(&step("s1", "Cargo.toml")).unwrap();
        let j = sup
            .semantic_evaluate(
                "t1",
                &MockProvider {
                    verdict: "on_track",
                },
                false,
                false,
            )
            .await
            .unwrap()
            .unwrap();
        assert_eq!(j.verdict, Verdict::OffTrack);
    }

    #[tokio::test]
    async fn semantic_model_can_add_risk() {
        let (store, sup) = setup();
        store.record_step(&step("s1", "src/a.rs")).unwrap();
        let j = sup
            .semantic_evaluate("t1", &MockProvider { verdict: "at_risk" }, false, false)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(j.verdict, Verdict::AtRisk);
    }

    #[tokio::test]
    async fn semantic_model_on_track_keeps_deterministic_on_track() {
        let (store, sup) = setup();
        store.record_step(&step("s1", "src/a.rs")).unwrap();
        let j = sup
            .semantic_evaluate(
                "t1",
                &MockProvider {
                    verdict: "on_track",
                },
                false,
                false,
            )
            .await
            .unwrap()
            .unwrap();
        assert_eq!(j.verdict, Verdict::OnTrack);
    }

    #[test]
    fn sweep_marks_stalled_and_returns_judgment() {
        let (store, sup) = setup();
        let mut old = step("s1", "src/a.rs");
        old.at = 1;
        store.record_step(&old).unwrap();
        {
            let conn = store.lock_for_test();
            conn.execute(
                "UPDATE ticket SET heartbeat_at=1, updated_at=1 WHERE id='t1'",
                [],
            )
            .unwrap();
        }
        let done = sup.sweep(1_000_000).unwrap();
        assert_eq!(done.len(), 1);
        assert_eq!(done[0].verdict, Verdict::Stalled);
        assert_eq!(
            store.snapshot("t1").unwrap().unwrap().status,
            TicketStatus::Stalled
        );
    }
}
