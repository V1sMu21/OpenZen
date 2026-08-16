//! WebUI server for OpenZen.
//!
//! Provides:
//! - `/api/chat` — POST endpoint that runs the agent loop and broadcasts events
//! - `/api/events` — SSE endpoint for real-time streaming
//! - `/api/sessions` — CRUD for chat sessions
//! - `/` — Static file serving for the frontend

pub mod approval;
pub mod sessions;
pub mod sse_bus;

pub use sessions::{SessionInfo, SessionStore};
pub use sse_bus::SseBus;

use std::collections::{HashMap, VecDeque};
use std::sync::atomic::AtomicBool;
use std::sync::Arc;
use std::sync::Mutex;
use std::time::Duration;

use anyhow::Context;
use axum::{
    extract::{Query, State},
    http::StatusCode,
    response::{
        sse::{Event, KeepAlive, Sse},
        IntoResponse, Json,
    },
    routing::{get, post},
    Router,
};
use futures::future::{ready, FutureExt};
use futures::stream::Stream;
use futures::StreamExt;
use serde::{Deserialize, Serialize};
use tokio::sync::RwLock;
use tokio_stream::wrappers::BroadcastStream;
use tower_http::services::ServeDir;

use oz_config::mykey::{MyKeyConfig, SessionType};
use oz_core::checkpoint::InterventionEvent;
use oz_core::checkpoint::InterventionKind;
use oz_core::handler::LoopConfig;
use oz_core_types::LlmClient;
use oz_core_types::{ContentBlock, Message, Role, ToolContext};
use oz_memory::MemorySystem;
use oz_tools::handler::ToolRegistryHandler;
use oz_tools::registry::ToolRegistry;

use crate::webui::sessions::SessionStatus;
use crate::webui::sse_bus::SseEvent;

/// Shared application state for the WebUI server.
pub struct AppState {
    pub config_path: String,
    pub assets_dir: String,
    pub working_dir: String,
    pub sessions: Arc<RwLock<SessionStore>>,
    pub sse_bus: SseBus,
    /// Intervention channels per session: session_id -> queue of intervention events.
    pub interventions: Mutex<HashMap<String, Arc<Mutex<VecDeque<InterventionEvent>>>>>,
    /// Per-session ask_user reply slots: the agent loop blocks on the
    /// session's slot while `ask_user` is pending, and resumes the same
    /// run with the reply as a tool_result (no new run / no new user msg).
    pub ask_user_rxs: Mutex<HashMap<String, Arc<Mutex<Option<String>>>>>,
    /// Per-session stop signals that the running agent loop polls. When the user
    /// clicks "Stop" the corresponding AtomicBool is flipped to true and the
    /// loop checks it between tool calls / turns.
    pub stop_signals: Mutex<HashMap<String, Arc<AtomicBool>>>,
    /// Process-level MCP manager pool: created once on first use and shared
    /// by every request, so subprocesses are not leaked per chat. A failed
    /// init leaves the cell empty and is retried on the next request.
    pub mcp_manager: tokio::sync::OnceCell<Arc<tokio::sync::Mutex<oz_mcp::McpManager>>>,
    /// Per-session run-guard mutexes. Holding the mutex of session X
    /// guarantees no other task is currently running the agent loop on
    /// session X — preventing two concurrent runs from racing on the same
    /// `additional_messages` history, double-pushing assistant turns, and
    /// overwriting each other's `stop_signal`.
    ///
    /// Uses `tokio::sync::Mutex` (not `std::sync::Mutex`) because we hold
    /// the guard across `.await` points, and `std::sync::MutexGuard` is
    /// `!Send`, which would make the entire handler future `!Send` and
    /// fail axum's `Handler` trait bound. `tokio::sync::MutexGuard` is
    /// `Send` (across yield points at least).
    pub run_guards: Mutex<HashMap<String, Arc<tokio::sync::Mutex<()>>>>,
    pub auth_token: String,
    pub approval_handler: crate::webui::approval::WebApprovalHandler,
}

impl AppState {
    pub fn new(config_path: String, assets_dir: String, working_dir: String) -> Self {
        Self::with_auth(config_path, assets_dir, working_dir, "".into())
    }

    pub fn with_auth(
        config_path: String,
        assets_dir: String,
        working_dir: String,
        auth_token: String,
    ) -> Self {
        let sessions_path = std::path::Path::new(&working_dir).join("openzen/sessions.json");
        let sse = SseBus::new(10_000);
        AppState {
            config_path,
            assets_dir,
            working_dir,
            sessions: Arc::new(RwLock::new(SessionStore::persisted(sessions_path))),
            sse_bus: sse.clone(),
            approval_handler: crate::webui::approval::WebApprovalHandler::new(sse),
            interventions: Mutex::new(HashMap::new()),
            ask_user_rxs: Mutex::new(HashMap::new()),
            stop_signals: Mutex::new(HashMap::new()),
            mcp_manager: tokio::sync::OnceCell::new(),
            run_guards: Mutex::new(HashMap::new()),
            auth_token,
        }
    }
}

type SharedState = Arc<AppState>;

/// Axum middleware that checks `Authorization: Bearer <token>` on
/// every API route. Health-check and static files are exempt.
async fn auth_middleware(
    State(state): State<SharedState>,
    req: axum::extract::Request,
    next: axum::middleware::Next,
) -> impl axum::response::IntoResponse {
    if state.auth_token.is_empty() {
        return next.run(req).await;
    }
    let header = req.headers().get("authorization");
    let is_public_path = req.uri().path() == "/api/health"
        || req.uri().path().starts_with("/api/events")
        || !req.uri().path().starts_with("/api/");
    if is_public_path {
        return next.run(req).await;
    }
    let expected = format!("Bearer {}", state.auth_token);
    match header.and_then(|v| v.to_str().ok()) {
        Some(value) if value == expected => next.run(req).await,
        _ => (StatusCode::UNAUTHORIZED, "unauthorized").into_response(),
    }
}

/// Start the WebUI server on the given port.
pub async fn serve_webui(
    port: u16,
    config_path: String,
    assets_dir: String,
    working_dir: String,
    frontend_dir: Option<String>,
    auth_token: Option<String>,
) -> anyhow::Result<()> {
    let was_provided = auth_token.is_some();
    let token = auth_token.unwrap_or_else(|| format!("ga-{}", uuid::Uuid::new_v4()));
    if !was_provided {
        tracing::info!("Generated random auth token: {}", token);
    }
    let state = Arc::new(AppState::with_auth(
        config_path,
        assets_dir,
        working_dir,
        token,
    ));

    // Determine where to find frontend static files
    let static_dir = match frontend_dir {
        Some(dir) => dir,
        None => {
            let p = std::path::Path::new(&state.working_dir).join("frontends/dist");
            std::fs::canonicalize(&p)
                .unwrap_or(p)
                .to_string_lossy()
                .to_string()
        }
    };

    let app = Router::new()
        // API routes — protected by auth middleware
        .route("/api/chat", post(handle_chat))
        .route("/api/events", get(handle_sse))
        .route("/api/sessions", get(list_sessions).post(create_session))
        .route(
            "/api/sessions/:id",
            get(get_session)
                .delete(delete_session)
                .patch(rename_session),
        )
        .route("/api/sessions/:id/stop", post(stop_session))
        .route("/api/sessions/:id/regenerate", post(handle_regenerate))
        .route("/api/sessions/:id/compress", post(handle_compress))
        .route("/api/sessions/:id/intervene", post(handle_intervene))
        .route(
            "/api/sessions/:id/ask_user_response",
            post(handle_ask_user_response),
        )
        .route(
            "/api/sessions/:id/checkpoints",
            get(list_session_checkpoints_handler),
        )
        .route("/api/sessions/:id/resume", post(resume_session_handler))
        .route(
            "/api/sessions/:id/approve",
            post(crate::webui::approval::handle_approve),
        )
        .route("/api/checkpoints", get(list_all_checkpoints_handler))
        .route("/api/upgrade", post(handle_upgrade))
        .route("/api/models", get(list_models))
        .route("/api/agents", get(list_agents))
        .route("/api/health", get(health_check))
        .route_layer(axum::middleware::from_fn_with_state(
            state.clone(),
            auth_middleware,
        ))
        // Static file serving for frontend — NOT behind auth
        .nest_service("/", ServeDir::new(&static_dir))
        .with_state(state);

    let addr = format!("0.0.0.0:{port}");
    tracing::info!("WebUI server listening on http://{addr}");

    let listener = tokio::net::TcpListener::bind(&addr)
        .await
        .context(format!("Failed to bind to {addr}"))?;
    axum::serve(listener, app).tcp_nodelay(true).await?;

    Ok(())
}

// ── Request / Response types ──

#[derive(Debug, Deserialize)]
pub struct ChatRequest {
    pub message: String,
    #[serde(default = "default_session_id")]
    pub session_id: String,
    #[serde(default)]
    pub session_name: Option<String>,
    #[serde(default)]
    pub model_name: Option<String>,
}

fn default_session_id() -> String {
    uuid::Uuid::new_v4().to_string()
}

