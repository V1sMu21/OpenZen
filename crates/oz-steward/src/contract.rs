//! Task contracts: the stable definition of "done" that a ticket is judged
//! against. A contract is immutable per version; changing the goal or the
//! acceptance criteria requires a new version and (for high risk) explicit
//! user confirmation.

use serde::{Deserialize, Serialize};

use crate::error::{Result, StewardError};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum RiskLevel {
    #[default]
    Low,
    Medium,
    High,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ContractBudget {
    pub max_tokens: u64,
    pub max_steps: u32,
    pub max_wall_secs: u64,
}

impl Default for ContractBudget {
    fn default() -> Self {
        Self {
            max_tokens: 2_000_000,
            max_steps: 200,
            max_wall_secs: 7_200,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Acceptance {
    pub id: String,
    pub description: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TaskContract {
    pub ticket_id: String,
    pub version: u32,
    pub goal: String,
    #[serde(default)]
    pub non_goals: Vec<String>,
    #[serde(default)]
    pub acceptance: Vec<Acceptance>,
    /// Empty means "no path restriction" (legacy/normal chat tickets).
    #[serde(default)]
    pub allowed_paths: Vec<String>,
    #[serde(default)]
    pub budget: ContractBudget,
    #[serde(default)]
    pub risk: RiskLevel,
    pub created_at: i64,
    #[serde(default)]
    pub confirmed: bool,
}

impl TaskContract {
    pub fn new(
        ticket_id: impl Into<String>,
        goal: impl Into<String>,
        acceptance: Vec<Acceptance>,
        risk: RiskLevel,
        confirmed: bool,
    ) -> Result<Self> {
        let contract = Self {
            ticket_id: ticket_id.into(),
            version: 1,
            goal: goal.into(),
            non_goals: Vec::new(),
            acceptance,
            allowed_paths: Vec::new(),
            budget: ContractBudget::default(),
            risk,
            created_at: oz_core_types::supervision::now_ms(),
            confirmed,
        };
        contract.validate()?;
        Ok(contract)
    }

    pub fn validate(&self) -> Result<()> {
        if self.ticket_id.trim().is_empty() {
            return Err(StewardError::Invalid("ticket_id is empty".into()));
        }
        if self.goal.trim().is_empty() {
            return Err(StewardError::Invalid("goal is empty".into()));
        }
        if self.version == 0 {
            return Err(StewardError::Invalid("version must be >= 1".into()));
        }
        if self.acceptance.is_empty() {
            return Err(StewardError::Invalid(
                "at least one acceptance criterion is required".into(),
            ));
        }
        if self.risk == RiskLevel::High && !self.confirmed {
            return Err(StewardError::Invalid(
                "high-risk contract requires user confirmation".into(),
            ));
        }
        for a in &self.acceptance {
            if a.id.trim().is_empty() || a.description.trim().is_empty() {
                return Err(StewardError::Invalid(
                    "acceptance id/description cannot be empty".into(),
                ));
            }
        }
        Ok(())
    }

    /// Empty allow-list = unrestricted. Otherwise a path must be equal to or
    /// nested under one of the allowed prefixes.
    pub fn path_allowed(&self, path: &str) -> bool {
        if self.allowed_paths.is_empty() {
            return true;
        }
        let path = path.trim_start_matches("./");
        self.allowed_paths.iter().any(|allowed| {
            let allowed = allowed
                .trim()
                .trim_end_matches('/')
                .trim_start_matches("./");
            path == allowed || path.starts_with(&format!("{allowed}/"))
        })
    }

    pub fn next_version(&self) -> Self {
        let mut next = self.clone();
        next.version = self.version.saturating_add(1);
        next.confirmed = false;
        next.created_at = oz_core_types::supervision::now_ms();
        next
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn acceptance() -> Vec<Acceptance> {
        vec![Acceptance {
            id: "tests".into(),
            description: "cargo test passes".into(),
        }]
    }

    #[test]
    fn high_risk_requires_confirmation() {
        let err =
            TaskContract::new("t1", "ship it", acceptance(), RiskLevel::High, false).unwrap_err();
        assert!(err.to_string().contains("confirmation"));
        assert!(TaskContract::new("t1", "ship it", acceptance(), RiskLevel::High, true).is_ok());
    }

    #[test]
    fn empty_goal_or_acceptance_is_rejected() {
        assert!(TaskContract::new("t1", " ", acceptance(), RiskLevel::Low, true).is_err());
        assert!(TaskContract::new("t1", "goal", vec![], RiskLevel::Low, true).is_err());
    }

    #[test]
    fn path_allow_list_matches_prefix_only() {
        let mut c = TaskContract::new("t1", "goal", acceptance(), RiskLevel::Low, true).unwrap();
        c.allowed_paths = vec!["src".into(), "docs/".into()];
        assert!(c.path_allowed("src/lib.rs"));
        assert!(c.path_allowed("docs/a.md"));
        assert!(c.path_allowed("./src/lib.rs"));
        assert!(!c.path_allowed("src2/lib.rs"));
        assert!(!c.path_allowed("Cargo.toml"));
    }

    #[test]
    fn empty_allow_list_is_unrestricted() {
        let c = TaskContract::new("t1", "goal", acceptance(), RiskLevel::Low, true).unwrap();
        assert!(c.path_allowed("anything/at/all"));
    }

    #[test]
    fn next_version_resets_confirmation() {
        let c = TaskContract::new("t1", "goal", acceptance(), RiskLevel::High, true).unwrap();
        let next = c.next_version();
        assert_eq!(next.version, 2);
        assert!(!next.confirmed);
        assert!(next.validate().is_err());
    }
}
