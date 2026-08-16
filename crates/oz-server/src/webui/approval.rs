//! Web approval handler — implements ApprovalHandler for the WebUI server.
//!
//! Flow:
//! 1. agent_loop calls `request_approval()` → generates request_id
//! 2. Sends `approval_needed` SSE event to frontend
//! 3. Blocks on a oneshot channel waiting for user response
//! 4. Frontend shows modal, user clicks → `POST /api/sessions/:id/approve`
//! 5. Endpoint resolves the oneshot → agent_loop continues

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use axum::{
    extract::{Path, State},
    http::StatusCode,
    response::{IntoResponse, Json},
};
use oz_safety::{ApprovalDecision, ApprovalError, ApprovalHandler, ApprovalRequest};
use serde::Deserialize;
use tokio::sync::oneshot;

use crate::webui::sse_bus::{SseBus, SseEvent};

pub struct WebApprovalHandler {
    sse_bus: SseBus,
    pending: Arc<Mutex<HashMap<String, oneshot::Sender<ApprovalDecision>>>>,
}

impl Clone for WebApprovalHandler {
    fn clone(&self) -> Self {
        WebApprovalHandler {
            sse_bus: self.sse_bus.clone(),
            pending: self.pending.clone(),
        }
    }
}

impl WebApprovalHandler {
    pub fn new(sse_bus: SseBus) -> Self {
        WebApprovalHandler {
            sse_bus,
            pending: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    pub fn resolve(&self, request_id: &str, decision: ApprovalDecision) -> bool {
        let sender = self.pending.lock().unwrap().remove(request_id);
        match sender {
            Some(s) => {
                let _ = s.send(decision);
                true
            }
            None => false,
        }
    }
}

#[async_trait::async_trait]
impl ApprovalHandler for WebApprovalHandler {
    async fn request_approval(
        &self,
        request: ApprovalRequest,
        timeout: Duration,
    ) -> Result<ApprovalDecision, ApprovalError> {
        let request_id = uuid::Uuid::new_v4().to_string();
        let (tx, mut rx) = oneshot::channel();

        self.pending.lock().unwrap().insert(request_id.clone(), tx);
        // Drop-safe cleanup: a cancelled waiting future (run stopped /
        // aborted) must not leak the map entry.
        struct PendingGuard {
            pending: Arc<Mutex<HashMap<String, oneshot::Sender<ApprovalDecision>>>>,
            request_id: String,
        }
        impl Drop for PendingGuard {
            fn drop(&mut self) {
                self.pending.lock().unwrap().remove(&self.request_id);
            }
        }
        let _pending_guard = PendingGuard {
            pending: self.pending.clone(),
            request_id: request_id.clone(),
        };

        let payload = serde_json::json!({
            "request_id": request_id,
            "tool_name": request.tool_name,
            "pattern": request.pattern,
            "arguments": request.arguments,
            "approved_count": request.info.approved_count,
            "current_level": format!("{:?}", request.info.current_level),
        });

        let event = SseEvent::new(
            &request.session_id,
            "approval_needed",
            &serde_json::to_string(&payload).unwrap_or_default(),
        );
        let _ = self.sse_bus.send(event);

        tracing::info!(
            "[safety] approval_needed sent: session={session_id}, tool={tool}/{pattern}, id={id}",
            session_id = request.session_id,
            tool = request.tool_name,
            pattern = request.pattern,
            id = request_id,
        );

        match tokio::time::timeout(timeout, &mut rx).await {
            Ok(Ok(decision)) => Ok(decision),
            Ok(Err(_)) => {
                tracing::warn!("[safety] approval channel closed for {request_id}");
                Err(ApprovalError::Cancelled)
            }
            Err(_) => {
                self.pending.lock().unwrap().remove(&request_id);
                tracing::warn!("[safety] approval timeout for {request_id}");
                Err(ApprovalError::Timeout)
            }
        }
    }
}

#[derive(Debug, Deserialize)]
pub struct ApproveRequest {
    pub request_id: String,
    pub decision: String,
}

pub async fn handle_approve(
    State(state): State<Arc<crate::webui::AppState>>,
    Path(_session_id): Path<String>,
    Json(body): Json<ApproveRequest>,
) -> impl IntoResponse {
    let decision = match body.decision.as_str() {
        "allow" => ApprovalDecision::Allow,
        "trust_session" => ApprovalDecision::TrustSession,
        "trust_workspace" => ApprovalDecision::TrustWorkspace,
        "deny" => ApprovalDecision::Deny,
        "block_forever" => ApprovalDecision::BlockForever,
        _ => return (StatusCode::BAD_REQUEST, "invalid decision").into_response(),
    };

    if state.approval_handler.resolve(&body.request_id, decision) {
        tracing::info!(
            "[safety] approval resolved: {request_id} → {d}",
            request_id = body.request_id,
            d = body.decision
        );
        Json(serde_json::json!({"status": "ok"})).into_response()
    } else {
        tracing::warn!(
            "[safety] approval not found: {request_id}",
            request_id = body.request_id
        );
        (StatusCode::NOT_FOUND, "approval request not found").into_response()
    }
}
