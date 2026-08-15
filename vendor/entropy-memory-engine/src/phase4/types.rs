#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum TaskPriority {
    #[default]
    Critical,
    Low,
}
