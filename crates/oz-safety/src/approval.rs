//! Approval handler — async trait bridging agent loop and UI.
//!
//! The agent loop calls `request_approval()` which pauses execution until
//! the user responds (or timeout). Implementations:
//!
//! - **Web**: SSE event → wait for HTTP POST /api/approve
//! - **Tauri**: sse_event → wait for IPC `approve_tool` command
//! - **Fallback**: always deny (used when no UI is available)

use crate::trust::ApprovalInfo;
use std::time::Duration;

#[derive(Clone, Debug)]
pub struct ApprovalRequest {
    pub session_id: String,
    pub tool_name: String,
    pub pattern: String,
    pub arguments: serde_json::Value,
    pub info: ApprovalInfo,
}

#[derive(Clone, Debug)]
pub enum ApprovalDecision {
    Allow,
    TrustSession,
    TrustWorkspace,
    Deny,
    BlockForever,
}

#[derive(Debug)]
pub enum ApprovalError {
    Timeout,
    Cancelled,
    Internal(String),
}

impl std::fmt::Display for ApprovalError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ApprovalError::Timeout => write!(f, "approval timed out"),
            ApprovalError::Cancelled => write!(f, "approval cancelled"),
            ApprovalError::Internal(msg) => write!(f, "approval error: {msg}"),
        }
    }
}

impl std::error::Error for ApprovalError {}

/// Trait for handling tool approval requests.
///
/// Each platform (Web, Tauri, CLI) implements this to provide
/// a user-facing approval mechanism.
#[async_trait::async_trait]
pub trait ApprovalHandler: Send + Sync {
    /// Request user approval for a tool call.
    ///
    /// This method must block until the user responds or timeout.
    /// It should NEVER return without user input or timeout —
    /// otherwise the agent loop will hang indefinitely.
    async fn request_approval(
        &self,
        request: ApprovalRequest,
        timeout: Duration,
    ) -> Result<ApprovalDecision, ApprovalError>;
}

/// A fallback handler that auto-denies all approval requests.
///
/// Used when no UI is available (e.g., headless CLI mode).
/// This ensures security is never silently bypassed.
pub struct DenyAllHandler;

#[async_trait::async_trait]
impl ApprovalHandler for DenyAllHandler {
    async fn request_approval(
        &self,
        request: ApprovalRequest,
        _timeout: Duration,
    ) -> Result<ApprovalDecision, ApprovalError> {
        tracing::warn!(
            "[safety] auto-denied approval for {}/{} (no UI handler available)",
            request.tool_name,
            request.pattern,
        );
        Ok(ApprovalDecision::Deny)
    }
}