/// Reconstruct the assistant turn(s) from a saved message's
/// `streamEvents`. Returns a `Vec<Message>` (0..2 entries):
///   1. The assistant message containing only the tool_use blocks
///      (text is dropped — see below).
///   2. (Optional) A follow-up user message containing every
///      `tool_result` block from this turn.
///
/// Splitting the assistant's tool_use from the tool_result is
/// REQUIRED by both Anthropic and OpenAI tool protocols. The previous
/// implementation put everything in a single assistant message, which
/// made the LLM forget it had already executed the tool sequence and
/// re-run it on every new turn (the user reported: "agent 在每次执行
/// 任务时都会把之前用户的发送的任务都执行一遍").
///
/// IMPORTANT (Bug #1 follow-up): the LLM ALSO tends to "echo" the
/// visible text of the previous assistant turn when it appears in
/// the assistant's own content blocks, especially when the previous
/// turn ended with a verbose summary like "All three steps completed".
/// The model treats that summary as a script to keep following instead
/// of pivoting to the new user request. We therefore drop the text
/// and only emit tool_use blocks; the full text is still saved on the
/// message and shown in the UI, but the LLM doesn't get a copy of
/// its own previous answer to parrot back. The LLM still has the
/// tool_use ↔ tool_result pairs which is the actual source of truth
/// for "what the agent did last turn".
///
/// `summary_only`: when true, replace the assistant's tool_use blocks
/// with a single short text block summarising the tools that were
/// called, and drop the result_blocks user message entirely. This is
/// what we feed to the LLM when replaying previous turns of a
/// multi-turn session — the LLM is otherwise prone to "echo" the
/// previous tool pattern and re-execute it on the new user request
/// (Bug #1). With a summary, the LLM knows the previous task is
/// complete but isn't given the tool_use ↔ tool_result structure that
/// would re-trigger execution.
fn reconstruct_assistant_turn(json_msg: &serde_json::Value, summary_only: bool) -> Vec<Message> {
    let Some(events) = json_msg.get("streamEvents").and_then(|v| v.as_array()) else {
        return Vec::new();
    };

    let mut text_acc: std::collections::HashMap<String, String> = std::collections::HashMap::new();
    let mut reasoning_acc: std::collections::HashMap<String, String> =
        std::collections::HashMap::new();
    let mut tool_use: std::collections::HashMap<String, (String, serde_json::Value)> =
        std::collections::HashMap::new();
    let mut tool_use_emitted: std::collections::HashSet<String> = std::collections::HashSet::new();
    let mut tool_names_in_order: Vec<String> = Vec::new();

    let mut assistant_blocks: Vec<ContentBlock> = Vec::new();
    let mut result_blocks: Vec<ContentBlock> = Vec::new();

    for ev in events {
        let evt_type = ev.get("type").and_then(|v| v.as_str()).unwrap_or("");
        match evt_type {
            "text_start" => {
                if let Some(id) = ev.get("id").and_then(|v| v.as_str()) {
                    text_acc.entry(id.to_string()).or_default();
                }
            }
            "text_delta" => {
                if let Some(id) = ev.get("id").and_then(|v| v.as_str()) {
                    let text = ev.get("text").and_then(|v| v.as_str()).unwrap_or("");
                    text_acc
                        .entry(id.to_string())
                        .and_modify(|s| s.push_str(text))
                        .or_insert_with(|| text.to_string());
                }
            }
            "text_end" => {
                if let Some(id) = ev.get("id").and_then(|v| v.as_str()) {
                    text_acc.remove(id);
                }
            }
            "reasoning_start" => {
                if let Some(id) = ev.get("id").and_then(|v| v.as_str()) {
                    reasoning_acc.entry(id.to_string()).or_default();
                }
            }
            "reasoning_delta" => {
                if let Some(id) = ev.get("id").and_then(|v| v.as_str()) {
                    let text = ev.get("text").and_then(|v| v.as_str()).unwrap_or("");
                    reasoning_acc
                        .entry(id.to_string())
                        .and_modify(|s| s.push_str(text))
                        .or_insert_with(|| text.to_string());
                }
            }
            "reasoning_end" => {
                if let Some(id) = ev.get("id").and_then(|v| v.as_str()) {
                    if let Some(text) = reasoning_acc.remove(id) {
                        if !text.is_empty() {
                            assistant_blocks.push(ContentBlock::Thinking {
                                thinking: text,
                                signature: None,
                            });
                        }
                    }
                }
            }
            "tool_input_start" => {
                if let Some(tc_id) = ev.get("tool_call_id").and_then(|v| v.as_str()) {
                    let name = ev
                        .get("name")
                        .and_then(|v| v.as_str())
                        .unwrap_or("")
                        .to_string();
                    tool_use
                        .entry(tc_id.to_string())
                        .or_insert_with(|| (name.clone(), serde_json::Value::Null));
                    if !tool_names_in_order.contains(&name) {
                        tool_names_in_order.push(name);
                    }
                }
            }
            "tool_input_delta" => {
                if let Some(tc_id) = ev.get("tool_call_id").and_then(|v| v.as_str()) {
                    let delta = ev.get("delta").and_then(|v| v.as_str()).unwrap_or("");
                    let entry = tool_use.entry(tc_id.to_string()).or_insert_with(|| {
                        (String::new(), serde_json::Value::String(String::new()))
                    });
                    if entry.1.is_string() {
                        if let Some(s) = entry.1.as_str() {
                            entry.1 = serde_json::Value::String(format!("{s}{delta}"));
                        }
                    }
                }
            }
            "tool_input_available" => {
                if let Some(tc_id) = ev.get("tool_call_id").and_then(|v| v.as_str()) {
                    let name = ev
                        .get("name")
                        .and_then(|v| v.as_str())
                        .unwrap_or("")
                        .to_string();
                    let raw_args = ev.get("args").and_then(|v| v.as_str()).unwrap_or("");
                    let args_value: serde_json::Value = serde_json::from_str(raw_args)
                        .unwrap_or_else(|_| serde_json::Value::String(raw_args.to_string()));
                    tool_use.insert(tc_id.to_string(), (name, args_value));
                    if tool_use_emitted.insert(tc_id.to_string()) && !summary_only {
                        if let Some((name, args)) = tool_use.get(tc_id) {
                            assistant_blocks.push(ContentBlock::tool_use(
                                tc_id,
                                name.clone(),
                                args.clone(),
                            ));
                        }
                    }
                }
            }
            "tool_output_available" => {
                if let Some(tc_id) = ev.get("tool_call_id").and_then(|v| v.as_str()) {
                    let output = ev
                        .get("output")
                        .and_then(|v| v.as_str())
                        .unwrap_or("")
                        .to_string();
                    // tool_result lives in a USER message that follows
                    // the assistant's tool_use. In summary_only mode we
                    // skip it so the LLM context doesn't contain the
                    // tool_use ↔ tool_result structure that would
                    // re-trigger execution of the same tools.
                    if !summary_only {
                        result_blocks.push(ContentBlock::tool_result(tc_id, output));
                    }
                }
            }
            _ => {} // data_*, finish_message, start_step, finish_step — skip
        }
    }

    // Stray text from unfinished `text_*` blocks (e.g. stream cut
    // off mid-text) is also dropped from the LLM context, for the
    // same reason as the text_end arm above.
    for (_, text) in text_acc.drain() {
        let _ = text;
    }
    // Reasoning blocks are the model's internal scratchpad. Anthropic's
    // API requires them to immediately precede the tool_use blocks they
    // belong to, so we only keep them in non-summary mode where we
    // still emit tool_use; in summary_only mode we drop them so the
    // LLM isn't tempted to continue any chain of thought that ended
    // with a tool call.
    if !summary_only {
        for (_, text) in reasoning_acc {
            if !text.is_empty() {
                assistant_blocks.push(ContentBlock::Thinking {
                    thinking: text,
                    signature: None,
                });
            }
        }
    }
    for (tc_id, (name, args)) in &tool_use {
        if !tool_use_emitted.contains(tc_id) {
            if summary_only {
                continue;
            }
            assistant_blocks.push(ContentBlock::tool_use(tc_id, name.clone(), args.clone()));
        }
    }

    let mut out = Vec::new();
    if summary_only {
        // Build a brief summary of what the assistant did last turn,
        // presented as a USER turn. The reasoning: the LLM is in
        // "agent mode" by default and tends to mirror the role+content
        // shape of its previous assistant turn (i.e. if it sees an
        // assistant turn that called tools, it calls them again on
        // the next user message). By making the prior work a USER
        // turn — phrased as a system note the user could plausibly
        // have sent — the LLM has no assistant-role "script" to echo
        // and just answers the new request directly (Bug #1).
        //
        // We deduplicate tool names in the order they were first seen
        // so the summary reads naturally ("called write, then read"
        // rather than "called write, write, read").
        let mut deduped: Vec<String> = Vec::new();
        for name in &tool_names_in_order {
            if !deduped.contains(name) {
                deduped.push(name.clone());
            }
        }
        let summary_text = if !deduped.is_empty() {
            if deduped.len() == 1 {
                format!(
                    "[System note: In a previous turn I used the `{}` tool to handle a prior request; that work is already complete. Now respond to the new request below — do not redo the previous tool call unless the new request explicitly asks for it.]",
                    deduped[0]
                )
            } else {
                let list = deduped.join(", ");
                format!(
                    "[System note: In previous turns I used {} to handle prior requests; that work is already complete. Now respond to the new request below — do not redo those tool calls unless the new request explicitly asks for it.]",
                    list
                )
            }
        } else {
            "[System note: In a previous turn I answered a request with a text response. Now respond to the new request below.]".to_string()
        };
        out.push(Message {
            role: Role::User,
            content: vec![ContentBlock::text(summary_text)],
            tool_results: None,
        });
    } else if !assistant_blocks.is_empty() {
        out.push(Message {
            role: Role::Assistant,
            content: assistant_blocks,
            tool_results: None,
        });
    }
    if !result_blocks.is_empty() {
        out.push(Message {
            role: Role::User,
            content: result_blocks,
            tool_results: None,
        });
    }
    out
}

#[derive(Debug, Serialize)]
pub struct ChatResponse {
    pub session_id: String,
    pub status: String,
    pub response: Option<String>,
    pub exit_reason: Option<String>,
}

#[derive(Debug, Serialize)]
struct SessionListResponse {
    sessions: Vec<SessionInfo>,
}

#[derive(Debug, Deserialize)]
struct CreateSessionRequest {
    #[serde(default)]
    name: Option<String>,
}

#[derive(Debug, Serialize)]
struct CreateSessionResponse {
    session_id: String,
    name: String,
}

#[derive(Debug, Deserialize)]
struct InterveneRequest {
    /// The intervention type: new_strategy, change_priority, inject_info, pause
    #[serde(default = "default_intervene_kind")]
    kind: String,
    /// The content/instruction from the user
    content: String,
}

fn default_intervene_kind() -> String {
    "inject_info".to_string()
}

#[derive(Debug, Deserialize)]
struct ResumeRequest {
    /// Turn number to resume from (0 = latest available)
    #[serde(default)]
    turn: u32,
    /// New message/instruction to kick off the resumed session
    message: Option<String>,
}

#[derive(Debug, Serialize)]
struct CheckpointListResponse {
    checkpoints: Vec<oz_core::checkpoint::CheckpointMeta>,
}

#[derive(Debug, Serialize)]
struct InterveneResponse {
    received: bool,
    intervention_id: String,
}

// ── Chat handler ──

