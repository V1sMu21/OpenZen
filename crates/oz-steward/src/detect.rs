//! Deterministic drift detectors. These run before any model: a stalled
//! ticket, a repeated tool call, an out-of-scope write, a budget overrun and
//! a self-declared completion without evidence must not depend on an LLM.

use std::collections::HashMap;

use oz_core_types::supervision::StepRecord;
use serde::Serialize;

use crate::contract::TaskContract;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize)]
pub enum FindingKind {
    Stall,
    Loop,
    Scope,
    Idle,
    Budget,
    FalseCompletion,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize)]
pub enum Severity {
    Info,
    Warn,
    Critical,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Finding {
    pub kind: FindingKind,
    pub severity: Severity,
    pub evidence_ids: Vec<String>,
    pub detail: String,
}

impl Finding {
    fn new(kind: FindingKind, severity: Severity, detail: impl Into<String>) -> Self {
        Self {
            kind,
            severity,
            evidence_ids: Vec::new(),
            detail: detail.into(),
        }
    }

    fn with_ids(mut self, ids: Vec<String>) -> Self {
        self.evidence_ids = ids;
        self
    }
}

pub struct Observation<'a> {
    pub contract: Option<&'a TaskContract>,
    /// Chronological order (oldest first). Callers should pass the tail they
    /// care about; detectors only inspect what they need.
    pub steps: &'a [StepRecord],
    pub now: i64,
    pub heartbeat_at: Option<i64>,
    pub active: bool,
    pub stale_ms: i64,
    pub claimed_done: bool,
    pub verified: bool,
}

impl<'a> Observation<'a> {
    pub fn new(steps: &'a [StepRecord], now: i64) -> Self {
        Self {
            contract: None,
            steps,
            now,
            heartbeat_at: None,
            active: true,
            stale_ms: 90_000,
            claimed_done: false,
            verified: false,
        }
    }
}

pub fn detect_all(obs: &Observation<'_>) -> Vec<Finding> {
    let mut out = Vec::new();
    out.extend(detect_stall(obs));
    out.extend(detect_loop(obs));
    out.extend(detect_scope(obs));
    out.extend(detect_idle(obs));
    out.extend(detect_budget(obs));
    out.extend(detect_false_completion(obs));
    out.sort_by_key(|b| std::cmp::Reverse(b.severity));
    out
}

fn last_activity(obs: &Observation<'_>) -> Option<i64> {
    let last_step = obs.steps.iter().map(|s| s.at).max();
    match (last_step, obs.heartbeat_at) {
        (Some(a), Some(b)) => Some(a.max(b)),
        (Some(a), None) => Some(a),
        (None, Some(b)) => Some(b),
        (None, None) => None,
    }
}

fn detect_stall(obs: &Observation<'_>) -> Vec<Finding> {
    if !obs.active {
        return Vec::new();
    }
    let Some(last) = last_activity(obs) else {
        return Vec::new();
    };
    if obs.now.saturating_sub(last) <= obs.stale_ms.max(1) {
        return Vec::new();
    }
    let ids = obs
        .steps
        .last()
        .map(|s| vec![s.id.clone()])
        .unwrap_or_default();
    vec![Finding::new(
        FindingKind::Stall,
        Severity::Critical,
        format!(
            "no step/heartbeat for {}s (threshold {}s)",
            (obs.now - last) / 1000,
            obs.stale_ms / 1000
        ),
    )
    .with_ids(ids)]
}

fn detect_loop(obs: &Observation<'_>) -> Vec<Finding> {
    let tail: Vec<&StepRecord> = obs.steps.iter().rev().take(8).collect();
    let mut counts: HashMap<(&str, &str), Vec<String>> = HashMap::new();
    for step in &tail {
        if let Some(tool) = &step.tool {
            counts
                .entry((tool.name.as_str(), tool.args_hash.as_str()))
                .or_default()
                .push(step.id.clone());
        }
    }
    let mut out = Vec::new();
    for ((name, _), ids) in counts {
        if ids.len() >= 3 {
            out.push(
                Finding::new(
                    FindingKind::Loop,
                    Severity::Warn,
                    format!(
                        "tool `{name}` repeated {} times with identical args",
                        ids.len()
                    ),
                )
                .with_ids(ids),
            );
        }
    }
    out
}

