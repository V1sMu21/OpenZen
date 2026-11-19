//! Butler task model: a persistent, dependency-aware task graph.
//!
//! A task is the unit the butler schedules. It owns its acceptance gates, its
//! attempt budget and its evidence. Completion is never written by a worker:
//! only the verifier moves a task into a terminal state.

use serde::{Deserialize, Serialize};

use crate::error::{Result, StewardError};
use crate::goal::spec::{GateResult, GoalGate};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum TaskStatus {
    #[default]
    Queued,
    Ready,
    Running,
    Verifying,
    Verified,
    Unverified,
    Blocked,
    Failed,
}

impl TaskStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            TaskStatus::Queued => "queued",
            TaskStatus::Ready => "ready",
            TaskStatus::Running => "running",
            TaskStatus::Verifying => "verifying",
            TaskStatus::Verified => "verified",
            TaskStatus::Unverified => "unverified",
            TaskStatus::Blocked => "blocked",
            TaskStatus::Failed => "failed",
        }
    }

    pub fn parse(raw: &str) -> Result<Self> {
        Ok(match raw {
            "queued" => TaskStatus::Queued,
            "ready" => TaskStatus::Ready,
            "running" => TaskStatus::Running,
            "verifying" => TaskStatus::Verifying,
            "verified" => TaskStatus::Verified,
            "unverified" => TaskStatus::Unverified,
            "blocked" => TaskStatus::Blocked,
            "failed" => TaskStatus::Failed,
            other => return Err(StewardError::Invalid(format!("bad task status: {other}"))),
        })
    }

    pub fn is_terminal(self) -> bool {
        matches!(
            self,
            TaskStatus::Verified | TaskStatus::Unverified | TaskStatus::Failed
        )
    }

    /// The scheduler owns every transition; this table is the contract.
    pub fn can_transition(self, next: TaskStatus) -> bool {
        if self == next {
            return true;
        }
        if self.is_terminal() {
            return false;
        }
        matches!(
            (self, next),
            (TaskStatus::Queued, TaskStatus::Ready | TaskStatus::Blocked)
                | (
                    TaskStatus::Ready,
                    TaskStatus::Running | TaskStatus::Blocked | TaskStatus::Failed
                )
                | (
                    TaskStatus::Running,
                    TaskStatus::Verifying | TaskStatus::Blocked
                )
                | (
                    TaskStatus::Verifying,
                    TaskStatus::Verified
                        | TaskStatus::Unverified
                        | TaskStatus::Ready
                        | TaskStatus::Failed
                        | TaskStatus::Blocked
                )
                | (TaskStatus::Blocked, TaskStatus::Ready | TaskStatus::Failed)
        )
    }
}

/// Re-exported so callers have one definition of model provenance.
pub use oz_core_types::session_role::ModelSource;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TaskBudget {
    pub max_tokens: u64,
    pub max_wall_secs: u64,
}

