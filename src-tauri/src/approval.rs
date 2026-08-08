//! Tauri approval handler — implements ApprovalHandler for the desktop app.
//!
//! Flow:
//! 1. agent_loop calls `request_approval()` → generates request_id
//! 2. Emits "sse_event" with type "approval_needed" to the webview
//! 3. Blocks on a oneshot channel waiting for user response
//! 4. Frontend shows modal, user clicks → calls Tauri IPC `approve_tool`
//! 5. IPC command resolves the oneshot → agent_loop continues

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use oz_safety::{ApprovalDecision, ApprovalError, ApprovalHandler, ApprovalRequest};
use tauri::{AppHandle, Emitter, State};
use tokio::sync::oneshot;

use crate::AppState;

pub type PendingApprovals = Arc<Mutex<HashMap<String, oneshot::Sender<ApprovalDecision>>>>;

pub fn new_pending() -> PendingApprovals {
    Arc::new(Mutex::new(HashMap::new()))
}

pub struct TauriApprovalHandler {
    app_handle: AppHandle,
    pending: PendingApprovals,
}

impl Clone for TauriApprovalHandler {
    fn clone(&self) -> Self {
        TauriApprovalHandler {
            app_handle: self.app_handle.clone(),
            pending: self.pending.clone(),
        }
    }
}

impl TauriApprovalHandler {
    pub fn new(app_handle: AppHandle, pending: PendingApprovals) -> Self {
        TauriApprovalHandler { app_handle, pending }
    }
}

#[async_trait::async_trait]
impl ApprovalHandler for TauriApprovalHandler {
    async fn request_approval(
        &self,
        request: ApprovalRequest,
        timeout: Duration,
    ) -> Result<ApprovalDecision, ApprovalError> {
        let request_id = uuid::Uuid::new_v4().to_string();
        let (tx, mut rx) = oneshot::channel();

        self.pending.lock().unwrap().insert(request_id.clone(), tx);

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

        let _ = self.app_handle.emit("sse_event", payload);

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
            Err(_) => {
                self.pending.lock().unwrap().remove(&request_id);
                Err(ApprovalError::Timeout)
            }
        }
    }
}

/// Tauri IPC command: approve_tool
///
/// Called from the frontend when the user clicks a decision button
/// in the approval modal.
#[tauri::command]
pub fn approve_tool(
    _session_id: String,
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

    let sender = state.pending_approvals.lock().unwrap().remove(&request_id);
    match sender {
        Some(s) => {
            let _ = s.send(dec);
            tracing::info!("[safety] tauri approval resolved: {request_id} → {decision}");
            Ok("ok".into())
        }
        None => Err(format!("approval request not found: {request_id}")),
    }
}
