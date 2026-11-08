//! Deterministic verdict + L1 nudge. The model is not allowed to turn a
//! deterministic critical finding into "on track"; semantic judging can only
//! add findings, never subtract them.

use oz_core_types::supervision::Verdict;
use serde::Serialize;

use crate::detect::{Finding, FindingKind, Severity};

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct DeterministicJudgment {
    pub verdict: Verdict,
    pub confidence: f32,
    pub findings: Vec<Finding>,
    pub action: Option<Action>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub enum Action {
    /// L1: a short, evidence-backed nudge; no tool permissions change.
    Nudge(String),
    /// L2+: the caller must re-anchor/replan; this type does not execute it.
    Escalate,
}

pub fn deterministic(findings: Vec<Finding>) -> DeterministicJudgment {
    if findings.is_empty() {
        return DeterministicJudgment {
            verdict: Verdict::OnTrack,
            confidence: 0.9,
            findings,
            action: None,
        };
    }

    let critical = findings.iter().any(|f| f.severity == Severity::Critical);
    let stalled = findings.iter().any(|f| f.kind == FindingKind::Stall);
    let scope_or_false = findings
        .iter()
        .any(|f| matches!(f.kind, FindingKind::Scope | FindingKind::FalseCompletion));
    let budget = findings.iter().any(|f| f.kind == FindingKind::Budget);

    let verdict = if scope_or_false {
        Verdict::OffTrack
    } else if stalled {
        Verdict::Stalled
    } else if budget {
        Verdict::AtRisk
    } else if critical {
        Verdict::OffTrack
    } else {
        Verdict::AtRisk
    };
    let confidence = if scope_or_false {
        0.95
    } else if stalled || critical {
        0.9
    } else if budget {
        0.85
    } else {
        0.7
    };
    let action = match verdict {
        Verdict::OffTrack => Some(Action::Escalate),
        Verdict::Stalled => Some(Action::Nudge(nudge_text(Verdict::Stalled, &findings))),
        Verdict::AtRisk => Some(Action::Nudge(nudge_text(Verdict::AtRisk, &findings))),
        _ => None,
    };

    DeterministicJudgment {
        verdict,
        confidence,
        findings,
        action,
    }
}

pub fn verdict_rank(verdict: Verdict) -> u8 {
    match verdict {
        Verdict::OnTrack => 0,
        Verdict::AtRisk => 1,
        Verdict::Blocked => 2,
        Verdict::Stalled => 3,
        Verdict::OffTrack => 4,
    }
}

pub fn max_verdict(a: Verdict, b: Verdict) -> Verdict {
    if verdict_rank(a) >= verdict_rank(b) {
        a
    } else {
        b
    }
}

pub fn nudge_text(verdict: Verdict, findings: &[Finding]) -> String {
    let details = findings
        .iter()
        .map(|f| format!("- {:?}: {}", f.kind, f.detail))
        .collect::<Vec<_>>()
        .join("\n");
    match verdict {
        Verdict::Stalled => format!(
            "过去一段时间没有新的步骤或心跳。先说明当前卡在哪一步，再继续；不要重复同一个调用。\n{details}"
        ),
        Verdict::AtRisk => format!(
            "检测到偏离验收的信号。先对照目标和验收标准，说明下一步的最小可验证动作。\n{details}"
        ),
        Verdict::OffTrack => format!(
            "当前动作与合同/验收不一致，已停止自动推进，等待人工确认。\n{details}"
        ),
        Verdict::Blocked => format!("当前任务被阻塞，需要你补充信息或做决定。\n{details}"),
        Verdict::OnTrack => "继续。".into(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::detect::{Finding, FindingKind, Severity};

    fn finding(kind: FindingKind, severity: Severity) -> Finding {
        Finding {
            kind,
            severity,
            evidence_ids: vec!["s1".into()],
            detail: "detail".into(),
        }
    }

    #[test]
    fn no_findings_is_on_track() {
        let j = deterministic(vec![]);
        assert_eq!(j.verdict, Verdict::OnTrack);
        assert!(j.action.is_none());
    }

    #[test]
    fn scope_is_off_track_even_if_other_findings_are_warn() {
        let j = deterministic(vec![
            finding(FindingKind::Loop, Severity::Warn),
            finding(FindingKind::Scope, Severity::Critical),
        ]);
        assert_eq!(j.verdict, Verdict::OffTrack);
        assert_eq!(j.action, Some(Action::Escalate));
    }

    #[test]
    fn stall_gets_l1_nudge() {
        let j = deterministic(vec![finding(FindingKind::Stall, Severity::Critical)]);
        assert_eq!(j.verdict, Verdict::Stalled);
        match j.action {
            Some(Action::Nudge(text)) => assert!(text.contains("没有新的步骤")),
            other => panic!("unexpected action: {other:?}"),
        }
    }

    #[test]
    fn false_completion_is_off_track() {
        let j = deterministic(vec![finding(
            FindingKind::FalseCompletion,
            Severity::Critical,
        )]);
        assert_eq!(j.verdict, Verdict::OffTrack);
        assert!(j.action.is_some());
    }

    #[test]
    fn budget_is_at_risk() {
        let j = deterministic(vec![finding(FindingKind::Budget, Severity::Critical)]);
        assert_eq!(j.verdict, Verdict::AtRisk);
    }
}