fn detect_scope(obs: &Observation<'_>) -> Vec<Finding> {
    let Some(contract) = obs.contract else {
        return Vec::new();
    };
    if contract.allowed_paths.is_empty() {
        return Vec::new();
    }
    let mut out = Vec::new();
    for step in obs.steps.iter().rev().take(20) {
        for path in &step.targets {
            if !contract.path_allowed(path) {
                out.push(
                    Finding::new(
                        FindingKind::Scope,
                        Severity::Critical,
                        format!("path `{path}` is outside the contract's allow-list"),
                    )
                    .with_ids(vec![step.id.clone()]),
                );
            }
        }
        // `code_run` receives arbitrary shell text. The recorder deliberately
        // does not store that text, so a scoped contract cannot infer which
        // paths it touched. Do not stay silent: surface it as a warning and
        // let the existing approval/safety path decide.
        if step
            .tool
            .as_ref()
            .map(|t| t.name == "code_run" && step.targets.is_empty())
            .unwrap_or(false)
        {
            out.push(
                Finding::new(
                    FindingKind::Scope,
                    Severity::Warn,
                    "code_run path scope is not inferable; shell writes need approval/review",
                )
                .with_ids(vec![step.id.clone()]),
            );
        }
    }
    out
}

fn detect_idle(obs: &Observation<'_>) -> Vec<Finding> {
    let tail: Vec<&StepRecord> = obs.steps.iter().rev().take(5).collect();
    if tail.len() < 5 {
        return Vec::new();
    }
    let no_progress = tail
        .iter()
        .all(|s| s.artifacts.is_empty() && s.targets.is_empty());
    let only_reads = tail.iter().all(|s| {
        s.tool
            .as_ref()
            .map(|t| {
                matches!(
                    t.name.as_str(),
                    "read" | "ls" | "grep" | "glob" | "get_file_info"
                )
            })
            .unwrap_or(true)
    });
    if no_progress && only_reads {
        vec![Finding::new(
            FindingKind::Idle,
            Severity::Warn,
            "five consecutive read-only steps with no artifact progress",
        )
        .with_ids(tail.iter().map(|s| s.id.clone()).collect())]
    } else {
        Vec::new()
    }
}

fn detect_budget(obs: &Observation<'_>) -> Vec<Finding> {
    let Some(contract) = obs.contract else {
        return Vec::new();
    };
    let mut out = Vec::new();
    // Recorder stores cumulative context counters on every step, so summing
    // steps would count the same tokens many times. The high-water mark is
    // the correct budget signal.
    let tokens = obs
        .steps
        .iter()
        .map(|s| s.tokens_in.saturating_add(s.tokens_out))
        .max()
        .unwrap_or(0);
    if tokens > contract.budget.max_tokens {
        out.push(
            Finding::new(
                FindingKind::Budget,
                Severity::Critical,
                format!(
                    "token budget exceeded: {tokens} > {}",
                    contract.budget.max_tokens
                ),
            )
            .with_ids(
                obs.steps
                    .iter()
                    .rev()
                    .take(3)
                    .map(|s| s.id.clone())
                    .collect(),
            ),
        );
    }
    if obs.steps.len() as u32 > contract.budget.max_steps {
        out.push(
            Finding::new(
                FindingKind::Budget,
                Severity::Warn,
                format!(
                    "step budget exceeded: {} > {}",
                    obs.steps.len(),
                    contract.budget.max_steps
                ),
            )
            .with_ids(
                obs.steps
                    .iter()
                    .rev()
                    .take(3)
                    .map(|s| s.id.clone())
                    .collect(),
            ),
        );
    }
    out
}

fn detect_false_completion(obs: &Observation<'_>) -> Vec<Finding> {
    if obs.claimed_done && !obs.verified {
        let ids = obs
            .steps
            .iter()
            .filter(|s| {
                s.tool
                    .as_ref()
                    .map(|t| t.name == "code_run")
                    .unwrap_or(false)
            })
            .rev()
            .take(3)
            .map(|s| s.id.clone())
            .collect();
        return vec![Finding::new(
            FindingKind::FalseCompletion,
            Severity::Critical,
            "completion claimed without a passing verification step",
        )
        .with_ids(ids)];
    }
    Vec::new()
}

#[cfg(test)]
use oz_core_types::supervision::{StepStatus, ToolRecord};

