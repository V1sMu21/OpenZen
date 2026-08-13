use std::sync::atomic::{AtomicU64, Ordering};

use serde::Serialize;
use serde_json::Value;
use tokio::sync::broadcast;

/// An SSE event that gets broadcast to all connected clients.
#[derive(Debug, Clone, Serialize)]
pub struct SseEvent {
    pub session_id: String,
    pub event_type: String,
    pub data: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub timestamp: Option<String>,
}

/// Counts how many events have been silently overwritten by the broadcast
/// channel because the receiver(s) were too slow. Used for visibility —
/// `tokio::sync::broadcast` drops old events on overflow without telling
/// the sender, which can leave the frontend and agent in inconsistent
/// states.
static OVERFLOW_COUNT: AtomicU64 = AtomicU64::new(0);
static SENT_COUNT: AtomicU64 = AtomicU64::new(0);

impl SseEvent {
    pub fn new(session_id: &str, event_type: &str, data: &str) -> Self {
        SseEvent {
            session_id: session_id.to_string(),
            event_type: event_type.to_string(),
            data: data.to_string(),
            timestamp: Some(chrono::Utc::now().to_rfc3339()),
        }
    }

    /// Error event — something went wrong.
    pub fn error(session_id: &str, message: &str) -> Self {
        Self::new(session_id, "error", message)
    }

    /// System event — status messages.
    pub fn system(session_id: &str, message: &str) -> Self {
        Self::new(session_id, "system", message)
    }

    /// Model info event — broadcast session model configuration at start.
    pub fn model_info(session_id: &str, model: &str, provider: &str, context_window: usize, is_local: bool) -> Self {
        // serde_json instead of hand-built format! — a model name containing
        // a quote previously produced broken JSON that the frontend silently
        // dropped.
        Self::new(
            session_id,
            "model_info",
            &serde_json::json!({
                "model": model,
                "provider": provider,
                "context_window": context_window,
                "is_local": is_local,
            })
            .to_string(),
        )
    }

    /// Done event — agent loop has completed.
    pub fn done(session_id: &str, response: Option<&str>, tokens_in: usize, tokens_out: usize, context_tokens: usize, exit_reason: Option<&str>) -> Self {
        let data = serde_json::json!({
            "exit_reason": exit_reason,
            "data": {
                "full_response": response,
                "input_tokens_est": tokens_in,
                "output_tokens_est": tokens_out,
                "context_tokens_est": context_tokens,
            }
        });
        Self::new(session_id, "done", &data.to_string())
    }

    /// Protocol v1 event — typed start-delta-end protocol event.
    /// The `data` param should be a JSON-serialised protocol event object.
    pub fn protocol_v1(session_id: &str, data: &str) -> Self {
        Self::new(session_id, "protocol_v1", data)
    }

    /// Convenience: build a protocol_v1 event from a serde_json::Value.
    pub fn protocol_v1_json(session_id: &str, payload: &Value) -> Self {
        let data = serde_json::to_string(payload).unwrap_or_default();
        Self::new(session_id, "protocol_v1", &data)
    }

    /// Approval needed event — sent when agent loop requires user confirmation.
    pub fn approval_needed(session_id: &str, payload: &Value) -> Self {
        let data = serde_json::to_string(payload).unwrap_or_default();
        Self::new(session_id, "approval_needed", &data)
    }
}

/// Broadcast bus for SSE events.
#[derive(Clone)]
pub struct SseBus {
    tx: broadcast::Sender<SseEvent>,
}

impl SseBus {
    pub fn new(capacity: usize) -> Self {
        let (tx, _) = broadcast::channel(capacity);
        SseBus { tx }
    }

    pub fn send(&self, event: SseEvent) -> Result<usize, broadcast::error::SendError<SseEvent>> {
        let event_type = event.event_type.clone();
        let session_id = event.session_id.clone();
        let before = self.tx.len();
        match self.tx.send(event) {
            Ok(n) => {
                SENT_COUNT.fetch_add(1, Ordering::Relaxed);
                if before + 1 > 9000 && before >= 9000 {
                    tracing::warn!(
                        "[SSE] broadcast channel nearing capacity: {}/10000 events queued (session={} type={})",
                        before, session_id, event_type
                    );
                }
                Ok(n)
            }
            Err(broadcast::error::SendError(event)) => {
                tracing::warn!(
                    "[SSE] send failed for session={} type={}: no active receivers (frontend disconnected?)",
                    session_id, event_type
                );
                Err(broadcast::error::SendError(event))
            }
        }
    }

    /// Snapshot of broadcast channel diagnostics — useful for debugging
    /// "events silently lost" or "frontend got stuck mid-stream" issues.
    pub fn diagnostics() -> (u64, u64) {
        (
            SENT_COUNT.load(Ordering::Relaxed),
            OVERFLOW_COUNT.load(Ordering::Relaxed),
        )
    }

    pub fn subscribe(&self) -> broadcast::Receiver<SseEvent> {
        self.tx.subscribe()
    }
}
