//! ga-safety — Progressive trust and agent safety guard for OpenZen.
//!
//! Provides:
//! - [`TrustStore`] — persistent trust entries with auto-escalation
//! - [`SafetyGuard`] — dispatch-time safety check pipeline
//! - [`ApprovalHandler`] — async trait for UI integration
//! - [`ApprovalQueue`] — concurrent multi-tool approval queue

pub mod trust;
pub mod patterns;
pub mod guard;
pub mod approval;
pub mod queue;

pub use trust::{TrustStore, TrustEntry, TrustLevel, TrustDecision};
pub use guard::SafetyGuard;
pub use approval::{ApprovalHandler, ApprovalRequest, ApprovalDecision, ApprovalError};
pub use queue::ApprovalQueue;
