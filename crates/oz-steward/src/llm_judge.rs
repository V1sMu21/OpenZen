//! System-Two judge: a typed decision model reviews the ledger after the
//! deterministic detectors have run. It may add risk or recommend an action;
//! it may never erase a critical deterministic finding, and a verdict without
//! evidence IDs is treated as abstained.

use serde::Serialize;

use oz_core_types::supervision::{StepRecord, Verdict};

use crate::contract::TaskContract;
use crate::decision::{DecisionProvider, DecisionRequest, Question};
use crate::detect::Finding;
use crate::error::Result;
use crate::judge::max_verdict;

#[derive(Debug, Clone, Serialize)]
pub struct JudgeOutcome {
    pub verdict: Verdict,
    pub confidence: f32,
    pub diagnosis: String,
    pub recommended_action: Option<String>,
    pub evidence_ids: Vec<String>,
    pub abstained: bool,
}

pub struct LLMJudge<'a> {
    provider: &'a dyn DecisionProvider,
    none_threshold: f32,
}

impl<'a> LLMJudge<'a> {
    pub fn new(provider: &'a dyn DecisionProvider) -> Self {
        Self {
            provider,
            none_threshold: 0.2,
        }
    }

    pub fn with_none_threshold(mut self, threshold: f32) -> Self {
        self.none_threshold = threshold.clamp(0.0, 1.0);
        self
    }

