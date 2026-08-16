//! Tauri approval handler — implements ApprovalHandler for the desktop app.
//!
//! Flow:
//! 1. agent_loop calls `request_approval()` → generates request_id
//! 2. Emits "sse_event" with type "approval_needed" to the webview
//! 3. Blocks on a oneshot channel waiting for user response
//! 4. Frontend shows modal, user clicks → calls Tauri IPC `approve_tool`
//! 5. IPC command resolves the oneshot → agent_loop continues

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use oz_safety::{ApprovalDecision, ApprovalError, ApprovalHandler, ApprovalRequest};
use tauri::{AppHandle, Emitter, State};
use tokio::sync::oneshot;

use crate::AppState;

/// A pending approval entry — the owning session is recorded so that only
/// the window/session that owns the request can adjudicate it (a request_id
/// alone is guessable/leakable across windows).
pub struct PendingApproval {
    pub session_id: String,
    pub tx: oneshot::Sender<ApprovalDecision>,
}

pub type PendingApprovals = Arc<Mutex<HashMap<String, PendingApproval>>>;

pub fn new_pending() -> PendingApprovals {
    Arc::new(Mutex::new(HashMap::new()))
}

pub struct TauriApprovalHandler {
    app_handle: AppHandle,
    pending: PendingApprovals,
    /// "完全访问" flag — when set, every approval request is auto-allowed
    /// (no modal, no blocking) so the agent can run without asking.
    full_access: Arc<AtomicBool>,
    /// session_id → window label. Requests are emitted to the owning window
    /// when mapped (dedicated session windows); otherwise broadcast.
    session_windows: Arc<Mutex<HashMap<String, String>>>,
}

impl Clone for TauriApprovalHandler {
    fn clone(&self) -> Self {
        TauriApprovalHandler {
            app_handle: self.app_handle.clone(),
            pending: self.pending.clone(),
            full_access: self.full_access.clone(),
            session_windows: self.session_windows.clone(),
        }
    }
}

impl TauriApprovalHandler {
    pub fn new(
        app_handle: AppHandle,
        pending: PendingApprovals,
        full_access: Arc<AtomicBool>,
        session_windows: Arc<Mutex<HashMap<String, String>>>,
    ) -> Self {
        TauriApprovalHandler {
            app_handle,
            pending,
            full_access,
            session_windows,
        }
    }
}

#[async_trait::async_trait]
impl ApprovalHandler for TauriApprovalHandler {
    async fn request_approval(
        &self,
        request: ApprovalRequest,
        timeout: Duration,
    ) -> Result<ApprovalDecision, ApprovalError> {
        // Full-access mode: auto-allow every request without showing the
        // modal, so the agent can execute without user intervention.
        if self.full_access.load(Ordering::Relaxed) {
            tracing::info!(
                "[safety] full_access enabled — auto-allowed {}/{} (session={})",
                request.tool_name,
                request.pattern,
                request.session_id,
            );
            return Ok(ApprovalDecision::Allow);
        }

        let request_id = uuid::Uuid::new_v4().to_string();
        let (tx, mut rx) = oneshot::channel();

        crate::lock_poison_guard(&self.pending).insert(
            request_id.clone(),
            PendingApproval {
                session_id: request.session_id.clone(),
                tx,
            },
        );
        // Drop-safe cleanup: when the waiting future is cancelled
        // (session stopped, run aborted), the map entry must not linger —
        // only the timeout path removed it before.
        struct PendingGuard {
            pending: PendingApprovals,
            request_id: String,
        }
        impl Drop for PendingGuard {
            fn drop(&mut self) {
                crate::lock_poison_guard(&self.pending).remove(&self.request_id);
            }
        }
        let _pending_guard = PendingGuard {
            pending: self.pending.clone(),
            request_id: request_id.clone(),
        };

        let payload = serde_json::json!({
            "session_id": request.session_id,
            "event_type": "approval_needed",
            "data": serde_json::to_string(&serde_json::json!({
                "request_id": request_id,
                "tool_name": request.tool_name,
                "pattern": request.pattern,
                "arguments": request.arguments,
                "approved_count": request.info.approved_count,
                "current_level": format!("{:?}", request.info.current_level),
            })).unwrap_or_default(),
        });

        // Route to the owning window when a dedicated session window exists;
        // fall back to broadcast (main window / closed session window).
        let targeted = {
            let mapping = crate::lock_poison_guard(&self.session_windows);
            match mapping.get(&request.session_id) {
                Some(label) => self
                    .app_handle
                    .emit_to(label, "sse_event", &payload)
                    .is_ok(),
                None => false,
            }
        };
        if !targeted {
            let _ = self.app_handle.emit("sse_event", payload);
        }

        tracing::info!(
            "[safety] tauri approval_needed: session={session_id}, tool={tool}/{pattern}, id={id}",
            session_id = request.session_id,
            tool = request.tool_name,
            pattern = request.pattern,
            id = request_id,
        );

        match tokio::time::timeout(timeout, &mut rx).await {
            Ok(Ok(decision)) => Ok(decision),
            Ok(Err(_)) => Err(ApprovalError::Cancelled),
            Err(_) => Err(ApprovalError::Timeout),
        }
    }
}

/// Tauri IPC command: approve_tool
///
/// Called from the frontend when the user clicks a decision button
/// in the approval modal. `session_id` must match the session that owns
/// the pending request — otherwise another window holding the request_id
/// could adjudicate someone else's approval.
#[tauri::command]
pub fn approve_tool(
    session_id: String,
    request_id: String,
    decision: String,
    state: State<'_, Arc<AppState>>,
) -> Result<String, String> {
    let dec = match decision.as_str() {
        "allow" => ApprovalDecision::Allow,
        "trust_session" => ApprovalDecision::TrustSession,
        "trust_workspace" => ApprovalDecision::TrustWorkspace,
        "deny" => ApprovalDecision::Deny,
        "block_forever" => ApprovalDecision::BlockForever,
        _ => return Err(format!("invalid decision: {decision}")),
    };

    let entry = crate::lock_poison_guard(&state.pending_approvals).remove(&request_id);
    match entry {
        Some(entry) => {
            if entry.session_id != session_id {
                // Wrong session — put the request back so the rightful
                // session can still answer it.
                let owner = entry.session_id.clone();
                crate::lock_poison_guard(&state.pending_approvals)
                    .insert(request_id.clone(), entry);
                tracing::warn!(
                    "[safety] tauri approval rejected: {request_id} belongs to session {owner}, got {session_id}"
                );
                return Err(format!(
                    "approval request {request_id} belongs to another session"
                ));
            }
            let _ = entry.tx.send(dec);
            tracing::info!("[safety] tauri approval resolved: {request_id} → {decision}");
            Ok("ok".into())
        }
        None => Err(format!("approval request not found: {request_id}")),
    }
}