async fn handle_chat(
    State(state): State<SharedState>,
    Json(req): Json<ChatRequest>,
) -> Result<Json<ChatResponse>, (StatusCode, String)> {
    let session_id = req.session_id.clone();
    let session_name = req.session_name.clone().unwrap_or_else(|| {
        let ts = chrono::Utc::now();
        format!("Session {}", ts.format("%H:%M"))
    });

    // Acquire (or create) the per-session run-guard. We use `try_lock` so
    // that a second concurrent `/api/chat` request for the same session
    // gets a clean 409 instead of queuing forever or racing on shared
    // state. The guard mutex itself is kept in `state.run_guards` so
    // `stop_session` and other code can reach it.
    //
    // IMPORTANT: We drop the entry-map `std::sync::MutexGuard` BEFORE
    // holding the session guard across `.await`, because the former is
    // `!Send`. The session guard itself is `tokio::sync::MutexGuard`,
    // which IS `Send` and is what we keep for the rest of the handler.
    let run_guard = {
        let mut guards = state.run_guards.lock().unwrap();
        guards
            .entry(session_id.clone())
            .or_insert_with(|| Arc::new(tokio::sync::Mutex::new(())))
            .clone()
        // entry-map guard dropped here at end of block
    };
    let _run_guard_lock = match run_guard.try_lock() {
        Ok(g) => g,
        Err(_) => {
            tracing::warn!(
                "[chat] rejecting concurrent request for session {session_id}: another agent run is in progress"
            );
            return Err((
                StatusCode::CONFLICT,
                format!("Agent for session {session_id} is already running"),
            ));
        }
    };

    // Ensure session exists
    {
        let mut sessions = state.sessions.write().await;
        if !sessions.has_session(&session_id) {
            sessions.create_with_id(&session_id, &session_name);
        }
    }

    // Set session status to running
    {
        let mut sessions = state.sessions.write().await;
        if let Some(s) = sessions.get_mut(&session_id) {
            s.status = SessionStatus::Running;
        }
    }

    // Add user message to session history
    {
        let mut sessions = state.sessions.write().await;
        if let Some(s) = sessions.get_mut(&session_id) {
            s.messages.push(serde_json::json!({
                "role": "user",
                "content": req.message,
                "timestamp": chrono::Utc::now().to_rfc3339(),
            }));
        }
    }
    // Persist immediately so user message survives a crash during agent run
    state.sessions.read().await.save();

    let agent_result = std::panic::AssertUnwindSafe(run_agent_for_session(
        &state,
        &session_id,
        &req.message,
        &state.assets_dir,
        &state.working_dir,
        &state.config_path,
        &state.sse_bus,
        req.model_name.as_deref(),
    ))
    .catch_unwind()
    .await
    .map_err(|e| {
        let msg = if let Some(s) = e.downcast_ref::<&str>() {
            s.to_string()
        } else if let Some(s) = e.downcast_ref::<String>() {
            s.clone()
        } else {
            "agent loop panicked".to_string()
        };
        tracing::error!("[chat] agent loop for session {session_id} panicked: {msg}");
        anyhow::anyhow!("agent loop panicked: {msg}")
    });

    // ALWAYS mark session idle and drop the stop_signal, even on panic.
    // Without this, a panic would leave the session permanently in
    // "Running" state with a stale stop_signal that future runs can't
    // reset (because the loop is gone).
    {
        let mut sessions = state.sessions.write().await;
        if let Some(s) = sessions.get_mut(&session_id) {
            s.status = SessionStatus::Idle;
        }
    }
    state.stop_signals.lock().unwrap().remove(&session_id);
    // `_guard_lock` is released here as it goes out of scope, allowing
    // the next /api/chat request for this session to proceed.

    match agent_result {
        // Outer Result: panic caught by `catch_unwind`
        Err(panic_err) => {
            tracing::error!("Agent loop for session {session_id} panicked: {panic_err}");
            let _ = state
                .sse_bus
                .send(SseEvent::error(&session_id, &panic_err.to_string()));
            Ok(Json(ChatResponse {
                session_id,
                status: "error".to_string(),
                response: None,
                exit_reason: Some(panic_err.to_string()),
            }))
        }
        // Inner Result: normal return or recoverable error
        Ok(Ok(response)) => Ok(Json(ChatResponse {
            session_id,
            status: "completed".to_string(),
            response,
            exit_reason: None,
        })),
        Ok(Err(e)) => {
            tracing::error!("Agent loop for session {session_id} failed: {e}");
            let _ = state
                .sse_bus
                .send(SseEvent::error(&session_id, &e.to_string()));
            Ok(Json(ChatResponse {
                session_id,
                status: "error".to_string(),
                response: None,
                exit_reason: Some(e.to_string()),
            }))
        }
    }
}

/// Cap the largest stream-event payloads (tool outputs, errors) so a long
/// run keeps bounded memory. Aligns with the 100KB cap applied when
/// session messages are persisted.
fn truncate_stream_event(event: oz_core_types::StreamEvent) -> oz_core_types::StreamEvent {
    const MAX_EVENT_FIELD_CHARS: usize = 100_000;
    let cap_chars = |s: &str| -> String {
        if s.chars().count() <= MAX_EVENT_FIELD_CHARS {
            s.to_string()
        } else {
            let mut out: String = s.chars().take(MAX_EVENT_FIELD_CHARS).collect();
            out.push('…');
            out
        }
    };
    use oz_core_types::StreamEvent as E;
    match event {
        E::ToolOutputAvailable {
            tool_call_id,
            name,
            output,
        } => E::ToolOutputAvailable {
            tool_call_id,
            name,
            output: cap_chars(&output),
        },
        E::Error { message } => E::Error {
            message: cap_chars(&message),
        },
        other => other,
    }
}

/// Byte-budget truncation that never splits a UTF-8 char: slicing at a raw
/// byte index panics when the boundary lands mid-char, which for CJK text
/// (>100KB tool output) is the common case, not the exception.
/// Returns None when the string already fits.
fn truncate_bytes_char_safe(s: &str, max_bytes: usize) -> Option<String> {
    if s.len() <= max_bytes {
        return None;
    }
    let mut end = max_bytes;
    while end > 0 && !s.is_char_boundary(end) {
        end -= 1;
    }
    Some(format!("{}... [truncated {} bytes]", &s[..end], s.len() - end))
}

