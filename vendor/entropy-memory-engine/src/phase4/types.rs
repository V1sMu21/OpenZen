#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[derive(Default)]
pub enum TaskPriority {
    #[default]
    Critical,
    Low,
}

