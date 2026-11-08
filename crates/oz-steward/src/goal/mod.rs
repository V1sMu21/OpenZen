//! Goal Mode: a bounded, resumable, gate-verified long-running objective.

pub mod gate;
pub mod guardian;
pub mod intent;
pub mod runtime;
pub mod spec;
pub mod store;

pub use gate::{GateOutcome, GateRunResult, GateRunner};
pub use guardian::GoalGuardian;
pub use intent::{detect, GoalIntent};
pub use runtime::{GoalDecision, GoalRuntime, GoalRuntimeConfig};
pub use spec::{
    GoalAttempt, GoalBudget, GoalEvent, GoalGate, GoalRecord, GoalReport, GoalSpec, GoalStatus,
};
pub use store::GoalStore;
