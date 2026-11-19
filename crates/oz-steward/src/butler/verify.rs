//! The verifier. The only component allowed to write a terminal task status.
//!
//! Verified requires: every hard gate passes, at least one STRONG gate passed,
//! no review defect was named, and no gate is inconclusive. Anything less is
//! `Unverified` (honest) or `Failed` (hard red).

use std::path::Path;

use async_trait::async_trait;

use crate::goal::gate::{GateOutcome, GateRunner};
use crate::goal::spec::{GateResult, GoalGate};

use super::spec::{gate_strength, GateStrength, Task};

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Verdict {
    Verified,
    Unverified,
    Failed,
}

#[derive(Debug, Clone, PartialEq, serde::Serialize)]
pub struct ReviewOutcome {
    pub passed: bool,
    pub defects: Vec<String>,
    pub confidence: f32,
}

impl ReviewOutcome {
    pub fn clean() -> Self {
        Self {
            passed: true,
            defects: Vec::new(),
            confidence: 0.8,
        }
    }

    pub fn defect(text: impl Into<String>) -> Self {
        Self {
            passed: false,
            defects: vec![text.into()],
            confidence: 0.9,
        }
    }
}

/// A blind reviewer: it receives the task plus gate evidence, never the
/// worker's own summary.
#[async_trait]
pub trait Reviewer: Send + Sync {
    async fn review(&self, rubric: &str, context: &str) -> Option<ReviewOutcome>;
    fn model_name(&self) -> String {
        "unknown".into()
    }
}

#[derive(Debug, Clone, PartialEq, serde::Serialize)]
pub struct VerifyReport {
    pub verdict: Verdict,
    pub gates: Vec<GateResult>,
    pub strong_passed: usize,
    pub weak_passed: usize,
    pub reasons: Vec<String>,
    pub review_model: Option<String>,
}

impl VerifyReport {
    pub fn summary(&self) -> String {
        let mut s = format!(
            "{:?}: {} strong / {} weak gate(s) passed",
            self.verdict, self.strong_passed, self.weak_passed
        );
        if !self.reasons.is_empty() {
            s.push_str(" — ");
            s.push_str(&self.reasons.join("; "));
        }
        s
    }
}

fn gate_result(index: usize, outcome: GateOutcome) -> GateResult {
    GateResult {
        gate_index: index,
        passed: outcome.passed,
        pending: outcome.pending,
        exit_code: outcome.exit_code,
        output: outcome.output.chars().take(2_000).collect(),
        duration_ms: outcome.duration_ms,
        evidence_id: format!("gate:{index}:{}", uuid::Uuid::new_v4()),
    }
}

fn review_context(task: &Task, results: &[GateResult]) -> String {
    let mut ctx = format!("Task: {}\n", task.goal);
    if !task.detail.trim().is_empty() {
        ctx.push_str(&format!(
            "Detail: {}\n",
            task.detail.chars().take(800).collect::<String>()
        ));
    }
    ctx.push_str("Gate evidence:\n");
    for r in results {
        ctx.push_str(&format!(
            "- gate {}: passed={} exit={:?} {} \n",
            r.gate_index,
            r.passed,
            r.exit_code,
            r.output.chars().take(300).collect::<String>()
        ));
    }
    ctx
}

