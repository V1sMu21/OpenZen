//! ScheduledTask trait and TaskContext.

use std::time::Duration;

#[derive(Debug, Clone, Default)]
pub struct TaskContext {
    pub working_dir: Option<String>,
    pub skill_mcp_dir: Option<String>,
    pub trust_path: Option<String>,
    /// In-process session pruner. When present, SessionCleanup delegates
    /// to it instead of editing sessions.json on disk — the desktop app
    /// owns the authoritative in-memory copy and would resurrect any
    /// disk-side deletion on its next save. Receives max_idle_days,
    /// returns the number of removed sessions.
    pub session_pruner: Option<SessionPruner>,
}

/// Newtype so TaskContext can keep its Debug/Clone derives.
#[derive(Clone)]
pub struct SessionPruner(pub std::sync::Arc<dyn Fn(i64) -> u32 + Send + Sync>);

impl std::fmt::Debug for SessionPruner {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("SessionPruner(..)")
    }
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
