use std::path::{Path, PathBuf};

use async_trait::async_trait;
use serde::{Deserialize, Serialize};

use crate::ReflectModule;

/// State file for a goal mode session.
#[derive(Debug, Serialize, Deserialize)]
pub struct GoalState {
    pub goal: String,
    pub status: String,
    pub start_time: chrono::DateTime<chrono::Utc>,
    pub budget_minutes: f64,
    pub turns_completed: u32,
}

impl GoalState {
    pub fn new(goal: String, budget_minutes: f64) -> Self {
        GoalState {
            goal,
            status: "running".to_string(),
            start_time: chrono::Utc::now(),
            budget_minutes,
            turns_completed: 0,
        }
    }

    pub fn elapsed_mins(&self) -> f64 {
        let elapsed = chrono::Utc::now() - self.start_time;
        elapsed.num_seconds() as f64 / 60.0
    }

    pub fn remaining_mins(&self) -> f64 {
        (self.budget_minutes - self.elapsed_mins()).max(0.0)
    }
}

/// Goal mode reflect module — budget-constrained self-driving agent.
pub struct GoalModeModule {
    state_file: PathBuf,
    max_budget_minutes: f64,
}

impl GoalModeModule {
    pub fn new(base_dir: &Path, max_budget_minutes: f64) -> Self {
        GoalModeModule {
            state_file: base_dir.join(".oz_goal_state.json"),
            max_budget_minutes,
        }
    }

    pub fn state_path(&self) -> &Path {
        &self.state_file
    }

    async fn load_state(&self) -> Option<GoalState> {
        if !self.state_file.exists() {
            return None;
        }
        let content = tokio::fs::read_to_string(&self.state_file).await.ok()?;
        serde_json::from_str(&content).ok()
    }

    async fn save_state(&self, state: &GoalState) -> Result<(), std::io::Error> {
        let content = serde_json::to_string_pretty(state).unwrap_or_default();
        tokio::fs::write(&self.state_file, content).await
    }

    /// Start a new goal mode session.
    pub async fn start_goal(&self, goal: String) -> Result<(), std::io::Error> {
        let state = GoalState::new(goal, self.max_budget_minutes);
        self.save_state(&state).await
    }

    /// Complete the current goal mode session.
    pub async fn complete_goal(&self) -> Result<(), std::io::Error> {
        if let Some(mut state) = self.load_state().await {
            state.status = "completed".to_string();
            self.save_state(&state).await?;
        }
        Ok(())
    }
}

#[async_trait]
impl ReflectModule for GoalModeModule {
    fn name(&self) -> &'static str {
        "goal_mode"
    }

    async fn check(&self) -> Option<String> {
        let state = self.load_state().await?;
        if state.status != "running" {
            return None;
        }

        let remaining = state.remaining_mins();
        if remaining <= 0.0 {
            Some(format!(
                "[GOAL] Budget exhausted after {} turns. Goal: {}",
                state.turns_completed, state.goal
            ))
        } else {
            Some(format!(
                "[GOAL] Continuing goal work. Elapsed: {:.0}min, Remaining: {:.0}min, Turns: {}, Goal: {}",
                state.elapsed_mins(),
                remaining,
                state.turns_completed,
                state.goal
            ))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn test_goal_state_new() {
        let state = GoalState::new("test goal".into(), 60.0);
        assert_eq!(state.status, "running");
        assert_eq!(state.goal, "test goal");
        assert_eq!(state.budget_minutes, 60.0);
        assert_eq!(state.turns_completed, 0);
    }

    #[test]
    fn test_goal_state_remaining() {
        let state = GoalState::new("test".into(), 60.0);
        // Just created, should have ~60 min remaining
        let remaining = state.remaining_mins();
        assert!(remaining > 59.0 && remaining <= 60.0);
    }

    #[tokio::test]
    async fn test_goal_mode_no_state_file() {
        let tmp = tempfile::tempdir().unwrap();
        let module = GoalModeModule::new(tmp.path(), 60.0);
        assert!(module.check().await.is_none());
    }

    #[tokio::test]
    async fn test_goal_mode_start_and_check() {
        let tmp = tempfile::tempdir().unwrap();
        let module = GoalModeModule::new(tmp.path(), 60.0);
        module.start_goal("write tests".into()).await.unwrap();
        let result = module.check().await;
        assert!(result.is_some());
        let msg = result.unwrap();
        assert!(msg.starts_with("[GOAL]"));
        assert!(msg.contains("Continuing goal work"));
        assert!(msg.contains("write tests"));
    }

    #[tokio::test]
    async fn test_goal_mode_complete() {
        let tmp = tempfile::tempdir().unwrap();
        let module = GoalModeModule::new(tmp.path(), 60.0);
        module.start_goal("test".into()).await.unwrap();
        module.complete_goal().await.unwrap();
        // After completion, check should return None
        assert!(module.check().await.is_none());
    }
}
