//! ga-safety — Progressive trust and agent safety guard for OpenZen.
//!
//! Provides:
//! - [`TrustStore`] — persistent trust entries with auto-escalation
//! - [`SafetyGuard`] — dispatch-time safety check pipeline
//! - [`ApprovalHandler`] — async trait for UI integration
//! - [`ApprovalQueue`] — concurrent multi-tool approval queue

pub mod approval;
pub mod guard;
pub mod patterns;
pub mod permissions;
pub mod queue;
pub mod trust;
pub mod trust_level;

pub use approval::{ApprovalDecision, ApprovalError, ApprovalHandler, ApprovalRequest};
pub use guard::SafetyGuard;
pub use permissions::{Decision, PermissionRule, Permissions};
pub use queue::ApprovalQueue;
pub use trust::{TrustDecision, TrustEntry, TrustLevel, TrustStore};
pub use trust_level::{load_trust, project_trust, save_trust, set_project_trust};
pub use trust_level::{ProjectTrustLevel, TrustEntry as ProjectTrustEntry, TrustFile};