pub async fn verify(
    task: &Task,
    workspace: &Path,
    reviewer: Option<&dyn Reviewer>,
) -> VerifyReport {
    let runner = GateRunner::new(workspace.to_path_buf());
    let mut results: Vec<GateResult> = Vec::new();
    let mut reasons: Vec<String> = Vec::new();
    let mut review_model = None;

    for (index, gate) in task.accept.iter().enumerate() {
        match gate {
            GoalGate::Review {
                rubric,
                min_confidence,
            } => {
                let outcome = match reviewer {
                    Some(r) => {
                        review_model = Some(r.model_name());
                        r.review(rubric, &review_context(task, &results)).await
                    }
                    None => None,
                };
                let result = match outcome {
                    Some(o) if o.confidence >= *min_confidence && o.passed => GateResult {
                        gate_index: index,
                        passed: true,
                        pending: false,
                        exit_code: Some(0),
                        output: format!("review passed (confidence {:.2})", o.confidence),
                        duration_ms: 0,
                        evidence_id: format!("gate:{index}:{}", uuid::Uuid::new_v4()),
                    },
                    Some(o) if !o.passed => GateResult {
                        gate_index: index,
                        passed: false,
                        pending: false,
                        exit_code: Some(1),
                        output: format!("review defects: {}", o.defects.join("; ")),
                        duration_ms: 0,
                        evidence_id: format!("gate:{index}:{}", uuid::Uuid::new_v4()),
                    },
                    _ => {
                        reasons.push(format!("review gate {index} was inconclusive"));
                        GateResult {
                            gate_index: index,
                            passed: false,
                            pending: true,
                            exit_code: None,
                            output: "review unavailable or below min_confidence".into(),
                            duration_ms: 0,
                            evidence_id: format!("gate:{index}:{}", uuid::Uuid::new_v4()),
                        }
                    }
                };
                results.push(result);
            }
            GoalGate::Human { .. } => {
                reasons.push("human gate is never satisfied automatically".into());
                results.push(GateResult {
                    gate_index: index,
                    passed: false,
                    pending: true,
                    exit_code: None,
                    output: "human acceptance is not part of the autonomous path".into(),
                    duration_ms: 0,
                    evidence_id: format!("gate:{index}:{}", uuid::Uuid::new_v4()),
                });
            }
            other => {
                let outcome = runner.run_gate(other).await;
                results.push(gate_result(index, outcome));
            }
        }
    }

    let pending = results.iter().filter(|r| r.pending).count();
    let hard_fail = results.iter().any(|r| !r.passed && !r.pending);
    let mut strong_passed = 0usize;
    let mut weak_passed = 0usize;
    for (i, r) in results.iter().enumerate() {
        if r.passed {
            match task
                .accept
                .get(i)
                .map(gate_strength)
                .unwrap_or(GateStrength::Weak)
            {
                GateStrength::Strong => strong_passed += 1,
                GateStrength::Medium => weak_passed += 1,
                _ => weak_passed += 1,
            }
        }
    }

    let verdict = if hard_fail {
        if let Some(r) = results.iter().find(|r| !r.passed && !r.pending) {
            reasons.push(format!(
                "hard gate failed: {}",
                r.output.chars().take(200).collect::<String>()
            ));
        }
        Verdict::Failed
    } else if strong_passed == 0 {
        reasons.push("no strong evidence (build/test) passed".into());
        Verdict::Unverified
    } else if pending > 0 {
        reasons.push(format!("{pending} gate(s) inconclusive"));
        Verdict::Unverified
    } else {
        Verdict::Verified
    };

    VerifyReport {
        verdict,
        gates: results,
        strong_passed,
        weak_passed,
        reasons,
        review_model,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::butler::spec::Task;
    use tempfile::tempdir;

    fn task_with(gates: Vec<GoalGate>) -> Task {
        let mut t = Task::new("p", "b", "goal");
        t.accept = gates;
        t
    }

    struct AlwaysOk;
    #[async_trait]
    impl Reviewer for AlwaysOk {
        async fn review(&self, _rubric: &str, _ctx: &str) -> Option<ReviewOutcome> {
            Some(ReviewOutcome::clean())
        }
        fn model_name(&self) -> String {
            "mock".into()
        }
    }

    struct Defect;
    #[async_trait]
    impl Reviewer for Defect {
        async fn review(&self, _rubric: &str, _ctx: &str) -> Option<ReviewOutcome> {
            Some(ReviewOutcome::defect("missing comparison table"))
        }
    }

    #[tokio::test]
    async fn strong_source_gate_plus_clean_review_is_verified() {
        let dir = tempdir().unwrap();
        for name in ["a.txt", "b.txt", "c.txt"] {
            std::fs::write(dir.path().join(name), "x").unwrap();
        }
        std::fs::write(dir.path().join("sources.md"), "`a.txt` `b.txt` `c.txt`").unwrap();
        let t = task_with(vec![
            GoalGate::SourceCheck {
                path: "sources.md".into(),
                min_sources: 3,
            },
            GoalGate::Review {
                rubric: "ok".into(),
                min_confidence: 0.5,
            },
        ]);
        let r = verify(&t, dir.path(), Some(&AlwaysOk)).await;
        assert_eq!(r.verdict, Verdict::Verified, "report: {r:?}");
        assert_eq!(r.strong_passed, 1);
    }

    #[tokio::test]
    async fn weak_only_is_unverified() {
        let dir = tempdir().unwrap();
        std::fs::write(dir.path().join("a.txt"), "hello").unwrap();
        let t = task_with(vec![GoalGate::FileAssertion {
            path: "a.txt".into(),
            must_exist: true,
            contains: None,
        }]);
        let r = verify(&t, dir.path(), None).await;
        assert_eq!(r.verdict, Verdict::Unverified);
        assert!(r.reasons.iter().any(|s| s.contains("no strong evidence")));
    }

    #[tokio::test]
    async fn failed_build_is_failed() {
        let dir = tempdir().unwrap();
        let t = task_with(vec![GoalGate::Command {
            command: "false".into(),
            timeout_secs: 5,
            retries: 0,
        }]);
        let r = verify(&t, dir.path(), None).await;
        assert_eq!(r.verdict, Verdict::Failed);
    }

    #[tokio::test]
    async fn review_defect_fails_even_with_a_passing_build() {
        let dir = tempdir().unwrap();
        let t = task_with(vec![
            GoalGate::Command {
                command: "true".into(),
                timeout_secs: 5,
                retries: 0,
            },
            GoalGate::Review {
                rubric: "ok".into(),
                min_confidence: 0.5,
            },
        ]);
        let r = verify(&t, dir.path(), Some(&Defect)).await;
        assert_eq!(r.verdict, Verdict::Failed);
        assert!(r.gates[1].output.contains("missing comparison table"));
    }

    #[tokio::test]
    async fn unavailable_review_is_unverified_not_verified() {
        let dir = tempdir().unwrap();
        let t = task_with(vec![
            GoalGate::Command {
                command: "true".into(),
                timeout_secs: 5,
                retries: 0,
            },
            GoalGate::Review {
                rubric: "ok".into(),
                min_confidence: 0.5,
            },
        ]);
        let r = verify(&t, dir.path(), None).await;
        assert_eq!(r.verdict, Verdict::Unverified);
    }

    #[tokio::test]
    async fn human_gate_never_passes() {
        let dir = tempdir().unwrap();
        let t = task_with(vec![
            GoalGate::Command {
                command: "true".into(),
                timeout_secs: 5,
                retries: 0,
            },
            GoalGate::Human {
                prompt: "please".into(),
            },
        ]);
        let r = verify(&t, dir.path(), None).await;
        assert_eq!(r.verdict, Verdict::Unverified);
    }
}