/// Run the agent loop for a session, broadcasting events via SSE.
/// Returns the full response text if available.
#[allow(clippy::too_many_arguments, clippy::field_reassign_with_default)]
async fn run_agent_for_session(
    state: &AppState,
    session_id: &str,
    user_message: &str,
    assets_dir: &str,
    working_dir: &str,
    config_path: &str,
    sse_bus: &SseBus,
    model_name: Option<&str>,
) -> anyhow::Result<Option<String>> {
    let lang = std::env::var("OZ_LANG").unwrap_or_default();
    let sys_prompt_filename = if lang == "en" {
        "sys_prompt_en.txt"
    } else {
        "sys_prompt.txt"
    };

    let ctx = ToolContext {
        working_dir: working_dir.to_string(),
        assets_dir: assets_dir.to_string(),
        script_dir: assets_dir.to_string(),
        lang: lang.clone(),
        skill_mcp_dir: None,
        harness_dir: None,
        session_id: session_id.to_string(),
    };

    // Load config
    let cfg_path = std::path::PathBuf::from(config_path);
    let cfg =
        MyKeyConfig::from_file(&cfg_path).map_err(|e| anyhow::anyhow!("Config error: {e}"))?;

    // Use default session, or the one specified by model_name
    let session_name = model_name
        .or(cfg.default_session.as_deref())
        .unwrap_or("claude_sonnet");
    let sess_config = cfg
        .get(session_name)
        .ok_or_else(|| anyhow::anyhow!("Session '{session_name}' not found in config"))?;
    let sess_type = cfg.session_type(session_name);

    // Broadcast model info
    let provider = match sess_type {
        SessionType::Claude => "claude",
        SessionType::Oai => "openai",
        SessionType::NativeClaude => "claude",
        SessionType::NativeOai => "openai",
        SessionType::Mixin => "mixin",
    };
    let _ = sse_bus.send(SseEvent::model_info(
        session_id,
        &sess_config.model,
        provider,
        sess_config.context_win,
        false, // always online for these providers
    ));

    let backend: Box<dyn oz_llm::Session> = match sess_type {
        SessionType::Claude => Box::new(oz_llm::ClaudeSession::new(sess_config.clone())),
        SessionType::Oai => Box::new(oz_llm::OaiSession::new(sess_config.clone())),
        SessionType::NativeClaude => {
            Box::new(oz_llm::NativeClaudeSession::new(sess_config.clone()))
        }
        SessionType::NativeOai => Box::new(oz_llm::NativeOAISession::new(sess_config.clone())),
        SessionType::Mixin => {
            anyhow::bail!("Mixin session not supported in WebUI");
        }
    };

    let mut client = oz_llm::NativeToolClient::new(backend);

    let memory = MemorySystem::new(std::path::Path::new(working_dir), &lang);
    let memory_context = memory.get_global_memory().await.unwrap_or_default();

    // Layer MCP-server tools on top of the built-in registry. Best-effort:
    // a missing or broken servers.toml leaves the agent with the defaults.
    let mut registry = ToolRegistry::build_default();
    let mcp_servers_path = std::path::Path::new(working_dir).join("servers.toml");
    if mcp_servers_path.exists() {
        // Process-level pool: the manager (and its child processes) is
        // created once and reused by every request. A per-request manager
        // leaked its subprocesses for the lifetime of the process.
        match state
            .mcp_manager
            .get_or_try_init(|| async {
                let mut discovery = oz_mcp::McpDiscovery::new(&mcp_servers_path);
                discovery
                    .load()
                    .map_err(|e| anyhow::anyhow!("failed to load servers.toml: {e}"))?;
                let manager = Arc::new(tokio::sync::Mutex::new(
                    oz_mcp::McpManager::from_discovery(&discovery),
                ));
                let n = manager
                    .lock()
                    .await
                    .start_all()
                    .await
                    .map_err(|e| anyhow::anyhow!("MCP start_all failed: {e}"))?;
                tracing::info!("[mcp] started {n} MCP server(s)");
                Ok::<_, anyhow::Error>(manager)
            })
            .await
        {
            Ok(manager) => {
                let mcp_count =
                    oz_tools::mcp_bridge::register_mcp_tools(&mut registry, manager).await;
                tracing::info!("[mcp] registered {mcp_count} MCP tool(s)");
            }
            Err(e) => {
                tracing::warn!("[mcp] skipped MCP tools this request: {e}");
            }
        }
    }
    let definitions = registry.to_schema("en");
    let mut handler = ToolRegistryHandler::new(registry);

    // Load system prompt
    let sys_prompt_path = std::path::PathBuf::from(assets_dir).join(sys_prompt_filename);
    let mut system_prompt = if sys_prompt_path.exists() {
        tokio::fs::read_to_string(&sys_prompt_path).await?
    } else {
        String::new()
    };

    // Strip the stale hard-coded "Today: YYYY-MM-DD ..." trailer left over
    // from when the prompt was checked in. The LLM is now expected to
    // fetch the current time itself via `code_run` (see Direct-Answer rules
    // in the prompt — path/time queries DO use code_run for live system
    // facts). Leaving the stale date in would just give the LLM a false
    // anchor to parrot.
    if let Some(idx) = system_prompt.find("\nToday: ") {
        system_prompt.truncate(idx);
    } else if let Some(idx) = system_prompt.find("Today: ") {
        if idx == 0 {
            if let Some(end) = system_prompt.find('\n') {
                system_prompt.replace_range(0..end + 1, "");
            } else {
                system_prompt.clear();
            }
        }
    }

    if !memory_context.is_empty() {
        system_prompt.push_str("\n\n## Persistent Memory Context\n\n");
        system_prompt.push_str(&memory_context);
    }

    let mut loop_config = LoopConfig::default();
    loop_config.context_win = sess_config.context_win;
    loop_config.max_turns = 70;
    loop_config.verbose = false;
    loop_config.session_id = session_id.to_string();
    loop_config.checkpoint_interval = 5; // Save checkpoint every 5 turns
    loop_config.working_dir = working_dir.to_string();
    let working_path = std::path::Path::new(working_dir);
    if let Err(e) = oz_skill_mcp::migration::run_all_migrations(working_path) {
        tracing::warn!("skill/MCP migration: {}", e);
    }
    let skill_mcp_dir = working_path.join(oz_skill_mcp::SKILL_MCP_DIR);
    if skill_mcp_dir.is_dir() {
        loop_config.skill_mcp_dir = Some(skill_mcp_dir.to_string_lossy().into_owned());
    }

    // Safety guard: progressive trust for tool calls
    let trust_path = std::path::Path::new(&working_dir).join("openzen/trust.json");
    let trust_store = oz_safety::TrustStore::new(Some(trust_path));
    let safety_guard = std::sync::Arc::new(oz_safety::SafetyGuard::new(trust_store));
    loop_config.safety_guard = Some(safety_guard.clone());
    loop_config.approval_handler = Some(std::sync::Arc::new(state.approval_handler.clone())
        as std::sync::Arc<dyn oz_safety::ApprovalHandler>);

    let (event_tx, mut event_rx) =
        tokio::sync::mpsc::unbounded_channel::<oz_core_types::StreamEvent>();
    let collected_events: Arc<Mutex<Vec<oz_core_types::StreamEvent>>> =
        Arc::new(Mutex::new(Vec::new()));
    let event_arrival_ms: Arc<Mutex<Vec<u64>>> = Arc::new(Mutex::new(Vec::new()));
    let start_ms: Arc<Mutex<Option<u64>>> = Arc::new(Mutex::new(None));

    // Collector task. The JoinHandle is awaited AFTER the agent loop returns
    // and we drop all event_tx clones, so the channel closes and this task
    // drains the buffer before we read collected_events for persistence.
    let collector_handle: tokio::task::JoinHandle<()> = {
        let sse = sse_bus.clone();
        let sid = session_id.to_string();
        let events_for_collector = collected_events.clone();
        let arrivals_for_collector = event_arrival_ms.clone();
        let start_for_collector = start_ms.clone();
        tokio::spawn(async move {
            while let Some(event) = event_rx.recv().await {
                let event = truncate_stream_event(event);
                let now_ms = std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map(|d| d.as_millis() as u64)
                    .unwrap_or(0);
                {
                    let mut s = start_for_collector.lock().unwrap();
                    if s.is_none() {
                        *s = Some(now_ms);
                    }
                }
                let base = start_for_collector.lock().unwrap().unwrap_or(now_ms);
                let arr = now_ms.saturating_sub(base);
                // Coalesce per-token deltas into their block: the collected
                // Vec and the persisted streamEvents stay O(blocks), not
                // O(tokens). Arrival samples stay index-aligned.
                let merged = {
                    let mut events = events_for_collector.lock().unwrap();
                    oz_core_types::append_coalesced(&mut events, event.clone())
                };
                if !merged {
                    arrivals_for_collector.lock().unwrap().push(arr);
                }

                // Forward as-is in the protocol_v1 envelope. Internal
                // events (ToolCallReady) are filtered out — UIs ignore
                // them anyway, but skipping saves a serialize round-trip.
                if !matches!(event, oz_core_types::StreamEvent::ToolCallReady { .. }) {
                    if let Ok(value) = serde_json::to_value(&event) {
                        let _ = sse.send(SseEvent::protocol_v1_json(&sid, &value));
                    }
                }
            }
        })
    };

    let event_tx_for_after = event_tx.clone();
    loop_config.event_tx = Some(event_tx);

    // Wire intervention channel for this session
    {
        let mut interventions = state.interventions.lock().unwrap();
        let queue = interventions
            .entry(session_id.to_string())
            .or_insert_with(|| Arc::new(Mutex::new(VecDeque::new())));
        loop_config.intervention_rx = Some(queue.clone());
    }

    // Wire ask_user reply slot for this session. Reset to None at run
    // start so a leftover reply from a prior (failed) run doesn't
    // satisfy the next ask_user.
    {
        let mut ask_rxs = state.ask_user_rxs.lock().unwrap();
        let slot = ask_rxs
            .entry(session_id.to_string())
            .or_insert_with(|| Arc::new(Mutex::new(None)));
        *slot.lock().unwrap() = None;
        loop_config.ask_user_rx = Some(slot.clone());
    }

    let additional_messages = {
        let sessions = state.sessions.read().await;
        let mut msgs: Vec<Message> = Vec::new();
        if let Some(s) = sessions.get(session_id) {
            let msg_count = s.messages.len();
            // Exclude the LAST message — it's the user message we just
            // pushed in `handle_chat` and is passed separately as
            // `user_message`. We re-construct the prior conversation so
            // the LLM can see what was already done in this session.
            //
            // For assistant turns we don't just send the visible text —
            // we also reconstruct the full tool_use / tool_result block
            // sequence from the saved `streamEvents`. Without this the
            // LLM has no memory of which tools it already called or
            // what those tools returned, and on the next turn it
            // re-runs the same tools (the user reported: "agent在每次
            // 执行任务时都会把之前用户的发送的任务都执行一遍").
            //
            // `reconstruct_assistant_turn` returns 0..2 messages per
            // turn: an assistant message with text/thinking/tool_use,
            // plus an optional follow-up user message carrying the
            // tool_result blocks. Tool_result MUST live in a user
            // message — that's the protocol both Anthropic and OpenAI
            // require, and was the root cause of the re-execution bug.
            let history = if msg_count > 1 {
                &s.messages[..msg_count - 1]
            } else {
                &[]
            };
            for json_msg in history {
                let role_str = json_msg.get("role").and_then(|v| v.as_str()).unwrap_or("");
                match role_str {
                    "user" => {
                        let content = json_msg
                            .get("content")
                            .and_then(|v| v.as_str())
                            .unwrap_or("");
                        msgs.push(Message {
                            role: Role::User,
                            content: vec![ContentBlock::text(content)],
                            tool_results: None,
                        });
                    }
                    "assistant" => {
                        let mut turn_msgs = reconstruct_assistant_turn(json_msg, true);
                        if turn_msgs.is_empty() {
                            // No stream events saved (legacy / partial save).
                            // Fall back to tool calls from the saved
                            // `toolCalls` field. Same summary_only treatment
                            // — we synthesize a single summary text block
                            // instead of emitting the tool_use / tool_result
                            // pairs, to avoid the re-execution bug.
                            let mut tool_names: Vec<String> = Vec::new();
                            if let Some(tool_calls) =
                                json_msg.get("toolCalls").and_then(|v| v.as_array())
                            {
                                for tc in tool_calls {
                                    let name = tc
                                        .get("name")
                                        .and_then(|v| v.as_str())
                                        .unwrap_or("")
                                        .to_string();
                                    if !tool_names.contains(&name) {
                                        tool_names.push(name);
                                    }
                                }
                            }
                            if !tool_names.is_empty() {
                                let summary = if tool_names.len() == 1 {
                                    format!("[Earlier turn: I called `{}` to handle the user's request. That work is complete.]", tool_names[0])
                                } else {
                                    let list = tool_names.join(", ");
                                    format!("[Earlier turn: I called {} to handle the user's request. That work is complete.]", list)
                                };
                                turn_msgs.push(Message {
                                    role: Role::Assistant,
                                    content: vec![ContentBlock::text(summary)],
                                    tool_results: None,
                                });
                            }
                        }
                        msgs.extend(turn_msgs);
                    }
                    _ => {} // Skip system, tool, and unknown roles
                }
            }
        }
        msgs
    };

    // ── BUG FIX #6 (root cause): re-execution guard. The LLM is in
    // "agent mode" with tool definitions always available; when it sees
    // a previous assistant turn that called a tool, it tends to pattern-
    // match and call the same tool again on the new request. The soft
    // "[System note: ... do not redo ...]" we used to emit was too weak
    // — the LLM treats it as one of many instructions and ignores it.
    //
    // The robust fix: scan the history for tools the agent used in any
    // earlier turn, then APPEND a binding system-level note listing them
    // as forbidden for this turn unless the new request explicitly asks.
    // System role outranks user role, so the LLM cannot route around it.
    //
    // "Forbidden tool" list = the union of every tool name the agent
    // called across all prior turns in this session. We deduplicate and
    // keep only the meaningful (non-`respond`) ones — `respond` is the
    // "just give me text" sentinel and is always allowed.
    {
        let sessions = state.sessions.read().await;
        if let Some(sess) = sessions.get(session_id) {
            let mut used: std::collections::BTreeSet<String> = std::collections::BTreeSet::new();
            for m in &sess.messages {
                if m.get("role").and_then(|v| v.as_str()) != Some("assistant") {
                    continue;
                }
                // Walk both the saved streamEvents and the legacy toolCalls
                // list so the rule applies even for sessions saved before
                // streamEvents existed.
                if let Some(events) = m.get("streamEvents").and_then(|v| v.as_array()) {
                    for ev in events {
                        if ev.get("type").and_then(|v| v.as_str()) == Some("tool_input_available") {
                            if let Some(n) = ev.get("name").and_then(|v| v.as_str()) {
                                if n != "respond" {
                                    used.insert(n.to_string());
                                }
                            }
                        }
                    }
                }
                if let Some(tcs) = m.get("toolCalls").and_then(|v| v.as_array()) {
                    for tc in tcs {
                        if let Some(n) = tc.get("name").and_then(|v| v.as_str()) {
                            if n != "respond" {
                                used.insert(n.to_string());
                            }
                        }
                    }
                }
            }
            if !used.is_empty() {
                let list = used.into_iter().collect::<Vec<_>>().join(", ");
                system_prompt.push_str(&format!(
                    "\n\n## Per-Turn Re-Execution Lock (auto-injected, binding)\n\
                     The following tool(s) were already used in earlier turns of this session: {list}.\n\
                     Their effect is ALREADY ON DISK / ALREADY IN YOUR CONTEXT.\n\
                     **STRICTLY FORBIDDEN for this turn:** do NOT call {list} again, in any form,\n\
                     UNLESS the new user request below explicitly contains a repeat trigger\n\
                     (\"again\" / \"再\" / \"do it again\" / \"the same\" / \"再来一遍\" / \"再写一次\" / \"redo\" / \"same as before\").\n\
                     If the new request is a fresh question or a different task, respond directly\n\
                     via `respond` (or call a DIFFERENT tool that's actually needed).",
                ));
            }
        }
    }

    let stop_signal = Arc::new(AtomicBool::new(false));
    {
        let mut map = state.stop_signals.lock().unwrap();
        map.insert(session_id.to_string(), stop_signal.clone());
    }

    let outcome = oz_core::agent_loop::run_agent_loop(
        &mut client,
        system_prompt,
        user_message.to_string(),
        additional_messages,
        &mut handler,
        &definitions,
        &ctx,
        &loop_config,
        &stop_signal,
    )
    .await;

    loop_config.event_tx = None;

    if let Some(ref err_msg) = outcome
        .data
        .as_ref()
        .and_then(|d| d.get("error"))
        .and_then(|v| v.as_str())
    {
        let _ = event_tx_for_after.send(oz_core_types::StreamEvent::Error {
            message: err_msg.to_string(),
        });
    } else {
        let _ = event_tx_for_after.send(oz_core_types::StreamEvent::FinishMessage {
            stop_reason: outcome.exit_reason.clone(),
        });
    }

    drop(event_tx_for_after);

    let _ = collector_handle.await;

    let full_response = outcome
        .data
        .as_ref()
        .and_then(|d| d.get("full_response"))
        .and_then(|v| v.as_str())
        .map(|s| s.to_string());

    let tokens_in: usize = outcome
        .data
        .as_ref()
        .and_then(|d| d.get("input_tokens_est"))
        .and_then(|v| v.as_u64())
        .unwrap_or(0) as usize;
    let tokens_out: usize = outcome
        .data
        .as_ref()
        .and_then(|d| d.get("output_tokens_est"))
        .and_then(|v| v.as_u64())
        .unwrap_or(0) as usize;
    let context_tokens: usize = outcome
        .data
        .as_ref()
        .and_then(|d| d.get("context_tokens_est"))
        .and_then(|v| v.as_u64())
        .unwrap_or(0) as usize;

    let exit_reason_str = outcome.exit_reason.clone();
    let _ = sse_bus.send(SseEvent::done(
        session_id,
        full_response.as_deref(),
        tokens_in,
        tokens_out,
        context_tokens,
        Some(exit_reason_str.as_str()),
    ));

    if let Some(ref response_text) = full_response {
        if !response_text.is_empty() {
            let now = chrono::Utc::now();

            let duration_ms: Option<u64> = {
                let sessions = state.sessions.read().await;
                sessions.get(session_id).and_then(|s| {
                    s.messages.last().and_then(|m| {
                        m.get("timestamp").and_then(|v| v.as_str()).and_then(|ts| {
                            chrono::DateTime::parse_from_rfc3339(ts).ok().map(|dt| {
                                let dur = now.signed_duration_since(dt.with_timezone(&chrono::Utc));
                                dur.num_milliseconds().max(0) as u64
                            })
                        })
                    })
                })
            };

            let stream_events_json: Vec<serde_json::Value> = {
                let events = collected_events.lock().unwrap();
                let arrivals = event_arrival_ms.lock().unwrap();
                let now_ms = std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map(|d| d.as_millis() as u64)
                    .unwrap_or(0);
                events
                    .iter()
                    .enumerate()
                    .filter_map(|(i, e)| {
                        let mut v = serde_json::to_value(e).ok()?;
                        let arr_i = arrivals.get(i).copied().unwrap_or(0);
                        let next_arr = arrivals.get(i + 1).copied().unwrap_or(now_ms);
                        let dur = next_arr.saturating_sub(arr_i);
                        if let Some(obj) = v.as_object_mut() {
                            obj.insert(
                                "duration_ms".to_string(),
                                serde_json::Value::Number(dur.into()),
                            );
                        }
                        Some(v)
                    })
                    .collect()
            };
            let stream_events_count = stream_events_json.len();
            tracing::info!(
                "[SAVE] collected {} stream events for session {}",
                stream_events_count,
                session_id
            );

            let mut sessions = state.sessions.write().await;
            if let Some(s) = sessions.get_mut(session_id) {
                let mut msg = serde_json::json!({
                    "role": "assistant",
                    "content": response_text,
                    "timestamp": now.to_rfc3339(),
                });

                if !stream_events_json.is_empty() {
                    msg["streamEvents"] = serde_json::Value::Array(stream_events_json);
                }

                if let Some(dur) = duration_ms {
                    msg["duration"] = serde_json::json!(dur);
                }

                msg["modelInfo"] = serde_json::json!({
                    "model": sess_config.model,
                    "provider": provider,
                    "contextWindow": sess_config.context_win,
                    "isLocal": false,
                });

                msg["exitReason"] = serde_json::json!(outcome.exit_reason);

                if let Some(ref data) = outcome.data {
                    if let Some(thinking) = data.get("full_thinking").and_then(|v| v.as_str()) {
                        if !thinking.is_empty() {
                            msg["thinking"] = serde_json::Value::String(thinking.to_string());
                        }
                    }
                    if let Some(tools) = data.get("tool_calls").and_then(|v| v.as_array()) {
                        if !tools.is_empty() {
                            let arr: Vec<serde_json::Value> = tools.to_vec();
                            msg["toolCalls"] = serde_json::Value::Array(arr);
                        }
                    }
                    if let Some(ti) = data.get("input_tokens_est").and_then(|v| v.as_u64()) {
                        msg["tokensIn"] = serde_json::Value::Number(ti.into());
                    }
                    if let Some(to) = data.get("output_tokens_est").and_then(|v| v.as_u64()) {
                        msg["tokensOut"] = serde_json::Value::Number(to.into());
                    }
                    if let Some(ct) = data.get("context_tokens_est").and_then(|v| v.as_u64()) {
                        msg["contextTokens"] = serde_json::Value::Number(ct.into());
                    }
                }
                // Truncate oversized tool results and content to keep sessions.json manageable
                const MAX_TOOL_RESULT_BYTES: usize = 100_000;
                const MAX_CONTENT_BYTES: usize = 200_000;
                if let Some(events) = msg["streamEvents"].as_array_mut() {
                    for ev in events.iter_mut() {
                        if ev.get("type").and_then(|v| v.as_str()) == Some("tool_result") {
                            if let Some(result) = ev.get("result").and_then(|v| v.as_str()) {
                                if let Some(truncated) =
                                    truncate_bytes_char_safe(result, MAX_TOOL_RESULT_BYTES)
                                {
                                    ev["result"] = serde_json::Value::String(truncated);
                                }
                            }
                        }
                    }
                }
                if let Some(content) = msg.get("content").and_then(|v| v.as_str()) {
                    if let Some(truncated) = truncate_bytes_char_safe(content, MAX_CONTENT_BYTES) {
                        msg["content"] = serde_json::Value::String(truncated);
                    }
                }

                s.messages.push(msg);
            }
            drop(sessions);
            state.sessions.read().await.save();
        }
    }

    // Archive session
    if let Some(ref data) = outcome.data {
        if let Some(full_response_str) = data.get("full_response").and_then(|v| v.as_str()) {
            if !full_response_str.is_empty() {
                let transcript = format!(
                    "# Session Transcript\n\n**Turns:** {}\n\n**Exit:** {}\n\n---\n\n{}",
                    outcome.turn, outcome.exit_reason, full_response_str
                );
                match memory.archive_session(&transcript).await {
                    Ok(path) => tracing::info!("Session archived to {:?}", path),
                    Err(e) => tracing::warn!("Failed to archive session: {e}"),
                }
            }
        }
    }

    {
        let mut map = state.stop_signals.lock().unwrap();
        map.remove(session_id);
    }

    Ok(full_response)
}