impl Default for TaskBudget {
    fn default() -> Self {
        Self {
            max_tokens: 400_000,
            max_wall_secs: 1_800,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Task {
    pub id: String,
    pub project_id: String,
    /// Human-readable project name when the task is bound to a project.
    #[serde(default)]
    pub project_name: Option<String>,
    pub butler_session: String,
    pub goal: String,
    #[serde(default)]
    pub detail: String,
    #[serde(default)]
    pub deps: Vec<String>,
    /// When true, an Unverified dependency keeps this task queued instead of
    /// being consumed as usable output. Opt-in: the default preserves the
    /// historical "weak evidence still unblocks" behaviour.
    #[serde(default)]
    pub require_verified_deps: bool,
    pub accept: Vec<GoalGate>,
    pub status: TaskStatus,
    #[serde(default)]
    pub owner_session: Option<String>,
    #[serde(default)]
    pub attempt: u32,
    #[serde(default = "default_max_attempts")]
    pub max_attempts: u32,
    #[serde(default)]
    pub budget: TaskBudget,
    /// Per-task model override (mykey.toml section or model name).
    #[serde(default)]
    pub model: Option<String>,
    /// How much damage this task can do; drives the approval policy.
    #[serde(default)]
    pub reversibility: Reversibility,
    #[serde(default)]
    pub evidence: Vec<GateResult>,
    #[serde(default)]
    pub assumptions: Vec<String>,
    #[serde(default)]
    pub skipped: Vec<String>,
    pub created_at: i64,
    pub updated_at: i64,
}

fn default_max_attempts() -> u32 {
    3
}

impl Task {
    pub fn new(
        project_id: impl Into<String>,
        butler_session: impl Into<String>,
        goal: impl Into<String>,
    ) -> Self {
        let now = oz_core_types::supervision::now_ms();
        Self {
            id: uuid::Uuid::new_v4().to_string(),
            project_id: project_id.into(),
            project_name: None,
            butler_session: butler_session.into(),
            goal: goal.into(),
            detail: String::new(),
            deps: Vec::new(),
            require_verified_deps: false,
            accept: Vec::new(),
            status: TaskStatus::Queued,
            owner_session: None,
            attempt: 0,
            max_attempts: default_max_attempts(),
            budget: TaskBudget::default(),
            model: None,
            reversibility: Reversibility::WorkspaceWrite,
            evidence: Vec::new(),
            assumptions: Vec::new(),
            skipped: Vec::new(),
            created_at: now,
            updated_at: now,
        }
    }

    pub fn validate(&self) -> Result<()> {
        if self.goal.trim().is_empty() {
            return Err(StewardError::Invalid("task goal is empty".into()));
        }
        if self.max_attempts == 0 {
            return Err(StewardError::Invalid("max_attempts must be > 0".into()));
        }
        if self.deps.iter().any(|d| d == &self.id) {
            return Err(StewardError::Invalid("task cannot depend on itself".into()));
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TaskEvent {
    pub id: String,
    pub task_id: String,
    pub at: i64,
    pub kind: String,
    pub detail: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum ApprovalStatus {
    #[default]
    Pending,
    Approved,
    Denied,
    Expired,
}

impl ApprovalStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            ApprovalStatus::Pending => "pending",
            ApprovalStatus::Approved => "approved",
            ApprovalStatus::Denied => "denied",
            ApprovalStatus::Expired => "expired",
        }
    }

    pub fn parse(raw: &str) -> Result<Self> {
        Ok(match raw {
            "pending" => ApprovalStatus::Pending,
            "approved" => ApprovalStatus::Approved,
            "denied" => ApprovalStatus::Denied,
            "expired" => ApprovalStatus::Expired,
            other => {
                return Err(StewardError::Invalid(format!(
                    "bad approval status: {other}"
                )))
            }
        })
    }
}

/// How much damage a task can do if it goes wrong. The approval policy is
/// keyed on this, not on the task text.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum Reversibility {
    #[default]
    WorkspaceWrite,
    ReadOnly,
    OutsideWrite,
    ExternalEffect,
    Destructive,
}

impl Reversibility {
    pub fn as_str(self) -> &'static str {
        match self {
            Reversibility::ReadOnly => "read_only",
            Reversibility::WorkspaceWrite => "workspace_write",
            Reversibility::OutsideWrite => "outside_write",
            Reversibility::ExternalEffect => "external_effect",
            Reversibility::Destructive => "destructive",
        }
    }

    pub fn parse(raw: &str) -> Result<Self> {
        Ok(match raw {
            "read_only" => Reversibility::ReadOnly,
            "workspace_write" => Reversibility::WorkspaceWrite,
            "outside_write" => Reversibility::OutsideWrite,
            "external_effect" => Reversibility::ExternalEffect,
            "destructive" => Reversibility::Destructive,
            other => return Err(StewardError::Invalid(format!("bad reversibility: {other}"))),
        })
    }

    /// Only these need a human. Everything else runs.
    pub fn needs_approval(self) -> bool {
        matches!(
            self,
            Reversibility::OutsideWrite
                | Reversibility::ExternalEffect
                | Reversibility::Destructive
        )
    }
}

/// One dispatched worker execution. Persisted so the console can show history
/// even after the child session row is cleaned up.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Run {
    pub run_id: String,
    pub task_id: String,
    pub project_id: String,
    pub child_id: String,
    pub model: String,
    pub started_at: i64,
    #[serde(default)]
    pub finished_at: Option<i64>,
    pub status: String,
    #[serde(default)]
    pub summary: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Approval {
    pub id: String,
    pub task_id: String,
    pub action: String,
    #[serde(default)]
    pub impact: String,
    #[serde(default)]
    pub reversibility: String,
    pub status: ApprovalStatus,
    pub created_at: i64,
    #[serde(default)]
    pub decided_at: Option<i64>,
}

impl Approval {
    pub fn new(task_id: impl Into<String>, action: impl Into<String>) -> Self {
        Self {
            id: uuid::Uuid::new_v4().to_string(),
            task_id: task_id.into(),
            action: action.into(),
            impact: String::new(),
            reversibility: Reversibility::OutsideWrite.as_str().into(),
            status: ApprovalStatus::Pending,
            created_at: oz_core_types::supervision::now_ms(),
            decided_at: None,
        }
    }
}

/// Gate strength: `verified` needs at least one Strong pass. A file that
/// merely exists must never be enough to claim success.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GateStrength {
    None,
    Weak,
    Medium,
    Strong,
}

pub fn command_is_known_test_runner(command: &str) -> bool {
    let c = command.trim().to_ascii_lowercase();
    [
        "cargo test",
        "cargo check",
        "cargo build",
        "cargo clippy",
        "npm test",
        "npm run build",
        "pnpm test",
        "pnpm build",
        "yarn test",
        "yarn build",
        "pytest",
        "python3 -m pytest",
        "python -m pytest",
        "go test",
        "go build",
        "make test",
        "tsc ",
        "tsc --noemit",
        "node --check",
        "mvn test",
        "gradle test",
    ]
    .iter()
    .any(|prefix| c.starts_with(prefix))
}

/// A verification command must be a single command. Shell chaining would let
/// `cargo test; rm -rf …` pass the prefix allowlist, so operators and
/// redirects are refused outright.
pub fn verify_command_shape_is_safe(command: &str) -> bool {
    ![";", "&", "|", "`", "$(", ">", "<", "\n", "\r"]
        .iter()
        .any(|token| command.contains(token))
}

/// Explicit user opt-in for verification commands outside the known test
/// runners. The environment is process-level, so a model/prompt cannot grant
/// itself this permission.
pub fn custom_verify_commands_allowed() -> bool {
    matches!(
        std::env::var("OPENZEN_ALLOW_CUSTOM_VERIFY").as_deref(),
        Ok("1") | Ok("true") | Ok("yes")
    )
}

pub fn gate_strength(gate: &GoalGate) -> GateStrength {
    match gate {
        GoalGate::Command { command, .. } => {
            if command_is_known_test_runner(command) {
                GateStrength::Strong
            } else {
                GateStrength::Medium
            }
        }
        GoalGate::StrongCommand { .. } => GateStrength::Strong,
        GoalGate::FileAssertion { .. } | GoalGate::FileContent { .. } => GateStrength::Weak,
        GoalGate::SourceCheck { .. } => GateStrength::Strong,
        GoalGate::Review { .. } => GateStrength::Medium,
        GoalGate::Human { .. } => GateStrength::None,
        GoalGate::All { gates } => gates
            .iter()
            .map(gate_strength)
            .max()
            .unwrap_or(GateStrength::None),
        GoalGate::Any { gates } => gates
            .iter()
            .map(gate_strength)
            .min()
            .unwrap_or(GateStrength::None),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn status_terminality_and_transitions() {
        assert!(TaskStatus::Verified.is_terminal());
        assert!(!TaskStatus::Blocked.is_terminal());
        assert!(TaskStatus::Ready.can_transition(TaskStatus::Running));
        assert!(TaskStatus::Verifying.can_transition(TaskStatus::Ready));
        assert!(TaskStatus::Verifying.can_transition(TaskStatus::Unverified));
        assert!(!TaskStatus::Verified.can_transition(TaskStatus::Ready));
        assert!(!TaskStatus::Queued.can_transition(TaskStatus::Running));
        for s in [
            TaskStatus::Queued,
            TaskStatus::Ready,
            TaskStatus::Running,
            TaskStatus::Verifying,
            TaskStatus::Verified,
            TaskStatus::Unverified,
            TaskStatus::Blocked,
            TaskStatus::Failed,
        ] {
            assert_eq!(TaskStatus::parse(s.as_str()).unwrap(), s);
        }
    }

    #[test]
    fn source_check_is_a_strong_gate() {
        let gate = GoalGate::SourceCheck {
            path: "sources.md".into(),
            min_sources: 3,
        };
        assert_eq!(gate_strength(&gate), GateStrength::Strong);
    }

    #[test]
    fn approval_policy_only_for_irreversible() {
        assert!(!Reversibility::ReadOnly.needs_approval());
        assert!(!Reversibility::WorkspaceWrite.needs_approval());
        assert!(Reversibility::OutsideWrite.needs_approval());
        assert!(Reversibility::ExternalEffect.needs_approval());
        assert!(Reversibility::Destructive.needs_approval());
    }

    #[test]
    fn strong_gate_is_required_for_verified() {
        assert_eq!(
            gate_strength(&GoalGate::Command {
                command: "true".into(),
                timeout_secs: 1,
                retries: 0,
            }),
            GateStrength::Medium,
            "a no-op command is not strong evidence"
        );
        assert_eq!(
            gate_strength(&GoalGate::Command {
                command: "cargo test --quiet".into(),
                timeout_secs: 1,
                retries: 0,
            }),
            GateStrength::Strong
        );
        assert_eq!(
            gate_strength(&GoalGate::FileAssertion {
                path: "a".into(),
                must_exist: true,
                contains: None,
            }),
            GateStrength::Weak
        );
    }

    #[test]
    fn task_validation_rejects_self_dependency() {
        let mut t = Task::new("p", "s", "do");
        t.deps.push(t.id.clone());
        assert!(t.validate().is_err());
        let mut ok = Task::new("p", "s", "do");
        ok.deps.push("x".into());
        assert!(ok.validate().is_ok());
    }
}
