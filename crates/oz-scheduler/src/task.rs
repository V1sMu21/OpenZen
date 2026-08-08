//! ScheduledTask trait and TaskContext.

use std::time::Duration;

#[derive(Debug, Clone, Default)]
pub struct TaskContext {
    pub working_dir: Option<String>,
    pub skill_mcp_dir: Option<String>,
    pub trust_path: Option<String>,
}

#[derive(Debug, thiserror::Error)]
pub enum TaskError {
    #[error("task error: {0}")]
    Custom(String),
}

#[async_trait::async_trait]
pub trait ScheduledTask: Send + Sync {
    fn name(&self) -> &str;
    fn interval(&self) -> Duration;
    async fn execute(&self, ctx: &TaskContext) -> Result<(), TaskError>;
}