// ── SSE handler ──

async fn handle_sse(
    State(state): State<SharedState>,
) -> Sse<impl Stream<Item = Result<Event, std::convert::Infallible>>> {
    let rx = state.sse_bus.subscribe();
    let stream = BroadcastStream::new(rx).filter_map(|result| {
        ready(match result {
            Ok(event) => {
                let data = serde_json::to_string(&event).unwrap_or_default();
                Some(Ok(Event::default().data(data)))
            }
            Err(e) => {
                tracing::warn!("SSE broadcast lagged, dropped {} events", e);
                None
            }
        })
    });

    Sse::new(stream).keep_alive(
        KeepAlive::new()
            .interval(Duration::from_secs(15))
            .text("ka"),
    )
}

// ── Session CRUD handlers ──

async fn list_sessions(State(state): State<SharedState>) -> Json<SessionListResponse> {
    let sessions = state.sessions.read().await;
    let list: Vec<SessionInfo> = sessions.list();
    Json(SessionListResponse { sessions: list })
}

async fn create_session(
    State(state): State<SharedState>,
    Json(req): Json<CreateSessionRequest>,
) -> Json<CreateSessionResponse> {
    let mut sessions = state.sessions.write().await;
    let name = req.name.unwrap_or_else(|| {
        let ts = chrono::Utc::now();
        format!("Session {}", ts.format("%H:%M"))
    });
    let info = sessions.create(&name);
    Json(CreateSessionResponse {
        session_id: info.id,
        name: info.name,
    })
}

#[derive(Serialize)]
pub struct ModelEntry {
    pub name: String,
    pub model: String,
    pub provider: String,
    pub context_win: usize,
}

async fn list_models(State(state): State<SharedState>) -> Json<Vec<ModelEntry>> {
    let cfg_path = std::path::PathBuf::from(&state.config_path);
    let models = if let Ok(cfg) = MyKeyConfig::from_file(&cfg_path) {
        cfg.sessions
            .iter()
            .map(|(name, sess)| {
                let provider = match cfg.session_type(name) {
                    SessionType::Claude | SessionType::NativeClaude => "claude",
                    SessionType::Oai | SessionType::NativeOai | SessionType::Mixin => "openai",
                };
                ModelEntry {
                    name: name.clone(),
                    model: sess.model.clone(),
                    provider: provider.to_string(),
                    context_win: sess.context_win,
                }
            })
            .collect()
    } else {
        vec![]
    };
    Json(models)
}