#[cfg(test)]
fn tool(name: &str) -> ToolRecord {
    ToolRecord {
        name: name.into(),
        args_hash: format!("{name}-hash"),
        output_hash: None,
        duration_ms: Some(1),
        status: StepStatus::Succeeded,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::contract::{Acceptance, RiskLevel};
    use oz_core_types::supervision::Phase;

    fn step(id: &str, at: i64, tool_name: &str, target: &str) -> StepRecord {
        StepRecord {
            id: id.into(),
            ticket_id: "t".into(),
            session_id: "s".into(),
            turn: 1,
            phase: Phase::Acting,
            at,
            tool: Some(tool(tool_name)),
            status: StepStatus::Succeeded,
            artifacts: if target.is_empty() {
                vec![]
            } else {
                vec![target.into()]
            },
            targets: if target.is_empty() {
                vec![]
            } else {
                vec![target.into()]
            },
            plan_hash: None,
            git_sha: None,
            tokens_in: 10,
            tokens_out: 10,
        }
    }

    fn contract() -> TaskContract {
        let mut c = TaskContract::new(
            "t",
            "goal",
            vec![Acceptance {
                id: "a".into(),
                description: "a".into(),
            }],
            RiskLevel::Low,
            true,
        )
        .unwrap();
        c.allowed_paths = vec!["src".into()];
        c
    }

    #[test]
    fn stall_detected_after_threshold() {
        let steps = vec![step("s1", 1_000, "read", "src/a.rs")];
        let mut obs = Observation::new(&steps, 100_000);
        obs.heartbeat_at = Some(1_000);
        let findings = detect_all(&obs);
        assert!(findings.iter().any(|f| f.kind == FindingKind::Stall));
    }

    #[test]
    fn loop_detected_on_three_identical_calls() {
        let steps = vec![
            step("s1", 1, "read", "src/a.rs"),
            step("s2", 2, "read", "src/a.rs"),
            step("s3", 3, "read", "src/a.rs"),
        ];
        let findings = detect_all(&Observation::new(&steps, 4));
        assert!(findings.iter().any(|f| f.kind == FindingKind::Loop));
    }

    #[test]
    fn scope_violation_is_critical() {
        let c = contract();
        let steps = vec![step("s1", 1, "write", "Cargo.toml")];
        let mut obs = Observation::new(&steps, 2);
        obs.contract = Some(&c);
        let findings = detect_all(&obs);
        assert!(findings
            .iter()
            .any(|f| f.kind == FindingKind::Scope && f.severity == Severity::Critical));
    }

    #[test]
    fn code_run_in_scoped_contract_is_a_warning_not_silence() {
        let c = contract();
        let steps = vec![step("s1", 1, "code_run", "")];
        let mut obs = Observation::new(&steps, 2);
        obs.contract = Some(&c);
        let findings = detect_all(&obs);
        assert!(findings
            .iter()
            .any(|f| f.kind == FindingKind::Scope && f.severity == Severity::Warn));
    }

    #[test]
    fn idle_reads_are_a_warning() {
        let steps: Vec<StepRecord> = (0..5)
            .map(|i| step(&format!("s{i}"), i, "read", ""))
            .collect();
        let findings = detect_all(&Observation::new(&steps, 10));
        assert!(findings.iter().any(|f| f.kind == FindingKind::Idle));
    }

    #[test]
    fn budget_and_false_completion_detected() {
        let c = contract();
        let steps = vec![step("s1", 1, "read", "src/a.rs")];
        let mut obs = Observation::new(&steps, 2);
        obs.contract = Some(&c);
        obs.claimed_done = true;
        obs.verified = false;
        let findings = detect_all(&obs);
        assert!(findings
            .iter()
            .any(|f| f.kind == FindingKind::FalseCompletion));
    }

    #[test]
    fn budget_uses_high_water_mark_not_summed_snapshots() {
        let c = contract();
        let mut s1 = step("s1", 1, "read", "src/a.rs");
        s1.tokens_in = 1_500_000;
        let mut s2 = step("s2", 2, "read", "src/b.rs");
        s2.tokens_in = 2_100_000;
        let steps = [s1, s2];
        let mut obs = Observation::new(&steps, 3);
        obs.contract = Some(&c);
        let findings = detect_all(&obs);
        assert!(findings.iter().any(|f| f.kind == FindingKind::Budget));
    }

    #[test]
    fn healthy_run_has_no_findings() {
        let c = contract();
        let steps = vec![step("s1", 1, "write", "src/a.rs")];
        let mut obs = Observation::new(&steps, 2);
        obs.contract = Some(&c);
        obs.claimed_done = true;
        obs.verified = true;
        assert!(detect_all(&obs).is_empty());
    }
}
