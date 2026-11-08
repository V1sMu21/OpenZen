use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::error::{Result, StewardError};

pub const DEFAULT_MAX_ATTEMPTS: u32 = 8;
pub const DEFAULT_MAX_WALL_SECS: u64 = 7_200;
pub const DEFAULT_MAX_TOKENS: u64 = 2_000_000;
pub const DEFAULT_MAX_TURNS: u32 = 12;
pub const DEFAULT_MAX_FAILURES: u32 = 3;
pub const DEFAULT_MAX_NO_PROGRESS: u32 = 2;

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "snake_case")]
pub enum GoalStatus {
    #[default]
    Draft,
    Active,
    Paused,
    Verifying,
    Blocked,
    Achieved,
    Exhausted,
    Abandoned,
}

impl GoalStatus {
    pub fn can_transition(self, next: GoalStatus) -> bool {
        if self == next {
            return true;
        }
        matches!(
            (self, next),
            (
                GoalStatus::Draft,
                GoalStatus::Active | GoalStatus::Abandoned
            ) | (
                GoalStatus::Active,
                GoalStatus::Paused
                    | GoalStatus::Verifying
                    | GoalStatus::Blocked
                    | GoalStatus::Exhausted
                    | GoalStatus::Abandoned
            ) | (
                GoalStatus::Paused,
                GoalStatus::Active | GoalStatus::Abandoned
            ) | (
                GoalStatus::Verifying,
                GoalStatus::Achieved
                    | GoalStatus::Blocked
                    | GoalStatus::Paused
                    | GoalStatus::Exhausted
                    | GoalStatus::Abandoned
            ) | (
                GoalStatus::Blocked,
                GoalStatus::Active | GoalStatus::Abandoned
            )
        )
    }

