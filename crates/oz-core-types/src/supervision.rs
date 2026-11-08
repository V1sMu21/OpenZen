//! Steward supervision records.
//!
//! These are deliberately plain data types: the code that writes them is the
//! runtime (agent loop / Tauri collector), never the model. Keeping them in
//! `oz-core-types` lets the store, detectors and UI share one schema without
//! pulling `oz-steward` into the core loop.

use serde::{Deserialize, Serialize};

/// What the worker is doing at a step boundary.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum Phase {
    #[default]
    Planning,
    Acting,
    Verifying,
    Reporting,
    Done,
}

/// Outcome of a single step/tool call.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum StepStatus {
    #[default]
    Started,
    Succeeded,
    Failed,
    Skipped,
    Blocked,
}

/// Tool call details attached to a step. Hashes, not raw args/output, so the
/// ledger can be shown to the model/user without leaking secrets or files.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct ToolRecord {
    pub name: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub args_hash: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub output_hash: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub duration_ms: Option<u64>,
    #[serde(default)]
    pub status: StepStatus,
}

/// One immutable line in the flight recorder.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StepRecord {
    pub id: String,
    pub ticket_id: String,
    pub session_id: String,
    pub turn: u32,
    pub phase: Phase,
    /// Unix milliseconds.
    pub at: i64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool: Option<ToolRecord>,
    #[serde(default)]
    pub status: StepStatus,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub artifacts: Vec<String>,
    /// Paths named in the tool arguments (e.g. write's `file_path`). Kept
    /// separate from artifacts so the scope detector can check what a tool
    /// actually touched even when the tool never opens an artifact.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub targets: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub plan_hash: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub git_sha: Option<String>,
    #[serde(default)]
    pub tokens_in: u64,
    #[serde(default)]
    pub tokens_out: u64,
}

/// The lifecycle of a supervised ticket. `Stalled` and `Failed` are terminal
/// until a human resumes/reopens; `Blocked` is recoverable.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum TicketStatus {
    #[default]
    Draft,
    Active,
    Verifying,
    Blocked,
    Stalled,
    /// The run ended normally but no completion gate was verified. This is
    /// terminal, but it is deliberately distinct from `Achieved`.
    Completed,
    Achieved,
    Exhausted,
    Abandoned,
    Failed,
}

impl TicketStatus {
    pub fn is_terminal(self) -> bool {
        matches!(
            self,
            TicketStatus::Completed
                | TicketStatus::Achieved
                | TicketStatus::Exhausted
                | TicketStatus::Abandoned
                | TicketStatus::Failed
        )
    }

    pub fn needs_attention(self) -> bool {
        matches!(
            self,
            TicketStatus::Blocked | TicketStatus::Stalled | TicketStatus::Exhausted
        )
    }
}

/// A typed verdict from the decision layer or the deterministic detectors.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum Verdict {
    #[default]
    OnTrack,
    AtRisk,
    OffTrack,
    Stalled,
    Blocked,
}

/// One judgment written by the steward. `evidence_ids` must point at step ids.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct JudgmentRecord {
    pub id: String,
    pub ticket_id: String,
    pub at: i64,
    pub provider: String,
    pub question: String,
    pub verdict: Verdict,
    pub confidence: f32,
    #[serde(default)]
    pub evidence_ids: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub action: Option<String>,
}

/// One correction applied (or proposed) by the steward.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct InterventionRecord {
    pub id: String,
    pub ticket_id: String,
    pub at: i64,
    /// L1..L5
    pub level: String,
    pub reason: String,
    #[serde(default)]
    pub evidence_ids: Vec<String>,
    #[serde(default)]
    pub result: String,
}

/// Compact view used by the watch panel / inbox.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TicketSnapshot {
    pub id: String,
    pub session_id: String,
    pub title: String,
    pub status: TicketStatus,
    pub phase: Phase,
    pub created_at: i64,
    pub updated_at: i64,
    pub heartbeat_at: Option<i64>,
    pub last_verdict: Option<Verdict>,
    pub step_count: u64,
    pub last_step_at: Option<i64>,
}

/// Unix milliseconds, saturating on pre-epoch clocks.
pub fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn step_record_roundtrips_with_optional_fields() {
        let step = StepRecord {
            id: "s1".into(),
            ticket_id: "t1".into(),
            session_id: "sess".into(),
            turn: 3,
            phase: Phase::Acting,
            at: 1_700_000_000_000,
            tool: Some(ToolRecord {
                name: "write".into(),
                args_hash: "abc".into(),
                output_hash: Some("def".into()),
                duration_ms: Some(12),
                status: StepStatus::Succeeded,
            }),
            status: StepStatus::Succeeded,
            artifacts: vec!["src/lib.rs".into()],
            targets: vec!["src/lib.rs".into()],
            plan_hash: Some("plan".into()),
            git_sha: None,
            tokens_in: 10,
            tokens_out: 20,
        };
        let json = serde_json::to_string(&step).unwrap();
        let back: StepRecord = serde_json::from_str(&json).unwrap();
        assert_eq!(step, back);
        assert!(!json.contains("git_sha"));
    }

    #[test]
    fn exhausted_is_terminal_but_still_needs_attention() {
        // Exhausted is a terminal outcome, but unlike Achieved it still needs
        // a human decision (extend budget / take over), so both flags are
        // intentionally true.
        assert!(TicketStatus::Exhausted.is_terminal());
        assert!(TicketStatus::Exhausted.needs_attention());

        for status in [
            TicketStatus::Completed,
            TicketStatus::Achieved,
            TicketStatus::Abandoned,
            TicketStatus::Failed,
        ] {
            assert!(status.is_terminal());
            assert!(!status.needs_attention());
        }
        assert!(TicketStatus::Achieved.is_terminal());
        assert!(TicketStatus::Blocked.needs_attention());
    }

    #[test]
    fn phase_defaults_to_planning() {
        assert_eq!(Phase::default(), Phase::Planning);
        assert_eq!(StepStatus::default(), StepStatus::Started);
    }

    #[test]
    fn old_records_without_optional_fields_deserialize() {
        let json = r#"{
            "id":"s","ticket_id":"t","session_id":"x","turn":0,
            "phase":"acting","at":1,"status":"succeeded"
        }"#;
        let step: StepRecord = serde_json::from_str(json).unwrap();
        assert!(step.tool.is_none());
        assert!(step.artifacts.is_empty());
        assert!(step.targets.is_empty());
        assert_eq!(step.tokens_in, 0);
    }
}
