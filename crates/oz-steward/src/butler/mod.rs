//! Butler: the task graph, its verifier and the acceptance composer.
//!
//! The butler session (src-tauri) orchestrates; this crate owns the durable
//! state and the judgement. No worker may write a terminal status.

pub mod budget;
pub mod compose;
pub mod spec;
pub mod store;
pub mod verify;

pub use compose::{compose, document_gates, objective_tokens, repo_gate};
pub use spec::{
    gate_strength, Approval, ApprovalStatus, GateStrength, ModelSource, Reversibility, Run, Task,
    TaskBudget, TaskEvent, TaskStatus,
};
pub use store::{ButlerStore, TaskCounts};
pub use verify::{verify, ReviewOutcome, Reviewer, Verdict, VerifyReport};