    pub fn is_terminal(self) -> bool {
        matches!(
            self,
            GoalStatus::Achieved | GoalStatus::Exhausted | GoalStatus::Abandoned
        )
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct GoalBudget {
    pub max_attempts: u32,
    pub max_wall_secs: u64,
    pub max_tokens: u64,
    pub max_turns_per_attempt: u32,
    pub max_consecutive_failures: u32,
    pub max_no_progress_rounds: u32,
}

impl Default for GoalBudget {
    fn default() -> Self {
        Self {
            max_attempts: DEFAULT_MAX_ATTEMPTS,
            max_wall_secs: DEFAULT_MAX_WALL_SECS,
            max_tokens: DEFAULT_MAX_TOKENS,
            max_turns_per_attempt: DEFAULT_MAX_TURNS,
            max_consecutive_failures: DEFAULT_MAX_FAILURES,
            max_no_progress_rounds: DEFAULT_MAX_NO_PROGRESS,
        }
    }
}

impl GoalBudget {
    pub fn validate(&self) -> Result<()> {
        if !(1..=50).contains(&self.max_attempts) {
            return Err(StewardError::Invalid("max_attempts must be 1..=50".into()));
        }
        if !(1..=86_400).contains(&self.max_wall_secs) {
            return Err(StewardError::Invalid(
                "max_wall_secs must be 1..=86400".into(),
            ));
        }
        if self.max_tokens == 0 {
            return Err(StewardError::Invalid("max_tokens must be > 0".into()));
        }
        if !(1..=100).contains(&self.max_turns_per_attempt) {
            return Err(StewardError::Invalid(
                "max_turns_per_attempt must be 1..=100".into(),
            ));
        }
        if !(1..=20).contains(&self.max_consecutive_failures) {
            return Err(StewardError::Invalid(
                "max_consecutive_failures must be 1..=20".into(),
            ));
        }
        if !(1..=10).contains(&self.max_no_progress_rounds) {
            return Err(StewardError::Invalid(
                "max_no_progress_rounds must be 1..=10".into(),
            ));
        }
        Ok(())
    }
}

fn default_timeout() -> u64 {
    120
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum GoalGate {
    Command {
        command: String,
        #[serde(default = "default_timeout")]
        timeout_secs: u64,
        #[serde(default)]
        retries: u32,
    },
    FileAssertion {
        path: String,
        #[serde(default = "default_true")]
        must_exist: bool,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        contains: Option<String>,
    },
    Review {
        rubric: String,
        #[serde(default = "default_confidence")]
        min_confidence: f32,
    },
    Human {
        prompt: String,
    },
    All {
        gates: Vec<GoalGate>,
    },
    Any {
        gates: Vec<GoalGate>,
    },
}

fn default_true() -> bool {
    true
}
fn default_confidence() -> f32 {
    0.8
}

impl GoalGate {
    pub fn human() -> Self {
        GoalGate::Human {
            prompt: "Human acceptance is required before this goal can be achieved.".into(),
        }
    }

    pub fn has_human(&self) -> bool {
        match self {
            GoalGate::Human { .. } => true,
            GoalGate::All { gates } | GoalGate::Any { gates } => {
                gates.iter().any(GoalGate::has_human)
            }
            _ => false,
        }
    }

    pub fn has_review(&self) -> bool {
        match self {
            GoalGate::Review { .. } => true,
            GoalGate::All { gates } | GoalGate::Any { gates } => {
                gates.iter().any(GoalGate::has_review)
            }
            _ => false,
        }
    }

    /// Snapshot JSON used to detect a contract edit; the runtime never lets a
    /// running goal silently loosen its gates.
    pub fn snapshot(&self) -> Value {
        serde_json::to_value(self).unwrap_or(Value::Null)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct GoalSpec {
    pub id: String,
    pub version: u32,
    pub title: String,
    pub objective: String,
    #[serde(default)]
    pub session_id: String,
    #[serde(default)]
    pub non_goals: Vec<String>,
    pub gates: Vec<GoalGate>,
    #[serde(default)]
    pub constraints: Vec<String>,
    pub budget: GoalBudget,
    #[serde(default = "default_true")]
    pub auto_continue: bool,
    #[serde(default)]
    pub require_human_acceptance: bool,
}

impl GoalSpec {
    pub fn new(
        id: impl Into<String>,
        title: impl Into<String>,
        objective: impl Into<String>,
    ) -> Self {
        Self {
            id: id.into(),
            version: 1,
            title: title.into(),
            objective: objective.into(),
            session_id: String::new(),
            non_goals: Vec::new(),
            gates: vec![GoalGate::human()],
            constraints: Vec::new(),
            budget: GoalBudget::default(),
            auto_continue: true,
            require_human_acceptance: false,
        }
    }

    pub fn add_gate(mut self, gate: GoalGate) -> Self {
        self.gates.push(gate);
        self
    }

    pub fn validate(&mut self) -> Result<()> {
        if self.id.trim().is_empty() {
            return Err(StewardError::Invalid("goal id is empty".into()));
        }
        if self.title.trim().is_empty() {
            return Err(StewardError::Invalid("goal title is empty".into()));
        }
        if self.objective.trim().is_empty() {
            return Err(StewardError::Invalid("goal objective is empty".into()));
        }
        if self.version == 0 {
            return Err(StewardError::Invalid("goal version must be >= 1".into()));
        }
        // An empty gate list gets the mandatory Human gate instead of
        // entering active with no acceptance at all.
        if self.gates.is_empty() {
            self.gates.push(GoalGate::human());
        }
        if self.require_human_acceptance && !self.gates.iter().any(GoalGate::has_human) {
            self.gates.push(GoalGate::human());
        }
        self.budget.validate()?;
        if let Some(gate) = self
            .gates
            .iter()
            .find(|g| matches!(g, GoalGate::Command { command, .. } if command.trim().is_empty()))
        {
            return Err(StewardError::Invalid(format!(
                "gate command is empty: {gate:?}"
            )));
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct GoalRecord {
    pub id: String,
    pub status: GoalStatus,
    pub current_version: u32,
    pub spec: GoalSpec,
    pub attempts: u32,
    pub consecutive_failures: u32,
    pub no_progress_rounds: u32,
    pub tokens_used: u64,
    pub wall_started_at: Option<i64>,
    pub last_report_id: Option<String>,
    pub created_at: i64,
    pub updated_at: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct GateResult {
    pub gate_index: usize,
    pub passed: bool,
    pub pending: bool,
    pub exit_code: Option<i32>,
    pub output: String,
    pub duration_ms: u64,
    pub evidence_id: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct GoalAttempt {
    pub goal_id: String,
    pub attempt_no: u32,
    pub started_at: i64,
    pub ended_at: i64,
    pub summary: String,
    pub outcome: String,
    pub failure_class: Option<String>,
    pub checkpoint_id: Option<String>,
    pub tokens: u64,
    pub gates: Vec<GateResult>,
    pub evidence_ids: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct GoalEvent {
    pub id: String,
    pub goal_id: String,
    pub at: i64,
    pub kind: String,
    pub detail: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct GoalReport {
    pub id: String,
    pub goal_id: String,
    pub status: GoalStatus,
    pub attempts: u32,
    pub gates: Vec<GateResult>,
    pub evidence_ids: Vec<String>,
    pub summary: String,
    pub next_actions: Vec<String>,
    pub created_at: i64,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_gates_get_human_gate() {
        let mut s = GoalSpec::new("g", "title", "objective");
        s.gates.clear();
        s.validate().unwrap();
        assert!(s.gates.iter().any(GoalGate::has_human));
    }

    #[test]
    fn budget_limits_are_enforced() {
        let mut s = GoalSpec::new("g", "title", "objective");
        s.budget.max_attempts = 0;
        assert!(s.validate().is_err());
        s.budget.max_attempts = 51;
        assert!(s.validate().is_err());
    }

    #[test]
    fn human_requirement_adds_gate() {
        let mut s = GoalSpec::new("g", "title", "objective");
        s.gates.clear();
        s.require_human_acceptance = true;
        s.validate().unwrap();
        assert!(s.gates.iter().any(GoalGate::has_human));
    }

    #[test]
    fn status_terminality() {
        assert!(GoalStatus::Achieved.is_terminal());
        assert!(GoalStatus::Exhausted.is_terminal());
        assert!(!GoalStatus::Blocked.is_terminal());
        assert!(!GoalStatus::Active.is_terminal());
    }
}
