#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TaskPriority {
    Critical,
    Low,
}

impl Default for TaskPriority {
    fn default() -> Self {
        TaskPriority::Critical
    }
}