async fn list_agents() -> Json<Vec<serde_json::Value>> {
    let dir = oz_agent::agents_dir();
    match oz_agent::Agent::list(&dir) {
        Ok(names) => {
            let agents: Vec<serde_json::Value> = names.into_iter().filter_map(|name| {
                oz_agent::Agent::load(&name, &dir).ok().map(|a| {
                    serde_json::json!({
                        "name": a.name,
                        "model": a.config.model,
                        "tools": a.tool_names(),
                        "has_instructions": a.config.instructions.as_ref().map(|s| !s.is_empty()).unwrap_or(false),
                    })
                })
            }).collect();
            Json(agents)
        }
        Err(_) => Json(vec![]),
    }
}

/// Pagination query for GET /api/sessions/:id.
///
/// `offset` counts from the END of the raw message vector: offset=0 is the
/// newest page (what the chat UI paints first), offset=limit is the next
/// older page, and so on. Each returned message carries its original `idx`
/// so the frontend can prepend older pages without remapping keys.
#[derive(Debug, Deserialize)]
struct SessionPageQuery {
    offset: Option<usize>,
    limit: Option<usize>,
}

async fn get_session(
    State(state): State<SharedState>,
    axum::extract::Path(id): axum::extract::Path<String>,
    Query(page): Query<SessionPageQuery>,
) -> Result<Json<serde_json::Value>, (StatusCode, String)> {
    let sessions = state.sessions.read().await;
    match sessions.get(&id) {
        Some(session) => {
            let total = session.messages.len();
            let offset = page.offset.unwrap_or(0).min(total);
            let end = page
                .limit
                .map(|limit| offset.saturating_add(limit).min(total))
                .unwrap_or(total);
            let start = total.saturating_sub(end);
            let page_messages: Vec<serde_json::Value> = session.messages[start..end]
                .iter()
                .enumerate()
                .map(|(page_pos, message)| {
                    let mut value = message.clone();
                    if let Some(obj) = value.as_object_mut() {
                        obj.insert("idx".to_string(), serde_json::json!(start + page_pos));
                    }
                    value
                })
                .collect();
            let value = serde_json::json!({
                "id": session.info.id,
                "name": session.info.name,
                "created_at": session.info.created_at,
                "status": session.status.as_str(),
                "messages": page_messages,
                "total_messages": total,
                "offset": offset,
                "limit": end - start,
                "has_more": start > 0,
            });
            Ok(Json(value))
        }
        None => Err((StatusCode::NOT_FOUND, format!("Session {id} not found"))),
    }
}

async fn delete_session(
    State(state): State<SharedState>,
    axum::extract::Path(id): axum::extract::Path<String>,
) -> Result<Json<serde_json::Value>, (StatusCode, String)> {
    let mut sessions = state.sessions.write().await;
    if sessions.delete(&id) {
        // Tidy up any per-session state (run-guard, stop signal,
        // intervention queue) so the maps don't grow without bound. The
        // run_guards and stop_signals maps use `std::sync::Mutex`, so
        // their guards must be dropped before any await. Removal of
        // already-arc'd entries is fine — the Arc keeps the inner
        // `tokio::sync::Mutex` alive if a handler is still holding it.
        state.run_guards.lock().unwrap().remove(&id);
        state.stop_signals.lock().unwrap().remove(&id);
        state.interventions.lock().unwrap().remove(&id);
        Ok(Json(serde_json::json!({ "status": "deleted" })))
    } else {
        Err((StatusCode::NOT_FOUND, format!("Session {id} not found")))
    }
}

#[derive(Debug, Deserialize)]
struct RenameSessionRequest {
    name: String,
}

async fn rename_session(
    State(state): State<SharedState>,
    axum::extract::Path(id): axum::extract::Path<String>,
    Json(req): Json<RenameSessionRequest>,
) -> Result<Json<serde_json::Value>, (StatusCode, String)> {
    let mut sessions = state.sessions.write().await;
    if sessions.rename(&id, &req.name) {
        Ok(Json(
            serde_json::json!({ "status": "renamed", "name": req.name }),
        ))
    } else {
        Err((StatusCode::NOT_FOUND, format!("Session {id} not found")))
    }
}

/// Simple health check endpoint for daemon monitoring.
/// Also returns the server's current auth token so the frontend can
/// auto-discover it (the endpoint is exempt from auth middleware).
async fn health_check(State(state): State<SharedState>) -> axum::Json<serde_json::Value> {
    axum::Json(serde_json::json!({
        "status": "ok",
        "service": "openzen",
        "auth_token": state.auth_token,
    }))
}

async fn stop_session(
    State(state): State<SharedState>,
    axum::extract::Path(id): axum::extract::Path<String>,
) -> Result<Json<serde_json::Value>, (StatusCode, String)> {
    // Trip the actual AtomicBool the running agent loop is polling.
    let tripped = {
        let map = state.stop_signals.lock().unwrap();
        map.get(&id).map(|sig| {
            sig.store(true, std::sync::atomic::Ordering::SeqCst);
            true
        })
    };
    {
        let mut sessions = state.sessions.write().await;
        if let Some(session) = sessions.get_mut(&id) {
            session.status = SessionStatus::Stopped;
        }
    }
    if tripped.is_none() {
        return Err((
            StatusCode::NOT_FOUND,
            format!("No running agent for session {id}"),
        ));
    }
    let _ = state
        .sse_bus
        .send(SseEvent::system(&id, "Session stopped by user"));
    Ok(Json(serde_json::json!({ "status": "stopped" })))
}

/// POST /api/sessions/:id/compress
/// Manually compress the context for a session: deserialize messages,
/// run compress_messages, serialize back, save, return stats.
async fn handle_compress(
    State(state): State<SharedState>,
    axum::extract::Path(id): axum::extract::Path<String>,
) -> Result<Json<serde_json::Value>, (StatusCode, String)> {
    let (
        before_chars,
        after_chars,
        saved_chars,
        saved_pct,
        messages_removed,
        before_count,
        after_count,
        metrics_str,
        summary,
        _llm_summary,
    ): (
        usize,
        usize,
        usize,
        f64,
        usize,
        usize,
        usize,
        String,
        String,
        Option<String>,
    ) = {
        let mut sessions = state.sessions.write().await;
        let session = sessions
            .get_mut(&id)
            .ok_or_else(|| (StatusCode::NOT_FOUND, format!("Session {id} not found")))?;

        let mut messages: Vec<Message> = session
            .messages
            .iter()
            .filter_map(|v| {
                let role = v.get("role")?.as_str()?;
                let content = v
                    .get("content")
                    .and_then(|c| c.as_str())
                    .unwrap_or("")
                    .to_string();
                match role {
                    "user" => Some(Message::user(&content)),
                    "assistant" => Some(Message::assistant(&content)),
                    "system" => Some(Message::system(&content)),
                    _ => None,
                }
            })
            .collect();

        let before_chars = oz_core::measure_usage(&messages).total_chars;
        let before_count = messages.len();

        let config = oz_core::CompressionConfig::default();
        // Manual /compact is a force action — bypass the trigger
        // threshold with context_win=1 (same trick as emergency_compress).
        let _saved = oz_core::compress_messages(&mut messages, 1, &config, None);

        let after_chars = oz_core::measure_usage(&messages).total_chars;
        let after_count = messages.len();
        let saved_chars = before_chars.saturating_sub(after_chars);
        let saved_pct = if before_chars > 0 {
            ((saved_chars as f64 / before_chars as f64) * 100.0 * 10.0).round() / 10.0
        } else {
            0.0
        };
        let messages_removed = before_count.saturating_sub(after_count);

        let original_msgs = session.messages.clone();
        session.messages =
            oz_core::compress::match_messages_to_originals(&messages, &session.messages);

        let metrics = oz_core::compress::CompressionMetrics::compute(
            before_chars,
            after_chars,
            before_count,
            after_count,
        );
        let removed_json = {
            let surviving_ids: std::collections::HashSet<String> = session
                .messages
                .iter()
                .filter_map(|v| {
                    Some(format!(
                        "{}_{}",
                        v.get("role")?.as_str()?,
                        v.get("content")?.as_str()?
                    ))
                })
                .collect();
            original_msgs
                .into_iter()
                .filter(|v| {
                    let id = format!(
                        "{}_{}",
                        v.get("role").and_then(|r| r.as_str()).unwrap_or(""),
                        v.get("content").and_then(|c| c.as_str()).unwrap_or("")
                    );
                    !surviving_ids.contains(&id)
                })
                .collect::<Vec<_>>()
        };
        let summary = oz_core::compress::build_compression_summary(&removed_json, "");

        sessions.save();

        (
            before_chars,
            after_chars,
            saved_chars,
            saved_pct,
            messages_removed,
            before_count,
            after_count,
            metrics.summary(),
            summary,
            None::<String>,
        )
    };

    // Generate LLM summary for /compact when enough messages were removed
    let llm_summary = if messages_removed >= 4 {
        let cfg = MyKeyConfig::from_file(&state.config_path).ok();
        // Use the same summary model as the agent loop's auto-compression.
        let sess_pair: Option<(String, oz_config::mykey::SessionConfig)> =
            cfg.as_ref().and_then(|c| {
                if let Some(ref name) = c.summary_model {
                    c.get(name).cloned().map(|s| (name.clone(), s)).or_else(|| {
                        c.sessions
                            .iter()
                            .find(|(_, s)| s.model == *name)
                            .map(|(n, s)| (n.clone(), s.clone()))
                    })
                } else {
                    let name = c.default_session.as_deref().unwrap_or("claude_sonnet");
                    c.get(name).cloned().map(|s| (name.to_string(), s))
                }
            });
        let sess_name = sess_pair
            .as_ref()
            .map(|(n, _)| n.as_str())
            .unwrap_or("claude_sonnet")
            .to_string();
        let sess_config = sess_pair.map(|(_, s)| s);
        let sess_type = cfg
            .as_ref()
            .map(|c| c.session_type(&sess_name))
            .unwrap_or(SessionType::Oai);

        let mut llm_summary: Option<String> = None;
        if let Some(sess_config) = sess_config {
            let backend: Option<Box<dyn oz_llm::Session>> = match sess_type {
                SessionType::Claude => {
                    Some(Box::new(oz_llm::ClaudeSession::new(sess_config.clone())))
                }
                SessionType::Oai => Some(Box::new(oz_llm::OaiSession::new(sess_config.clone()))),
                SessionType::NativeClaude => Some(Box::new(oz_llm::NativeClaudeSession::new(
                    sess_config.clone(),
                ))),
                SessionType::NativeOai => {
                    Some(Box::new(oz_llm::NativeOAISession::new(sess_config.clone())))
                }
                _ => None,
            };
            if let Some(backend) = backend {
                let mut client = oz_llm::NativeToolClient::new(backend);
                let prompt = Message::user(format!(
                    "Summarize what was discussed in these conversation fragments \
                     in ONE short sentence (max 30 words). Do NOT re-execute or \
                     continue the conversation.\n\n{summary}"
                ));
                let msgs = [prompt];
                if let Ok(Ok(resp)) =
                    tokio::time::timeout(Duration::from_secs(10), client.chat(&msgs, &[])).await
                {
                    if !resp.content.is_empty() {
                        llm_summary = Some(resp.content);
                    }
                }
            }
        }
        llm_summary
    } else {
        None
    };

    // Inject LLM summary into session if generated
    if let Some(ref ls) = llm_summary {
        let mut sessions = state.sessions.write().await;
        if let Some(session) = sessions.get_mut(&id) {
            session.messages.insert(
                0,
                serde_json::json!({
                    "role": "system",
                    "content": format!("[Compression summary]: {ls}"),
                    "timestamp": chrono::Utc::now().to_rfc3339(),
                }),
            );
            sessions.save();
        }
    }

    Ok(Json(serde_json::json!({
        "session_id": id,
        "before_chars": before_chars,
        "after_chars": after_chars,
        "saved_chars": saved_chars,
        "saved_pct": saved_pct,
        "messages_removed": messages_removed,
        "metrics": metrics_str,
        "summary": summary,
        "llm_summary": llm_summary,
        "strategy": format!("compressed {}→{} messages, saved {:.1}% chars{}",
            before_count, after_count, saved_pct,
            if llm_summary.is_some() { " (LLM summary)" } else { " (template)" }),
    })))
}

