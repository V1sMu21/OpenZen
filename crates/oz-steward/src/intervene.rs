//! Bounded correction ladder.
//!
//! L1 nudge → L2 re-anchor → L3 replan → L4 rollback (S4) → L5 stop.
//! This module only proposes and records the correction; executing it is the
//! scheduler/runtime's job. Budget and cooldown are enforced here so a noisy
//! detector cannot nudge on every tick.

use std::collections::HashMap;

use serde::Serialize;

use oz_core_types::supervision::{InterventionRecord, Verdict};
use uuid::Uuid;

use crate::detect::{Finding, FindingKind};
use crate::error::Result;
use crate::store::Store;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum CorrectionLevel {
    L1,
    L2,
    L3,
    L4,
    L5,
}

impl CorrectionLevel {
    pub fn as_str(self) -> &'static str {
        match self {
            CorrectionLevel::L1 => "L1",
            CorrectionLevel::L2 => "L2",
            CorrectionLevel::L3 => "L3",
            CorrectionLevel::L4 => "L4",
            CorrectionLevel::L5 => "L5",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum CorrectionAction {
    Nudge(String),
    Reanchor(String),
    Replan(String),
    Rollback(String),
    Stop(String),
}

impl CorrectionAction {
    pub fn level(&self) -> CorrectionLevel {
        match self {
            CorrectionAction::Nudge(_) => CorrectionLevel::L1,
            CorrectionAction::Reanchor(_) => CorrectionLevel::L2,
            CorrectionAction::Replan(_) => CorrectionLevel::L3,
            CorrectionAction::Rollback(_) => CorrectionLevel::L4,
            CorrectionAction::Stop(_) => CorrectionLevel::L5,
        }
    }

    pub fn text(&self) -> &str {
        match self {
            CorrectionAction::Nudge(t)
            | CorrectionAction::Reanchor(t)
            | CorrectionAction::Replan(t)
            | CorrectionAction::Rollback(t)
            | CorrectionAction::Stop(t) => t,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct CorrectionPlan {
    pub ticket_id: String,
    pub level: CorrectionLevel,
    pub action: CorrectionAction,
    pub reason: String,
    pub evidence_ids: Vec<String>,
}

pub struct CorrectionEngine {
    counts: HashMap<(String, CorrectionLevel), u32>,
    last_at: HashMap<(String, CorrectionLevel), i64>,
    cooldown_ms: i64,
    max_l1: u32,
}

impl Default for CorrectionEngine {
    fn default() -> Self {
        Self {
            counts: HashMap::new(),
            last_at: HashMap::new(),
            cooldown_ms: 60_000,
            max_l1: 2,
        }
    }
}

impl CorrectionEngine {
    pub fn with_cooldown(mut self, ms: i64) -> Self {
        self.cooldown_ms = ms.max(0);
        self
    }

    pub fn propose(
        &mut self,
        ticket_id: &str,
        verdict: Verdict,
        findings: &[Finding],
        now: i64,
    ) -> Option<CorrectionPlan> {
        if verdict == Verdict::OnTrack || verdict == Verdict::Blocked {
            return None;
        }
        let budget = findings.iter().any(|f| f.kind == FindingKind::Budget);
        let critical_scope = findings.iter().any(|f| {
            f.kind == FindingKind::Scope && f.severity == crate::detect::Severity::Critical
        });

        let level = if budget {
            CorrectionLevel::L5
        } else if verdict == Verdict::OffTrack {
            if self.count(ticket_id, CorrectionLevel::L3) > 0 {
                CorrectionLevel::L5
            } else if self.count(ticket_id, CorrectionLevel::L2) > 0 {
                CorrectionLevel::L3
            } else {
                CorrectionLevel::L2
            }
        } else if verdict == Verdict::Stalled {
            if self.count(ticket_id, CorrectionLevel::L1) < self.max_l1 {
                CorrectionLevel::L1
            } else {
                CorrectionLevel::L2
            }
        } else if self.count(ticket_id, CorrectionLevel::L1) < self.max_l1 {
            CorrectionLevel::L1
        } else if self.count(ticket_id, CorrectionLevel::L2) == 0 {
            CorrectionLevel::L2
        } else {
            CorrectionLevel::L3
        };

        let key = (ticket_id.to_string(), level);
        if let Some(prev) = self.last_at.get(&key) {
            if now.saturating_sub(*prev) < self.cooldown_ms {
                return None;
            }
        }

        let text = correction_text(level, verdict, critical_scope, findings);
        let action = match level {
            CorrectionLevel::L1 => CorrectionAction::Nudge(text),
            CorrectionLevel::L2 => CorrectionAction::Reanchor(text),
            CorrectionLevel::L3 => CorrectionAction::Replan(text),
            CorrectionLevel::L4 => CorrectionAction::Rollback(text),
            CorrectionLevel::L5 => CorrectionAction::Stop(text),
        };
        self.counts
            .insert(key.clone(), self.count(ticket_id, level) + 1);
        self.last_at.insert(key, now);

        Some(CorrectionPlan {
            ticket_id: ticket_id.to_string(),
            level,
            action,
            reason: findings
                .first()
                .map(|f| f.detail.clone())
                .unwrap_or_else(|| format!("{verdict:?}")),
            evidence_ids: findings
                .iter()
                .flat_map(|f| f.evidence_ids.clone())
                .collect(),
        })
    }

    fn count(&self, ticket_id: &str, level: CorrectionLevel) -> u32 {
        self.counts
            .get(&(ticket_id.to_string(), level))
            .copied()
            .unwrap_or(0)
    }

    pub fn record(&self, store: &Store, plan: &CorrectionPlan) -> Result<()> {
        store.record_intervention(&InterventionRecord {
            id: Uuid::new_v4().to_string(),
            ticket_id: plan.ticket_id.clone(),
            at: oz_core_types::supervision::now_ms(),
            level: plan.level.as_str().into(),
            reason: plan.reason.clone(),
            evidence_ids: plan.evidence_ids.clone(),
            result: plan.action.text().to_string(),
        })
    }
}

fn correction_text(
    level: CorrectionLevel,
    verdict: Verdict,
    critical_scope: bool,
    findings: &[Finding],
) -> String {
    let detail = findings
        .iter()
        .map(|f| f.detail.as_str())
        .collect::<Vec<_>>()
        .join("；");
    match level {
        CorrectionLevel::L1 => format!(
            "[L1 提醒] 当前 verdict={verdict:?}。先对照验收标准，说明下一步最小动作；不要重复同一个调用。{detail}"
        ),
        CorrectionLevel::L2 => format!(
            "[L2 重锚] 回到合同目标与验收项，停止当前路径；{} {detail}",
            if critical_scope {
                "禁止继续写入 allow-list 之外。"
            } else {
                "重新选择一条可验证的路径。"
            }
        ),
        CorrectionLevel::L3 => format!(
            "[L3 重规划] 当前策略连续失败。停写，重新列 3 步计划并选择最小可验证的一步。{detail}"
        ),
        CorrectionLevel::L4 => format!("[L4 回滚] 需要恢复到最后一个 good checkpoint。{detail}"),
        CorrectionLevel::L5 => format!(
            "[L5 停止] 预算/安全/连续失败达到上限，停止自动推进，等待人工确认。{detail}"
        ),
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
    fn at_risk_gets_l1_then_l2() {
        let mut e = CorrectionEngine::default();
        let f = vec![finding(FindingKind::Loop, Severity::Warn)];
        let a = e.propose("t", Verdict::AtRisk, &f, 1_000).unwrap();
        assert_eq!(a.level, CorrectionLevel::L1);
        let b = e.propose("t", Verdict::AtRisk, &f, 61_000).unwrap();
        assert_eq!(b.level, CorrectionLevel::L1);
        let c = e.propose("t", Verdict::AtRisk, &f, 121_000).unwrap();
        assert_eq!(c.level, CorrectionLevel::L2);
    }

    #[test]
    fn off_track_escalates_to_replan_then_stop() {
        let mut e = CorrectionEngine::default();
        let f = vec![finding(FindingKind::Scope, Severity::Critical)];
        let a = e.propose("t", Verdict::OffTrack, &f, 1_000).unwrap();
        assert_eq!(a.level, CorrectionLevel::L2);
        let b = e.propose("t", Verdict::OffTrack, &f, 61_000).unwrap();
        assert_eq!(b.level, CorrectionLevel::L3);
        let c = e.propose("t", Verdict::OffTrack, &f, 121_000).unwrap();
        assert_eq!(c.level, CorrectionLevel::L5);
    }

    #[test]
    fn budget_stops_immediately() {
        let mut e = CorrectionEngine::default();
        let f = vec![finding(FindingKind::Budget, Severity::Critical)];
        let a = e.propose("t", Verdict::AtRisk, &f, 1_000).unwrap();
        assert_eq!(a.level, CorrectionLevel::L5);
    }

    #[test]
    fn cooldown_prevents_repeat_of_same_level() {
        let mut e = CorrectionEngine::default();
        let f = vec![finding(FindingKind::Loop, Severity::Warn)];
        assert!(e.propose("t", Verdict::AtRisk, &f, 1_000).is_some());
        assert!(e.propose("t", Verdict::AtRisk, &f, 1_001).is_none());
    }
}