    pub async fn judge(
        &self,
        contract: Option<&TaskContract>,
        steps: &[StepRecord],
        findings: &[Finding],
        deterministic: Verdict,
    ) -> Result<JudgeOutcome> {
        let evidence_ids = findings
            .iter()
            .flat_map(|f| f.evidence_ids.clone())
            .collect::<Vec<_>>();
        let state = serde_json::json!({
            "goal": contract.map(|c| c.goal.clone()).unwrap_or_default(),
            "acceptance": contract
                .map(|c| c.acceptance.iter().map(|a| a.description.clone()).collect::<Vec<_>>())
                .unwrap_or_default(),
            "steps": steps.iter().map(|s| serde_json::json!({
                "turn": s.turn,
                "tool": s.tool.as_ref().map(|t| t.name.clone()),
                "status": format!("{:?}", s.status),
                "targets": s.targets,
                "artifacts": s.artifacts,
            })).collect::<Vec<_>>(),
            "deterministic_findings": findings.iter().map(|f| serde_json::json!({
                "kind": format!("{:?}", f.kind),
                "severity": format!("{:?}", f.severity),
                "detail": f.detail,
                "evidence_ids": f.evidence_ids,
            })).collect::<Vec<_>>(),
            "deterministic_verdict": format!("{deterministic:?}"),
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
                "evidence",
                Question::yes_no("Is there ledger evidence that supports this verdict?"),
            )
            .ask(
                "action",
                Question::choice(
                    "What is the smallest correct next action?",
                    ["nudge", "reanchor", "replan", "rollback", "stop"],
                ),
            );
        let resp = self.provider.evaluate(&req).await?;
        let Some(answer) = resp.answers.get("verdict") else {
            return Ok(JudgeOutcome {
                verdict: deterministic,
                confidence: 0.0,
                diagnosis: "judge omitted the verdict answer".into(),
                recommended_action: None,
                evidence_ids,
                abstained: true,
            });
        };
        if answer.abstained(self.none_threshold) || answer.choice.is_none() {
            return Ok(JudgeOutcome {
                verdict: deterministic,
                confidence: answer.confidence.unwrap_or(0.0),
                diagnosis: "judge abstained (none_probability/choice missing)".into(),
                recommended_action: None,
                evidence_ids,
                abstained: true,
            });
        }
        // The evidence question is a hard gate when the judge says yes/no.
        let evidence_yes = resp
            .answers
            .get("evidence")
            .and_then(|a| a.noul)
            .unwrap_or(0.0);
        if evidence_ids.is_empty() || evidence_yes < 0.5 {
            return Ok(JudgeOutcome {
                verdict: deterministic,
                confidence: answer.confidence.unwrap_or(0.0),
                diagnosis: "judge verdict has no supporting evidence".into(),
                recommended_action: None,
                evidence_ids,
                abstained: true,
            });
        }
        let model_verdict = match answer.choice.as_deref() {
            Some("on_track") => Verdict::OnTrack,
            Some("at_risk") => Verdict::AtRisk,
            Some("off_track") => Verdict::OffTrack,
            Some("stalled") => Verdict::Stalled,
            Some("blocked") => Verdict::Blocked,
            _ => deterministic,
        };
        let recommended_action = resp.answers.get("action").and_then(|a| a.choice.clone());
        Ok(JudgeOutcome {
            verdict: max_verdict(deterministic, model_verdict),
            confidence: answer.confidence.unwrap_or(0.0).clamp(0.0, 1.0),
            diagnosis: format!("typed judge returned {model_verdict:?}"),
            recommended_action,
            evidence_ids,
            abstained: false,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::decision::{Answer, DecisionResponse};
    use oz_core_types::supervision::{Phase, StepStatus, ToolRecord};
    use std::collections::BTreeMap;

    struct Mock {
        verdict: &'static str,
        evidence_yes: f32,
        none: f32,
    }

    #[async_trait::async_trait]
    impl DecisionProvider for Mock {
        async fn evaluate(&self, _req: &DecisionRequest) -> Result<DecisionResponse> {
            let mut answers = BTreeMap::new();
            answers.insert(
                "verdict".into(),
                Answer {
                    choice: Some(self.verdict.into()),
                    confidence: Some(0.9),
                    none_probability: Some(self.none),
                    ..Answer::default()
                },
            );
            answers.insert(
                "evidence".into(),
                Answer {
                    noul: Some(self.evidence_yes),
                    ..Answer::default()
                },
            );
            answers.insert(
                "action".into(),
                Answer {
                    choice: Some("reanchor".into()),
                    ..Answer::default()
                },
            );
            Ok(DecisionResponse {
                answers,
                ..DecisionResponse::default()
            })
        }
        fn name(&self) -> &'static str {
            "mock"
        }
    }

    fn step(id: &str, target: &str) -> StepRecord {
        StepRecord {
            id: id.into(),
            ticket_id: "t".into(),
            session_id: "s".into(),
            turn: 1,
            phase: Phase::Acting,
            at: 1,
            tool: Some(ToolRecord {
                name: "write".into(),
                args_hash: "h".into(),
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

    fn finding() -> Finding {
        Finding {
            kind: crate::detect::FindingKind::Loop,
            severity: crate::detect::Severity::Warn,
            evidence_ids: vec!["s1".into()],
            detail: "loop".into(),
        }
    }

    #[tokio::test]
    async fn judge_cannot_downgrade_deterministic_off_track() {
        let mock = Mock {
            verdict: "on_track",
            evidence_yes: 0.9,
            none: 0.0,
        };
        let out = LLMJudge::new(&mock)
            .judge(
                None,
                &[step("s1", "src/a")],
                &[finding()],
                Verdict::OffTrack,
            )
            .await
            .unwrap();
        assert_eq!(out.verdict, Verdict::OffTrack);
        assert!(!out.abstained);
    }

    #[tokio::test]
    async fn no_evidence_abstains_and_keeps_deterministic() {
        let mock = Mock {
            verdict: "at_risk",
            evidence_yes: 0.2,
            none: 0.0,
        };
        let out = LLMJudge::new(&mock)
            .judge(None, &[step("s1", "src/a")], &[], Verdict::OnTrack)
            .await
            .unwrap();
        assert!(out.abstained);
        assert_eq!(out.verdict, Verdict::OnTrack);
    }

    #[tokio::test]
    async fn model_can_add_risk_with_evidence() {
        let mock = Mock {
            verdict: "at_risk",
            evidence_yes: 0.9,
            none: 0.0,
        };
        let out = LLMJudge::new(&mock)
            .judge(None, &[step("s1", "src/a")], &[finding()], Verdict::OnTrack)
            .await
            .unwrap();
        assert_eq!(out.verdict, Verdict::AtRisk);
        assert_eq!(out.recommended_action.as_deref(), Some("reanchor"));
    }

    #[tokio::test]
    async fn none_probability_abstains() {
        let mock = Mock {
            verdict: "at_risk",
            evidence_yes: 0.9,
            none: 0.9,
        };
        let out = LLMJudge::new(&mock)
            .judge(None, &[], &[finding()], Verdict::OnTrack)
            .await
            .unwrap();
        assert!(out.abstained);
    }
}