/// POST /api/sessions/:id/regenerate
/// Re-run the last user message, removing the most recent assistant response.
/// This implements chat branching: the last assistant message is marked as a
/// child branch, and a new response is generated from the same user prompt.
async fn handle_regenerate(
    State(state): State<SharedState>,
    axum::extract::Path(id): axum::extract::Path<String>,
) -> Result<Json<ChatResponse>, (StatusCode, String)> {
    // Extract the last user message, then remove the last assistant+user pair.
    let last_user_msg: Option<String> = {
        let mut sessions = state.sessions.write().await;
        let session = sessions
            .get_mut(&id)
            .ok_or_else(|| (StatusCode::NOT_FOUND, format!("Session {id} not found")))?;
        let msgs = &mut session.messages;
        // Pop last assistant message
        while msgs
            .last()
            .is_some_and(|m| m.get("role").and_then(|v| v.as_str()) == Some("assistant"))
        {
            msgs.pop();
        }
        // Pop last user message and save it
        let user_msg = msgs.pop().and_then(|m| {
            m.get("content")
                .and_then(|v| v.as_str())
                .map(|s| s.to_string())
        });
        sessions.save();
        user_msg
    };

    let user_message = last_user_msg.ok_or_else(|| {
        (
            StatusCode::BAD_REQUEST,
            "No user message found to regenerate".to_string(),
        )
    })?;

    // Set session status to running
    {
        let mut sessions = state.sessions.write().await;
        if let Some(s) = sessions.get_mut(&id) {
            s.status = SessionStatus::Running;
        }
    }

    // Re-add the user message
    {
        let mut sessions = state.sessions.write().await;
        if let Some(s) = sessions.get_mut(&id) {
            s.messages.push(serde_json::json!({
                "role": "user",
                "content": user_message,
                "timestamp": chrono::Utc::now().to_rfc3339(),
            }));
        }
    }
    state.sessions.read().await.save();

    let agent_result = run_agent_for_session(
        &state,
        &id,
        &user_message,
        &state.assets_dir,
        &state.working_dir,
        &state.config_path,
        &state.sse_bus,
        None,
    )
    .await;

    {
        let mut sessions = state.sessions.write().await;
        if let Some(s) = sessions.get_mut(&id) {
            s.status = SessionStatus::Idle;
        }
    }

    match agent_result {
        Ok(response) => Ok(Json(ChatResponse {
            session_id: id,
            status: "completed".to_string(),
            response,
            exit_reason: None,
        })),
        Err(e) => {
            tracing::error!("Regenerate for session {id} failed: {e}");
            let _ = state.sse_bus.send(SseEvent::error(&id, &e.to_string()));
            Ok(Json(ChatResponse {
                session_id: id,
                status: "error".to_string(),
                response: None,
                exit_reason: Some(e.to_string()),
            }))
        }
    }
}

// ── Intervention handlers ──

/// Parse intervention kind string from the API request.
fn parse_intervention_kind(s: &str) -> InterventionKind {
    match s {
        "new_strategy" => InterventionKind::NewStrategy,
        "change_priority" => InterventionKind::ChangePriority,
        "inject_info" => InterventionKind::InjectInfo,
        "pause" => InterventionKind::Pause,
        "resume" => InterventionKind::Resume,
        _ => InterventionKind::InjectInfo,
    }
}

/// POST /api/sessions/:id/intervene
/// Inject a user intervention into a running session.
async fn handle_intervene(
    State(state): State<SharedState>,
    axum::extract::Path(id): axum::extract::Path<String>,
    Json(req): Json<InterveneRequest>,
) -> Result<Json<InterveneResponse>, (StatusCode, String)> {
    // Check session exists
    {
        let sessions = state.sessions.read().await;
        if !sessions.has_session(&id) {
            return Err((StatusCode::NOT_FOUND, format!("Session {id} not found")));
        }
    }

    let kind = parse_intervention_kind(&req.kind);
    let intervention = oz_core::checkpoint::make_intervention(kind, &req.content);

    // Push to the session's intervention queue
    {
        let mut interventions = state.interventions.lock().unwrap();
        let queue = interventions
            .entry(id.clone())
            .or_insert_with(|| Arc::new(Mutex::new(VecDeque::new())));
        queue.lock().unwrap().push_back(intervention.clone());
    }

    // Broadcast to SSE
    let _ = state.sse_bus.send(SseEvent::system(
        &id,
        &format!(
            "Intervention received: {} — {}",
            intervention.kind,
            &req.content[..req.content.len().min(100)]
        ),
    ));

    Ok(Json(InterveneResponse {
        received: true,
        intervention_id: intervention.id,
    }))
}

/// Request body for the ask_user reply endpoint.
#[derive(Debug, Deserialize)]
struct AskUserResponseRequest {
    /// The user's reply — becomes a tool_result for the ask_user call,
    /// NOT a new user message.
    response: String,
}

#[derive(Debug, Serialize)]
struct AskUserResponseResponse {
    received: bool,
}

/// POST /api/sessions/:id/ask_user_response
///
/// Unblock the agent loop's ask_user wait so the same run resumes
/// with the reply as a tool_result.
async fn handle_ask_user_response(
    State(state): State<SharedState>,
    axum::extract::Path(id): axum::extract::Path<String>,
    Json(req): Json<AskUserResponseRequest>,
) -> Result<Json<AskUserResponseResponse>, (StatusCode, String)> {
    {
        let sessions = state.sessions.read().await;
        if !sessions.has_session(&id) {
            return Err((StatusCode::NOT_FOUND, format!("Session {id} not found")));
        }
    }

    let ask_rxs = state.ask_user_rxs.lock().unwrap();
    let slot = match ask_rxs.get(&id) {
        Some(s) => s.clone(),
        None => {
            return Err((
                StatusCode::CONFLICT,
                format!("Session {id} has no pending ask_user (agent isn't waiting)"),
            ));
        }
    };
    *slot.lock().unwrap() = Some(req.response.clone());

    let _ = state.sse_bus.send(SseEvent::system(
        &id,
        "ask_user reply received; agent resuming the same run",
    ));

    Ok(Json(AskUserResponseResponse { received: true }))
}

// ── Checkpoint handlers ──

/// GET /api/sessions/:id/checkpoints
async fn list_session_checkpoints_handler(
    State(state): State<SharedState>,
    axum::extract::Path(id): axum::extract::Path<String>,
) -> Result<Json<CheckpointListResponse>, (StatusCode, String)> {
    let cp_dir = oz_core::checkpoint::checkpoint_dir(std::path::Path::new(&state.working_dir));
    let checkpoints = oz_core::checkpoint::list_session_checkpoints(&cp_dir, &id);
    Ok(Json(CheckpointListResponse { checkpoints }))
}

/// GET /api/checkpoints
async fn list_all_checkpoints_handler(
    State(state): State<SharedState>,
) -> Json<CheckpointListResponse> {
    let cp_dir = oz_core::checkpoint::checkpoint_dir(std::path::Path::new(&state.working_dir));
    let checkpoints = oz_core::checkpoint::list_all_checkpoints(&cp_dir);
    Json(CheckpointListResponse { checkpoints })
}

/// POST /api/sessions/:id/resume
/// Resume a session from the latest (or specified) checkpoint.
async fn resume_session_handler(
    State(state): State<SharedState>,
    axum::extract::Path(id): axum::extract::Path<String>,
    Json(req): Json<ResumeRequest>,
) -> Result<Json<serde_json::Value>, (StatusCode, String)> {
    let cp_dir = oz_core::checkpoint::checkpoint_dir(std::path::Path::new(&state.working_dir));

    let checkpoint = if req.turn > 0 {
        oz_core::checkpoint::load_checkpoint_at_turn(&cp_dir, &id, req.turn)
    } else {
        oz_core::checkpoint::load_latest_loop_checkpoint(&cp_dir, &id)
    };

    match checkpoint {
        Some(cp) => {
            // Restore messages to session store
            let mut sessions = state.sessions.write().await;
            if let Some(session) = sessions.get_mut(&id) {
                session.messages = cp
                    .messages
                    .iter()
                    .map(|m| {
                        serde_json::json!({
                            "role": m.role,
                            "content": m.content,
                            "turn": cp.turn,
                        })
                    })
                    .collect();
                session.status = SessionStatus::Idle;
            }
            drop(sessions);

            // If a new message is provided, automatically trigger a chat with resume context
            if let Some(msg) = req.message {
                let resume_prompt = format!(
                    "[RESUMED FROM CHECKPOINT turn {}]\n\nPrevious state was paused with exit: {:?}\n\nNew instruction: {}",
                    cp.turn, cp.exit_reason, msg
                );
                let _ = state.sse_bus.send(SseEvent::system(
                    &id,
                    &format!("Resumed from turn {} with new instruction", cp.turn),
                ));
                // Return info so the frontend can re-send the chat request with the resume prompt
                Ok(Json(serde_json::json!({
                    "status": "resumed",
                    "turn": cp.turn,
                    "message_count": cp.messages.len(),
                    "resume_prompt": resume_prompt,
                })))
            } else {
                Ok(Json(serde_json::json!({
                    "status": "resumed",
                    "turn": cp.turn,
                    "message_count": cp.messages.len(),
                })))
            }
        }
        None => Err((
            StatusCode::NOT_FOUND,
            format!("No checkpoint found for session {id}"),
        )),
    }
}

// ── Upgrade handler ──

/// POST /api/upgrade
/// Trigger a self-upgrade. In WebUI mode, returns instructions to run CLI upgrade.
async fn handle_upgrade() -> Json<serde_json::Value> {
    tracing::info!("Upgrade requested via API");

    // In WebUI mode, the server can't restart itself.
    // The user should run `ga upgrade --force` from the terminal.
    // For daemon mode, the daemon handles the upgrade signal.
    Json(serde_json::json!({
        "status": "upgrade_instruction",
        "message": "To upgrade, run 'ga upgrade --force' from the terminal, or restart the daemon.",
        "cli_command": "ga upgrade --force",
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::Body;
    use axum::http::{Request, StatusCode};
    use axum::routing::delete;
    use tower::ServiceExt;

    // ── SessionStore tests ──

    #[test]
    fn test_session_store_create_list() {
        let mut store = SessionStore::new();
        assert!(store.list().is_empty());

        let info = store.create("test-session");
        assert_eq!(info.name, "test-session");
        assert!(!info.id.is_empty());

        let list = store.list();
        assert_eq!(list.len(), 1);
        assert_eq!(list[0].name, "test-session");
    }

    #[test]
    fn test_session_store_has_and_get() {
        let mut store = SessionStore::new();
        store.create_with_id("sess-1", "session one");

        assert!(store.has_session("sess-1"));
        assert!(!store.has_session("nonexistent"));

        let entry = store.get("sess-1");
        assert!(entry.is_some());
        assert_eq!(entry.unwrap().info.name, "session one");
    }

    #[test]
    fn test_session_store_delete() {
        let mut store = SessionStore::new();
        store.create_with_id("sess-1", "test");
        assert!(store.delete("sess-1"));
        assert!(!store.has_session("sess-1"));
        assert!(!store.delete("nonexistent"));
    }

    #[test]
    fn test_session_store_status_tracking() {
        let mut store = SessionStore::new();
        store.create_with_id("sess-1", "test");

        let entry = store.get_mut("sess-1").unwrap();
        entry.status = SessionStatus::Running;

        let entry = store.get("sess-1").unwrap();
        assert_eq!(entry.status, SessionStatus::Running);
    }

    #[test]
    fn test_session_store_message_counts() {
        let mut store = SessionStore::new();
        store.create_with_id("sess-1", "test");

        let entry = store.get_mut("sess-1").unwrap();
        entry.messages.push(serde_json::json!({"role": "user"}));
        entry
            .messages
            .push(serde_json::json!({"role": "assistant"}));

        assert_eq!(store.get("sess-1").unwrap().messages.len(), 2);
    }

    // ── SseBus tests ──

    #[tokio::test]
    async fn test_sse_bus_send_receive() {
        let bus = SseBus::new(16);
        let mut rx = bus.subscribe();

        let event = SseEvent::protocol_v1("sess-1", "{\"type\":\"text_delta\"}");
        bus.send(event.clone()).unwrap();

        let received = rx.recv().await.unwrap();
        assert_eq!(received.session_id, "sess-1");
        assert_eq!(received.event_type, "protocol_v1");
        assert_eq!(received.data, "{\"type\":\"text_delta\"}");
    }

    #[tokio::test]
    async fn test_sse_bus_multiple_subscribers() {
        let bus = SseBus::new(16);
        let mut rx1 = bus.subscribe();
        let mut rx2 = bus.subscribe();

        bus.send(SseEvent::system("sess-1", "test")).unwrap();

        let r1 = rx1.recv().await.unwrap();
        let r2 = rx2.recv().await.unwrap();
        assert_eq!(r1.data, "test");
        assert_eq!(r2.data, "test");
    }

    #[test]
    fn test_sse_event_types() {
        let e = SseEvent::error("s1", "error msg");
        assert_eq!(e.event_type, "error");

        let e = SseEvent::system("s1", "system msg");
        assert_eq!(e.event_type, "system");

        let e = SseEvent::protocol_v1("s1", "{}");
        assert_eq!(e.event_type, "protocol_v1");
    }

    // ── HTTP endpoint tests ──

    fn test_app_state() -> SharedState {
        let tmp = tempfile::tempdir().unwrap();
        let config_path = tmp.path().join("mykey.toml");
        std::fs::write(&config_path, "").unwrap();

        Arc::new(AppState::new(
            config_path.to_string_lossy().to_string(),
            tmp.path().to_string_lossy().to_string(),
            tmp.path().to_string_lossy().to_string(),
        ))
    }

    #[tokio::test]
    async fn test_create_session_endpoint() {
        let state = test_app_state();
        let app = Router::new()
            .route("/api/sessions", post(create_session))
            .with_state(state);

        let req = Request::builder()
            .method("POST")
            .uri("/api/sessions")
            .header("content-type", "application/json")
            .body(Body::from(r#"{"name":"test-session"}"#))
            .unwrap();

        let resp = app.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::OK);

        let body = axum::body::to_bytes(resp.into_body(), 1024 * 1024)
            .await
            .unwrap();
        let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert!(json.get("session_id").is_some());
        assert_eq!(json["name"], "test-session");
    }

    #[tokio::test]
    async fn test_list_sessions_endpoint() {
        let state = test_app_state();
        {
            let mut sessions = state.sessions.write().await;
            sessions.create_with_id("test-1", "session 1");
            sessions.create_with_id("test-2", "session 2");
        }

        let app = Router::new()
            .route("/api/sessions", get(list_sessions))
            .with_state(state);

        let req = Request::builder()
            .uri("/api/sessions")
            .body(Body::empty())
            .unwrap();

        let resp = app.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::OK);

        let body = axum::body::to_bytes(resp.into_body(), 1024 * 1024)
            .await
            .unwrap();
        let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
        let sessions = json["sessions"].as_array().unwrap();
        assert_eq!(sessions.len(), 2);
    }

    #[tokio::test]
    async fn test_get_session_endpoint() {
        let state = test_app_state();
        {
            let mut sessions = state.sessions.write().await;
            sessions.create_with_id("test-1", "my session");
            let entry = sessions.get_mut("test-1").unwrap();
            entry
                .messages
                .push(serde_json::json!({"role": "user", "content": "hello"}));
        }

        let app = Router::new()
            .route("/api/sessions/:id", get(get_session))
            .with_state(state);

        let req = Request::builder()
            .uri("/api/sessions/test-1")
            .body(Body::empty())
            .unwrap();

        let resp = app.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::OK);

        let body = axum::body::to_bytes(resp.into_body(), 1024 * 1024)
            .await
            .unwrap();
        let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(json["name"], "my session");
        assert_eq!(json["messages"].as_array().unwrap().len(), 1);
    }

    #[tokio::test]
    async fn test_get_session_not_found() {
        let state = test_app_state();
        let app = Router::new()
            .route("/api/sessions/:id", get(get_session))
            .with_state(state);

        let req = Request::builder()
            .uri("/api/sessions/nonexistent")
            .body(Body::empty())
            .unwrap();

        let resp = app.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn test_delete_session_endpoint() {
        let state = test_app_state();
        {
            let mut sessions = state.sessions.write().await;
            sessions.create_with_id("test-1", "delete me");
        }

        let app = Router::new()
            .route("/api/sessions/:id", delete(delete_session))
            .with_state(state.clone());

        let req = Request::builder()
            .method("DELETE")
            .uri("/api/sessions/test-1")
            .body(Body::empty())
            .unwrap();

        let resp = app.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::OK);

        let sessions = state.sessions.read().await;
        assert!(!sessions.has_session("test-1"));
    }

    #[tokio::test]
    async fn test_stop_session_endpoint() {
        let state = test_app_state();
        {
            let mut sessions = state.sessions.write().await;
            sessions.create_with_id("test-1", "stop me");
        }
        // Register a stop signal so the endpoint can find a running agent.
        {
            let mut signals = state.stop_signals.lock().unwrap();
            signals.insert("test-1".to_string(), Arc::new(AtomicBool::new(false)));
        }

        let app = Router::new()
            .route("/api/sessions/:id/stop", post(stop_session))
            .with_state(state.clone());

        let req = Request::builder()
            .method("POST")
            .uri("/api/sessions/test-1/stop")
            .body(Body::empty())
            .unwrap();

        let resp = app.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::OK);

        let sessions = state.sessions.read().await;
        let entry = sessions.get("test-1").unwrap();
        assert_eq!(entry.status, SessionStatus::Stopped);
    }

    #[tokio::test]
    async fn test_stop_session_not_found() {
        let state = test_app_state();
        let app = Router::new()
            .route("/api/sessions/:id/stop", post(stop_session))
            .with_state(state);

        let req = Request::builder()
            .method("POST")
            .uri("/api/sessions/nonexistent/stop")
            .body(Body::empty())
            .unwrap();

        let resp = app.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn test_chat_endpoint_creates_session() {
        let state = test_app_state();
        let app = Router::new()
            .route("/api/chat", post(handle_chat))
            .with_state(state.clone());

        let req = Request::builder()
            .method("POST")
            .uri("/api/chat")
            .header("content-type", "application/json")
            .body(Body::from(
                r#"{"message":"hello","session_id":"chat-test-1","session_name":"chat test"}"#,
            ))
            .unwrap();

        let resp = app.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::OK);

        let sessions = state.sessions.read().await;
        assert!(sessions.has_session("chat-test-1"));

        let entry = sessions.get("chat-test-1").unwrap();
        assert_eq!(entry.messages.len(), 1);
        assert_eq!(entry.messages[0]["role"], "user");
        assert_eq!(entry.messages[0]["content"], "hello");
    }

    #[tokio::test]
    async fn test_sse_endpoint_returns_stream() {
        let state = test_app_state();
        let app = Router::new()
            .route("/api/events", get(handle_sse))
            .with_state(state);

        let req = Request::builder()
            .uri("/api/events")
            .header("accept", "text/event-stream")
            .body(Body::empty())
            .unwrap();

        let resp = app.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let content_type = resp
            .headers()
            .get("content-type")
            .and_then(|v| v.to_str().ok())
            .unwrap_or("");
        assert!(content_type.contains("text/event-stream"));
    }
}
