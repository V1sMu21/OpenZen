//! Tauri IPC command handlers for the OpenZen desktop app.

use std::sync::Arc;

use oz_config::mykey::{MyKeyConfig, SessionType};
use oz_core_types::{LlmClient, Message};
use oz_llm;
use oz_server::webui::sessions::{SessionInfo, SessionStatus, SessionStore};
use oz_server::webui::sse_bus::SseEvent;
use tauri::{AppHandle, Emitter, Manager, State};

use crate::{
    data_dir, debug_log, lock_poison_guard, runner, AppState, ModelEntry, SendMessageResponse,
};

#[tauri::command]
pub fn clear_session_messages(
    session_id: String,
    state: State<'_, Arc<AppState>>,
) -> serde_json::Value {
    if lock_poison_guard(&state.running_agents).contains_key(&session_id) {
        return serde_json::json!({"error": "session is running; stop the agent first"});
    }
    let mut store = lock_poison_guard(&state.sessions);
    if let Some(s) = store.get_mut(&session_id) {
        s.messages.clear();
        store.save();
        serde_json::json!({"status":"ok"})
    } else {
        serde_json::json!({"error":"session not found"})
    }
}

#[tauri::command]
pub fn ping(state: State<'_, Arc<AppState>>) -> serde_json::Value {
    let sessions = lock_poison_guard(&state.sessions);
    let agent_count = lock_poison_guard(&state.running_agents).len();
    let cfg_path = std::path::Path::new(&state.config_path);
    let models: Vec<serde_json::Value> = match MyKeyConfig::from_file(cfg_path) {
        Ok(cfg) => cfg
            .sessions
            .iter()
            .map(|(name, sess)| {
                let provider = match cfg.session_type(name) {
                    SessionType::Claude | SessionType::NativeClaude => "claude",
                    SessionType::Oai | SessionType::NativeOai | SessionType::Mixin => "openai",
                };
                serde_json::json!({
                    "name": name,
                    "model": sess.model,
                    "provider": provider,
                    "context_win": sess.context_win,
                    "is_local": crate::is_local_deploy(&sess.apibase),
                })
            })
            .collect(),
        Err(e) => {
            debug_log(&format!("ping: config error: {}", e));
            vec![]
        }
    };
    debug_log(&format!(
        "ping: {} models, config_path={}",
        models.len(),
        cfg_path.display()
    ));
    serde_json::json!({
        "status": "ok",
        "service": "openzen-tauri",
        "uptime": chrono::Utc::now().to_rfc3339(),
        "sessions": sessions.list().len(),
        "running_agents": agent_count,
        "scheduler": state.scheduler_started.load(std::sync::atomic::Ordering::Relaxed),
        "models": models,
        "model_count": models.len(),
        "working_dir": state.working_dir,
    })
}

#[tauri::command]
pub fn get_working_dir(state: State<'_, Arc<AppState>>) -> String {
    state.working_dir.clone()
}

#[tauri::command]
pub fn get_working_dir_for_session(session_id: String, state: State<'_, Arc<AppState>>) -> String {
    // Resolve working directory from session's project, matching runner.rs logic
    let store = lock_poison_guard(&state.sessions);
    let pid = store.get(&session_id).and_then(|e| e.project_id.clone());
    drop(store);
    if let Some(ref pid) = pid {
        let projects = lock_poison_guard(&state.projects);
        let found = projects.iter().find(|p| p.id == *pid);
        if let Some(p) = found {
            return p.root_path.clone();
        }
    }
    state.working_dir.clone()
}

#[tauri::command]
pub fn list_models(state: State<'_, Arc<AppState>>) -> Vec<ModelEntry> {
    let cfg_path = std::path::Path::new(&state.config_path);
    debug_log(&format!("list_models: config_path={}", cfg_path.display()));
    debug_log(&format!("list_models: file_exists={}", cfg_path.exists()));
    let models = match MyKeyConfig::from_file(cfg_path) {
        Ok(cfg) => {
            let count = cfg.sessions.len();
            debug_log(&format!("list_models: parsed OK, {} sessions", count));
            cfg.sessions
                .iter()
                .map(|(name, sess)| {
                    let provider = match cfg.session_type(name) {
                        SessionType::Claude | SessionType::NativeClaude => "claude",
                        SessionType::Oai | SessionType::NativeOai | SessionType::Mixin => "openai",
                    };
                    let is_local = crate::is_local_deploy(&sess.apibase);
                    debug_log(&format!(
                        "list_models:   [{}] model={} provider={} ctx={} local={}",
                        name, sess.model, provider, sess.context_win, is_local
                    ));
                    ModelEntry {
                        name: name.clone(),
                        model: sess.model.clone(),
                        provider: provider.to_string(),
                        context_win: sess.context_win,
                        is_local,
                    }
                })
                .collect()
        }
        Err(e) => {
            debug_log(&format!("list_models: parse error: {e}"));
            vec![]
        }
    };
    debug_log(&format!("list_models: returning {} entries", models.len()));
    models
}

#[tauri::command]
pub fn get_dashboard_stats() -> serde_json::Value {
    serde_json::json!({ "status": "ok", "service": "openzen-tauri" })
}

#[tauri::command]
pub fn list_sessions(
    project_id: Option<String>,
    state: State<'_, Arc<AppState>>,
) -> Vec<SessionInfo> {
    let mut store = lock_poison_guard(&state.sessions);
    store.reload();
    let interrupted: Vec<String> = store
        .list()
        .iter()
        .filter(|s| s.status == "running")
        .map(|s| s.id.clone())
        .collect();
    for sid in &interrupted {
        let agents = lock_poison_guard(&state.running_agents);
        if !agents.contains_key(sid) {
            drop(agents);
            recover_session_from_checkpoints(&mut store, sid, &state.working_dir);
        }
    }
    let sessions = store.list();
    if let Some(pid) = project_id {
        sessions
            .into_iter()
            .filter(|s| s.project_id.as_deref() == Some(&pid))
            .collect()
    } else {
        sessions
    }
}

#[tauri::command]
pub fn create_session(name: Option<String>, state: State<'_, Arc<AppState>>) -> serde_json::Value {
    let session_name = name.unwrap_or_else(|| {
        let ts = chrono::Local::now();
        format!("Session {}", ts.format("%H:%M"))
    });
    let working_dir = state.working_dir.clone();
    let info = lock_poison_guard(&state.sessions).create_with_project(
        &session_name,
        None,
        None,
        Some(&working_dir),
    );
    serde_json::json!({ "session_id": info.id, "name": info.name, "working_dir": info.working_dir })
}

#[tauri::command]
pub fn create_session_in_project(
    project_id: Option<String>,
    name: Option<String>,
    state: State<'_, Arc<AppState>>,
) -> serde_json::Value {
    let session_name = name.unwrap_or_else(|| {
        let ts = chrono::Local::now();
        format!("Session {}", ts.format("%H:%M"))
    });
    let project_name = project_id.as_ref().and_then(|pid| {
        let projects = lock_poison_guard(&state.projects);
        projects
            .iter()
            .find(|p| p.id == *pid)
            .map(|p| p.name.clone())
    });
    let working_dir = project_id
        .as_ref()
        .and_then(|pid| {
            let projects = lock_poison_guard(&state.projects);
            projects
                .iter()
                .find(|p| p.id == *pid)
                .map(|p| p.root_path.clone())
        })
        .unwrap_or_else(|| state.working_dir.clone());
    let info = lock_poison_guard(&state.sessions).create_with_project(
        &session_name,
        project_id.as_deref(),
        project_name.as_deref(),
        Some(&working_dir),
    );
    debug_log(&format!(
        "create_session_in_project: session_id={}, project_id={:?}, project_name={:?}, working_dir={}",
        info.id, project_id, project_name, working_dir
    ));
    serde_json::json!({ "session_id": info.id, "name": info.name, "project_id": project_id, "project_name": project_name, "working_dir": working_dir })
}

#[tauri::command]
pub fn move_session_to_project(
    session_id: String,
    project_id: String,
    state: State<'_, Arc<AppState>>,
) -> Result<(), String> {
    let is_running = {
        let agents = lock_poison_guard(&state.running_agents);
        agents.contains_key(&session_id)
    };
    if is_running {
        return Err("Please stop the session before moving it".to_string());
    }

    let target_exists = {
        let projects = lock_poison_guard(&state.projects);
        projects.iter().any(|p| p.id == project_id)
    };
    if !target_exists {
        return Err("Target project not found".to_string());
    }

    let current_project_id = {
        let store = lock_poison_guard(&state.sessions);
        store.get(&session_id).and_then(|e| e.project_id.clone())
    };

    if current_project_id.as_deref() == Some(&project_id) {
        return Ok(());
    }

    let target_root = {
        let projects = lock_poison_guard(&state.projects);
        projects
            .iter()
            .find(|p| p.id == project_id)
            .map(|p| p.root_path.clone())
    };
    let Some(target_root) = target_root else {
        return Err("Target project not found".to_string());
    };

    let target_broken = !std::path::Path::new(&target_root).is_dir();
    if target_broken {
        return Err("Target project directory no longer exists (broken project)".to_string());
    }

    lock_poison_guard(&state.sessions).move_to_project(&session_id, &project_id, &target_root);

    debug_log(&format!(
        "move_session_to_project: session={} from={:?} to={}",
        session_id, current_project_id, project_id
    ));
    Ok(())
}

#[tauri::command]
pub fn get_session(id: String, state: State<'_, Arc<AppState>>) -> serde_json::Value {
    let mut store = lock_poison_guard(&state.sessions);
    match store.get(&id) {
        Some(entry) => {
            let wd = store
                .get(&id)
                .and_then(|e| e.working_dir.clone())
                .unwrap_or_else(|| state.working_dir.clone());
            if entry.status == SessionStatus::Running {
                let agents = lock_poison_guard(&state.running_agents);
                if !agents.contains_key(&id) {
                    // Use the session's own working_dir (project root)
                    // so checkpoints are found for project sessions.
                    recover_session_from_checkpoints(&mut store, &id, &wd);
                }
            } else if store
                .get(&id)
                .map(|e| !has_assistant_message(e))
                .unwrap_or(true)
            {
                // Not running and no assistant message persisted: restore the
                // full conversation (and todos) from the checkpoint. This is
                // the "bubbles vanished after restart" case — the agent was
                // killed mid-task before after_run could save.
                recover_session_from_checkpoints(&mut store, &id, &wd);
            }
            serde_json::to_value(store.get(&id)).unwrap_or_default()
        }
        None => serde_json::json!({ "error": "not found" }),
    }
}

/// True when the session store already contains at least one assistant
/// message (the normal after_run path persisted it).
fn has_assistant_message(entry: &oz_server::webui::sessions::SessionEntry) -> bool {
    entry.messages.iter().any(|m| {
        m.get("role").and_then(|v| v.as_str()) == Some("assistant")
            || m.get("tool_results").is_some()
    })
}

/// Pop trailing messages until the last user message with non-empty text.
/// Assistant turns, user-role tool_results carriers (empty content) and any
/// system summaries are discarded. Returns the seed text for a regenerate.
fn pop_regenerate_seed(messages: &mut Vec<serde_json::Value>) -> Option<String> {
    while let Some(m) = messages.pop() {
        if m.get("role").and_then(|v| v.as_str()) == Some("user") {
            let content = m.get("content").and_then(|v| v.as_str()).unwrap_or("");
            if !content.is_empty() {
                return Some(content.to_string());
            }
        }
        // assistant turn / tool_results carrier / system summary — keep walking
    }
    None
}

fn recover_session_from_checkpoints(store: &mut SessionStore, session_id: &str, working_dir: &str) {
    let cp_dir = oz_core::checkpoint::checkpoint_dir(std::path::Path::new(working_dir));
    if let Some(cp) = oz_core::checkpoint::load_best_loop_checkpoint(&cp_dir, session_id) {
        if let Some(entry) = store.get_mut(session_id) {
            if !cp.todos.is_empty() {
                entry.todos = cp.todos.clone();
            }

            // Rebuild the full conversation from the checkpoint so a
            // restart shows the same bubbles (tool cards, thinking,
            // text) the user saw while the agent ran. Without this the
            // session would only show the raw trigger message, because
            // after_run never persisted mid-task (agent killed by
            // stop/abort before completing a turn).
            let rebuilt = checkpoint_messages_to_store(&cp, &entry.messages);
            entry.messages = rebuilt;
            entry.status = SessionStatus::Idle;
            store.save();
        }
    } else if let Some(entry) = store.get_mut(session_id) {
        entry.status = SessionStatus::Idle;
        store.save();
    }
}

/// Convert a checkpoint's internal messages into store messages the
/// frontend can render (streamEvents for assistant turns, tool_results
/// for the paired user turns). Preserves the session's own messages
/// (the trigger message) when present.
fn checkpoint_messages_to_store(
    cp: &oz_core::checkpoint::LoopCheckpoint,
    existing: &[serde_json::Value],
) -> Vec<serde_json::Value> {
    let mut out: Vec<serde_json::Value> = Vec::new();

    // Keep the session's own leading messages (trigger/user input).
    for m in existing {
        if m.get("role").and_then(|v| v.as_str()) == Some("assistant") {
            break; // stop at the first persisted assistant turn
        }
        out.push(m.clone());
    }

    let now = chrono::Utc::now();
    // When the session already carries the trigger message (user input),
    // the checkpoint's first user message is the same trigger replayed —
    // skip it to avoid a duplicated bubble.
    let existing_has_trigger = out
        .iter()
        .any(|m| m.get("role").and_then(|v| v.as_str()) == Some("user"));
    let mut skip_first_user = existing_has_trigger;
    for m in &cp.messages {
        let role = match m.role {
            oz_core_types::Role::User => "user",
            oz_core_types::Role::Assistant => "assistant",
            oz_core_types::Role::System => "system",
            oz_core_types::Role::Tool => "tool",
        };
        if role == "system" {
            continue;
        }
        if role == "user" && skip_first_user {
            skip_first_user = false;
            continue;
        }

        let mut text_parts: Vec<String> = Vec::new();
        let mut tool_uses: Vec<serde_json::Value> = Vec::new();
        let mut tool_results: Vec<serde_json::Value> = Vec::new();
        let mut thinking: Option<String> = None;

        for block in &m.content {
            match block {
                oz_core_types::ContentBlock::Text { text, .. } => {
                    if !text.is_empty() {
                        text_parts.push(text.clone());
                    }
                }
                oz_core_types::ContentBlock::Thinking { thinking: th, .. } => {
                    thinking = Some(th.clone());
                }
                oz_core_types::ContentBlock::ToolUse { id, name, input } => {
                    tool_uses.push(serde_json::json!({
                        "id": id,
                        "name": name,
                        "input": input,
                    }));
                }
                oz_core_types::ContentBlock::ToolResult {
                    tool_use_id,
                    content,
                    ..
                } => {
                    tool_results.push(serde_json::json!({
                        "tool_use_id": tool_use_id,
                        "content": content.as_text().unwrap_or_default(),
                    }));
                }
                oz_core_types::ContentBlock::ImageUrl { .. } => {}
            }
        }

        let text = text_parts.join("\n");
        if role == "assistant" {
            let mut events: Vec<serde_json::Value> = Vec::new();
            if thinking.is_some() {
                let tid = format!("rs_{}", tool_uses.len());
                events.push(serde_json::json!({
                    "type": "reasoning_start",
                    "id": tid,
                    "position": events.len(),
                }));
                events.push(serde_json::json!({
                    "type": "reasoning_delta",
                    "id": tid,
                    "text": thinking.unwrap_or_default(),
                }));
                events.push(serde_json::json!({
                    "type": "reasoning_end",
                    "id": tid,
                }));
            }
            for (i, tu) in tool_uses.iter().enumerate() {
                let tc_id = tu.get("id").and_then(|v| v.as_str()).unwrap_or("");
                let name = tu.get("name").and_then(|v| v.as_str()).unwrap_or("");
                let input = tu.get("input").unwrap_or(&serde_json::Value::Null);
                events.push(serde_json::json!({
                    "type": "tool_input_start",
                    "tool_call_id": tc_id,
                    "name": name,
                }));
                events.push(serde_json::json!({
                    "type": "tool_input_available",
                    "tool_call_id": tc_id,
                    "name": name,
                    "args": serde_json::to_string(input).unwrap_or_else(|_| "{}".into()),
                }));
                events.push(serde_json::json!({
                    "type": "tool_output_available",
                    "tool_call_id": tc_id,
                    "name": name,
                    "output": "",
                }));
            }
            if !text.is_empty() {
                let tid = format!("ts_{}_{}", now.timestamp_millis(), events.len());
                events.push(serde_json::json!({
                    "type": "text_start",
                    "id": tid,
                    "position": events.len(),
                }));
                events.push(serde_json::json!({
                    "type": "text_delta",
                    "id": tid,
                    "text": text,
                }));
                events.push(serde_json::json!({
                    "type": "text_end",
                    "id": tid,
                }));
            }
            let mut msg = serde_json::json!({
                "role": "assistant",
                "content": text,
                "timestamp": now.to_rfc3339(),
                "streamEvents": events,
            });
            if !events.is_empty() {
                msg["streamEvents"] = serde_json::Value::Array(events);
            }
            out.push(msg);
        } else if role == "user" && !tool_results.is_empty() {
            out.push(serde_json::json!({
                "role": "user",
                "content": text,
                "tool_results": tool_results,
                "timestamp": now.to_rfc3339(),
            }));
        } else if !text.is_empty() {
            out.push(serde_json::json!({
                "role": "user",
                "content": text,
                "timestamp": now.to_rfc3339(),
            }));
        }
    }

    // Only the LAST assistant message carries the real exit reason —
    // earlier restored turns were complete (tools ran, text streamed),
    // so marking them "interrupted" would show a "任务已停止" banner on
    // every historical bubble.
    if let Some(exit) = cp.exit_reason.as_deref().or(Some("interrupted")) {
        if let Some(last_asst) = out
            .iter_mut()
            .rev()
            .find(|m| m.get("role").and_then(|v| v.as_str()) == Some("assistant"))
        {
            last_asst["exitReason"] = serde_json::json!(exit);
        }
    }
    out
}

#[tauri::command]
pub fn delete_session(
    id: String,
    state: State<'_, Arc<AppState>>,
) -> Result<serde_json::Value, String> {
    if lock_poison_guard(&state.running_agents).contains_key(&id) {
        return Err("Session is running; stop the agent before deleting".to_string());
    }
    lock_poison_guard(&state.sessions).delete(&id);
    Ok(serde_json::json!({"status":"ok"}))
}

#[tauri::command]
pub fn rename_session(id: String, name: String, state: State<'_, Arc<AppState>>) {
    lock_poison_guard(&state.sessions).rename(&id, &name);
}

#[tauri::command]
pub async fn stop_session(
    id: String,
    state: State<'_, Arc<AppState>>,
) -> Result<serde_json::Value, String> {
    let state = state.inner().clone();
    if lock_poison_guard(&state.sessions)
        .get(&id)
        .map(|e| e.status != SessionStatus::Running)
        .unwrap_or(false)
    {
        return Ok(serde_json::json!({"status": "already_stopped"}));
    }
    stop_running_agent(&id, &state).await;
    Ok(serde_json::json!({"status": "ok"}))
}

/// Hard cap on user-supplied message bodies accepted over IPC — a webview
/// (or a buggy client) must not be able to force multi-GB strings into the
/// session store (P3/A8).
const MAX_MESSAGE_CHARS: usize = 1_000_000;

/// Inject a user message into a running agent session without interrupting it.
/// The message is appended to the session store and pushed to the agent's
/// intervention queue — the agent loop picks it up before the next LLM turn.
#[tauri::command]
pub fn inject_message(
    session_id: String,
    text: String,
    state: State<'_, Arc<AppState>>,
    app_handle: AppHandle,
) -> Result<serde_json::Value, String> {
    if text.chars().count() > MAX_MESSAGE_CHARS {
        return Err(format!(
            "Message too large (max {} characters)",
            MAX_MESSAGE_CHARS
        ));
    }
    // 1. Append to session store so the UI shows it immediately
    {
        let mut store = lock_poison_guard(&state.sessions);
        if let Some(entry) = store.get_mut(&session_id) {
            entry.messages.push(serde_json::json!({
                "role": "user",
                "content": text,
                "timestamp": chrono::Utc::now().to_rfc3339(),
            }));
        }
        store.save();
    }

    // 2. Push intervention into the agent's queue
    {
        let queues = lock_poison_guard(&state.intervention_queues);
        if let Some(queue) = queues.get(&session_id) {
            let intervention = oz_core::checkpoint::InterventionEvent {
                id: uuid::Uuid::new_v4().to_string(),
                timestamp: chrono::Utc::now().timestamp() as f64,
                kind: oz_core::checkpoint::InterventionKind::InjectInfo,
                content: text.clone(),
            };
            lock_poison_guard(queue).push_back(intervention);
            debug_log(&format!(
                "inject_message: pushed intervention to session={}",
                session_id
            ));
        } else {
            // Agent not running — just added to store, that's fine
            debug_log(&format!(
                "inject_message: no running agent for session={}, stored only",
                session_id
            ));
        }
    }

    // 3. Notify frontend to re-render
    let _ = app_handle.emit(
        "sse_event",
        serde_json::json!({
            "type": "protocol_v1",
            "data": { "type": "user_message_stored", "session_id": session_id }
        }),
    );

    Ok(serde_json::json!({"status": "ok"}))
}

/// Gracefully stop a running agent: signal → wait → detach if unresponsive.
/// Never force-aborts — the stop signal causes the agent loop to exit cleanly,
/// and `after_run` must execute to persist messages.
async fn stop_running_agent(session_id: &str, state: &Arc<AppState>) {
    {
        let map = lock_poison_guard(&state.stop_signals);
        if let Some(sig) = map.get(session_id) {
            sig.store(true, std::sync::atomic::Ordering::SeqCst);
        }
    }
    // Wait up to 10s for graceful exit via stop_signal
    for _ in 0..100 {
        {
            let agents = lock_poison_guard(&state.running_agents);
            if !agents.contains_key(session_id) {
                return;
            }
        } // MutexGuard dropped before await
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    }
    // Still running after 10s — detach, don't abort.
    // The task will finish naturally when stop_signal takes effect.
    // Move the handle to detached_agents: the task keeps running, but a
    // new run for this session will abort it (see abort_detached_agent)
    // so two agents can never write the same session concurrently.
    let handle = lock_poison_guard(&state.running_agents).remove(session_id);
    if let Some(handle) = handle {
        lock_poison_guard(&state.detached_agents).insert(session_id.to_string(), handle);
        debug_log(&format!(
            "stop_running_agent: detaching slow agent session={}",
            session_id
        ));
    }
}

/// Abort a detached (still-running) task for a session before a new run.
/// Called on send/regenerate/resume — the user explicitly started a new run,
/// so the stale task must not keep writing to the same session.
fn abort_detached_agent(session_id: &str, state: &Arc<AppState>) {
    if let Some(handle) = lock_poison_guard(&state.detached_agents).remove(session_id) {
        if !handle.is_finished() {
            handle.abort();
            debug_log(&format!(
                "abort_detached_agent: aborted stale task session={}",
                session_id
            ));
        }
    }
}

/// Serializes read-modify-write cycles on mykey.toml (add_platform,
/// remove_platform, …) so concurrent commands can't clobber each other.
static CONFIG_WRITE_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

/// RAII guard: removes a session's agent handles when its run task exits —
/// including the panic path. Without this, a panic inside
/// `run_agent_for_session` (e.g. a poisoned lock elsewhere) skips the
/// cleanup block and leaves the session stuck in "Running" forever, with
/// its JoinHandle leaking in `running_agents`.
struct AgentSessionGuard {
    state: Arc<AppState>,
    session_id: String,
}

impl Drop for AgentSessionGuard {
    fn drop(&mut self) {
        let my_id = tokio::task::try_id();
        if let Some(my_id) = my_id {
            let mut agents = lock_poison_guard(&self.state.running_agents);
            if let Some(h) = agents.get(&self.session_id) {
                if Some(h.id()) == Some(my_id) {
                    agents.remove(&self.session_id);
                }
            }
        }
        lock_poison_guard(&self.state.detached_agents).remove(&self.session_id);
        lock_poison_guard(&self.state.intervention_queues).remove(&self.session_id);
    }
}

#[tauri::command]
pub async fn send_message(
    message: String,
    session_id: String,
    session_name: Option<String>,
    model_name: Option<String>,
    app_handle: AppHandle,
    state: State<'_, Arc<AppState>>,
) -> Result<SendMessageResponse, String> {
    let state = state.inner().clone();
    if message.chars().count() > MAX_MESSAGE_CHARS {
        return Err(format!(
            "Message too large (max {} characters)",
            MAX_MESSAGE_CHARS
        ));
    }
    debug_log(&format!(
        "send_message: session_id={}, msg_len={}",
        session_id,
        message.len()
    ));

    {
        let mut store = lock_poison_guard(&state.sessions);
        let existed = store.has_session(&session_id);
        let existing_pid = store.get(&session_id).and_then(|e| e.project_id.clone());
        debug_log(&format!(
            "send_message: session_id={}, existed={}, existing_project_id={:?}",
            session_id, existed, existing_pid
        ));
        if !store.has_session(&session_id) {
            let name = session_name.clone().unwrap_or_else(|| {
                let ts = chrono::Local::now();
                format!("Session {}", ts.format("%H:%M"))
            });
            store.create_with_id(&session_id, &name);
        }
        if let Some(s) = store.get_mut(&session_id) {
            s.status = SessionStatus::Running;
            s.messages.push(serde_json::json!({
                "role": "user",
                "content": message,
                "timestamp": chrono::Utc::now().to_rfc3339(),
            }));
        }
        store.save();
        drop(store);
    }

    let session_id = session_id.clone();
    let state_clone: Arc<AppState> = state.clone();
    let app_clone = app_handle.clone();
    let session_id_clone = session_id.clone();
    let model_name_clone = model_name.clone();

    abort_detached_agent(&session_id, &state);

    {
        let agents = lock_poison_guard(&state.running_agents);
        if agents.contains_key(&session_id) {
            return Err("Another agent is already running for this session".to_string());
        }
        if agents.len() >= 3 {
            return Err("Too many concurrent agent sessions (max 3)".to_string());
        }
        drop(agents);
    }

    let handle = tokio::spawn(async move {
        // RAII cleanup — runs even if run_agent_for_session panics.
        let _cleanup = AgentSessionGuard {
            state: state_clone.clone(),
            session_id: session_id_clone.clone(),
        };
        if let Err(e) = runner::run_agent_for_session(
            &app_clone,
            &state_clone,
            &session_id_clone,
            model_name_clone.as_deref(),
            false,
        )
        .await
        {
            debug_log(&format!("run_agent error: {e}"));
            // Safety net: an early error return (config parse, session
            // missing, …) skips the runner's own status writeback — reset
            // Running → Idle here so the UI can't be stuck on "Running".
            {
                let mut store = lock_poison_guard(&state_clone.sessions);
                if let Some(s) = store.get_mut(&session_id_clone) {
                    if s.status == SessionStatus::Running {
                        s.status = SessionStatus::Idle;
                    }
                }
                store.save();
            }
            let _ = app_clone.emit(
                "sse_event",
                serde_json::to_value(&SseEvent::error(&session_id_clone, &e.to_string()))
                    .unwrap_or_default(),
            );
        }
    });

    lock_poison_guard(&state.running_agents).insert(session_id.clone(), handle);

    Ok(SendMessageResponse {
        session_id,
        status: "started".to_string(),
    })
}

#[tauri::command]
pub async fn regenerate(
    session_id: String,
    app_handle: AppHandle,
    state: State<'_, Arc<AppState>>,
) -> Result<serde_json::Value, String> {
    debug_log(&format!("regenerate: session_id={session_id}"));
    let state = state.inner().clone();

    abort_detached_agent(&session_id, &state);

    {
        let agents = lock_poison_guard(&state.running_agents);
        if agents.contains_key(&session_id) {
            return Err("Another agent is already running for this session".to_string());
        }
    }

    {
        let mut store = lock_poison_guard(&state.sessions);
        let session = store
            .get_mut(&session_id)
            .ok_or_else(|| format!("Session {session_id} not found"))?;

        // Walk back to the last user message that actually carries text.
        // Trailing assistant turns and user-role tool_results carriers
        // (persisted with empty content) are popped; without this, a
        // session whose last turn ended with tool results seeded an
        // empty user message and the runner bailed with
        // "No user message to process".
        let msg = pop_regenerate_seed(&mut session.messages)
            .ok_or_else(|| "No user message to regenerate".to_string())?;

        session.messages.push(serde_json::json!({
            "role": "user",
            "content": msg,
            "timestamp": chrono::Utc::now().to_rfc3339(),
        }));

        session.status = SessionStatus::Running;
        store.save();
    }

    let state_clone: Arc<AppState> = state.clone();
    let app_clone = app_handle.clone();
    let sid = session_id.clone();

    let handle = tokio::spawn(async move {
        if let Err(e) =
            runner::run_agent_for_session(&app_clone, &state_clone, &sid, None, false).await
        {
            debug_log(&format!("regenerate agent error: {e}"));
            let _ = app_clone.emit(
                "sse_event",
                serde_json::to_value(&SseEvent::error(&sid, &e.to_string())).unwrap_or_default(),
            );
        }
        let my_id = tokio::task::try_id();
        {
            let mut agents = lock_poison_guard(&state_clone.running_agents);
            if let Some(h) = agents.get(&sid) {
                if Some(h.id()) == my_id {
                    agents.remove(&sid);
                }
            }
        }
        lock_poison_guard(&state_clone.detached_agents).remove(&sid);
    });

    lock_poison_guard(&state.running_agents).insert(session_id, handle);

    Ok(serde_json::json!({ "status": "started" }))
}

#[tauri::command]
pub async fn resume_session(
    session_id: String,
    model_name: Option<String>,
    app_handle: AppHandle,
    state: State<'_, Arc<AppState>>,
) -> Result<SendMessageResponse, String> {
    debug_log(&format!("resume_session: session_id={session_id}"));
    let state = state.inner().clone();

    abort_detached_agent(&session_id, &state);

    {
        let agents = lock_poison_guard(&state.running_agents);
        if agents.contains_key(&session_id) {
            return Err("Agent is already running for this session; stop it first".to_string());
        }
    }

    // Resolve working directory from session's project, matching runner.rs logic
    let working_dir = {
        let store = lock_poison_guard(&state.sessions);
        let pid = store.get(&session_id).and_then(|e| e.project_id.clone());
        drop(store);
        if let Some(ref pid) = pid {
            let projects = lock_poison_guard(&state.projects);
            projects
                .iter()
                .find(|p| p.id == *pid)
                .map(|p| p.root_path.clone())
                .unwrap_or(state.working_dir.clone())
        } else {
            state.working_dir.clone()
        }
    };
    let cp_dir = oz_core::checkpoint::checkpoint_dir(std::path::Path::new(&working_dir));
    if oz_core::checkpoint::load_latest_loop_checkpoint(&cp_dir, &session_id).is_none() {
        return Err(
            "No checkpoint found for this session; the agent must have been run at least once"
                .to_string(),
        );
    }

    // Mark session as running. The agent loop uses checkpoint data directly
    // for resume; session store messages are preserved intact so the UI
    // retains streamEvents, tool cards, and thinking cards on reopen.
    {
        let mut store = lock_poison_guard(&state.sessions);
        if let Some(s) = store.get_mut(&session_id) {
            s.status = SessionStatus::Running;
        }
        store.save();
    }

    let session_id = session_id.clone();
    let state_clone: Arc<AppState> = state.clone();
    let app_clone = app_handle.clone();
    let sid = session_id.clone();

    let handle = tokio::spawn(async move {
        if let Err(e) = runner::run_agent_for_session(
            &app_clone,
            &state_clone,
            &sid,
            model_name.as_deref(),
            true,
        )
        .await
        {
            debug_log(&format!("resume_agent error: {e}"));
            let _ = app_clone.emit(
                "sse_event",
                serde_json::to_value(&SseEvent::error(&sid, &e.to_string())).unwrap_or_default(),
            );
        }
        let my_id = tokio::task::try_id();
        {
            let mut agents = lock_poison_guard(&state_clone.running_agents);
            if let Some(h) = agents.get(&sid) {
                if Some(h.id()) == my_id {
                    agents.remove(&sid);
                }
            }
        }
        lock_poison_guard(&state_clone.detached_agents).remove(&sid);
    });

    lock_poison_guard(&state.running_agents).insert(session_id.clone(), handle);

    Ok(SendMessageResponse {
        session_id,
        status: "resumed".to_string(),
    })
}

#[tauri::command]
pub fn ask_user_response(
    session_id: String,
    response: String,
    app_handle: AppHandle,
    state: State<'_, Arc<AppState>>,
) -> Result<serde_json::Value, String> {
    {
        let store = lock_poison_guard(&state.sessions);
        if !store.has_session(&session_id) {
            return Err(format!("Session {session_id} not found"));
        }
    }
    let ask_rxs = lock_poison_guard(&state.ask_user_rxs);
    let slot = match ask_rxs.get(&session_id) {
        Some(s) => s.clone(),
        None => {
            return Err(format!(
                "Session {session_id} has no pending ask_user (agent isn't waiting)"
            ));
        }
    };
    *lock_poison_guard(&slot) = Some(response);
    let _ = app_handle.emit(
        "sse_event",
        serde_json::to_value(&SseEvent::system(
            &session_id,
            "ask_user reply received; agent resuming the same run",
        ))
        .unwrap_or_default(),
    );
    Ok(serde_json::json!({ "received": true }))
}

#[tauri::command]
pub fn open_session_window(
    session_id: String,
    app_handle: AppHandle,
    state: State<'_, Arc<AppState>>,
) -> serde_json::Value {
    let label = format!("session-{session_id}");
    // Register the mapping so session-scoped events (e.g. approvals) are
    // routed to this window instead of being broadcast everywhere.
    lock_poison_guard(&state.session_windows).insert(session_id.clone(), label.clone());
    if app_handle.get_webview_window(&label).is_some() {
        if let Some(w) = app_handle.get_webview_window(&label) {
            let _ = w.show();
            let _ = w.set_focus();
        }
        return serde_json::json!({ "status": "focused", "label": label });
    }
    match tauri::WebviewWindowBuilder::new(
        &app_handle,
        &label,
        tauri::WebviewUrl::App("index.html".into()),
    )
    .title(format!("OpenZen — {session_id}"))
    .build()
    {
        Ok(_) => serde_json::json!({ "status": "opened", "label": label }),
        Err(e) => serde_json::json!({ "error": e.to_string() }),
    }
}

#[tauri::command]
pub async fn compress_session(
    id: String,
    state: State<'_, Arc<AppState>>,
    app_handle: AppHandle,
) -> Result<serde_json::Value, String> {
    let (
        before,
        after,
        saved_chars,
        saved_pct,
        messages_removed,
        before_chars,
        after_chars,
        metrics,
        template_summary,
        _llm,
    ) = {
        let mut store = lock_poison_guard(&state.sessions);
        let entry = match store.get_mut(&id) {
            Some(e) => e,
            None => return Err(format!("Session {id} not found")),
        };

        let mut messages: Vec<oz_core_types::Message> = entry
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
                    "user" => Some(oz_core_types::Message::user(&content)),
                    "assistant" => Some(oz_core_types::Message::assistant(&content)),
                    "system" => Some(oz_core_types::Message::system(&content)),
                    _ => None,
                }
            })
            .collect();

        let before_chars = oz_core::measure_usage(&messages).total_chars;
        let before = messages.len();

        let comp_config = oz_core::CompressionConfig::default();
        // Manual /compact is a user-invoked "force" action: it must fold
        // old turns into a summary regardless of how full the context
        // window is. Passing context_win=1 bypasses the trigger threshold
        // (same trick emergency_compress uses) so compression always runs
        // down to the min_messages floor instead of no-oping on small
        // sessions. The auto-compress path in the agent loop still uses
        // the real context window from config.
        let _saved = oz_core::compress_messages(&mut messages, 1, &comp_config, None);

        let after_chars = oz_core::measure_usage(&messages).total_chars;
        let after = messages.len();
        let saved_chars = before_chars.saturating_sub(after_chars);
        let saved_pct = if before_chars > 0 {
            ((saved_chars as f64 / before_chars as f64) * 100.0 * 10.0).round() / 10.0
        } else {
            0.0
        };

        let original_msgs = entry.messages.clone();
        entry.messages = oz_core::compress::match_messages_to_originals(&messages, &entry.messages);

        let metrics = oz_core::compress::CompressionMetrics::compute(
            before_chars,
            after_chars,
            before,
            after,
        );
        let removed_json: Vec<serde_json::Value> = {
            let surviving_ids: std::collections::HashSet<String> = entry
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
                .iter()
                .filter(|v| {
                    let id = format!(
                        "{}_{}",
                        v.get("role").and_then(|r| r.as_str()).unwrap_or(""),
                        v.get("content").and_then(|c| c.as_str()).unwrap_or("")
                    );
                    !surviving_ids.contains(&id)
                })
                .cloned()
                .collect()
        };
        let template_summary = oz_core::compress::build_compression_summary(&removed_json, "");

        store.save();

        let messages_removed = before.saturating_sub(after);
        (
            before,
            after,
            saved_chars,
            saved_pct,
            messages_removed,
            before_chars,
            after_chars,
            metrics,
            template_summary,
            None::<String>,
        )
    };

    let llm_summary = if messages_removed >= 4 {
        generate_compact_summary(&state, &template_summary).await
    } else {
        None
    };

    if let Some(ref summary) = llm_summary {
        let mut store = lock_poison_guard(&state.sessions);
        if let Some(entry) = store.get_mut(&id) {
            entry.messages.insert(
                0,
                serde_json::json!({
                    "role": "system",
                    "content": format!("[Compression summary]: {summary}"),
                    "timestamp": chrono::Utc::now().to_rfc3339(),
                }),
            );
            store.save();
        }
    }

    // Manual compress lacks LLM token counts; use default ratio (chars/4).
    let before_tokens = before_chars / 4;
    let after_tokens = after_chars / 4;
    let saved_tokens = before_tokens.saturating_sub(after_tokens);
    // System notification when the user isn't looking at the main window —
    // compression takes a while and users usually switch away to wait.
    {
        let lang = lock_poison_guard(&state.locale).clone();
        let body = if lang == "zh" {
            format!(
                "上下文压缩完成：{} → {} 条消息，节省 {:.1}% tokens",
                before, after, saved_pct
            )
        } else {
            format!(
                "Context compressed: {} → {} messages, saved {:.1}% tokens",
                before, after, saved_pct
            )
        };
        crate::notify_if_unfocused(&app_handle, "OpenZen", &body);
    }
    Ok(serde_json::json!({
        "session_id": id,
        "before_chars": before_chars,
        "after_chars": after_chars,
        "saved_chars": saved_chars,
        "before_tokens": before_tokens,
        "after_tokens": after_tokens,
        "saved_tokens": saved_tokens,
        "saved_pct": saved_pct,
        "messages_removed": messages_removed,
        "metrics": metrics.summary(),
        "summary": template_summary,
        "llm_summary": llm_summary,
        "strategy": format!("compressed {}→{} messages, saved {:.1}% tokens{}",
            before, after, saved_pct,
            if llm_summary.is_some() { " (LLM summary)" } else { " (template)" }),
    }))
}

async fn generate_compact_summary(state: &AppState, template: &str) -> Option<String> {
    let config_path = state.config_path.clone();
    let cfg = oz_config::mykey::MyKeyConfig::from_file(std::path::Path::new(&config_path)).ok()?;
    // Manual /compact must use the same summary model as the agent
    // loop's auto-compression (summary_model), not default_session,
    // so local deployments get a small fast model for the summary.
    let (sess_name, sess_config): (String, oz_config::mykey::SessionConfig) =
        if let Some(ref name) = cfg.summary_model {
            let found = cfg.get(name).or_else(|| {
                cfg.sessions
                    .iter()
                    .find(|(_, s)| s.model == *name)
                    .map(|(_, s)| s)
            });
            if let Some(sc) = found {
                (name.clone(), sc.clone())
            } else {
                return None;
            }
        } else {
            let name = cfg.default_session.as_deref().unwrap_or("claude_sonnet");
            (name.to_string(), cfg.get(name)?.clone())
        };
    let sess_type = cfg.session_type(&sess_name);

    let backend: Box<dyn oz_llm::Session> = match sess_type {
        SessionType::Claude => Box::new(oz_llm::ClaudeSession::new(sess_config.clone())),
        SessionType::Oai => Box::new(oz_llm::OaiSession::new(sess_config.clone())),
        SessionType::NativeClaude => {
            Box::new(oz_llm::NativeClaudeSession::new(sess_config.clone()))
        }
        SessionType::NativeOai => Box::new(oz_llm::NativeOAISession::new(sess_config.clone())),
        _ => return None,
    };
    let mut client = oz_llm::NativeToolClient::new(backend);
    let prompt = Message::user(&format!(
        "Summarize what was discussed in these conversation fragments \
         in ONE short sentence (max 30 words). Do NOT re-execute or \
         continue the conversation.\n\n{template}"
    ));
    let msgs = [prompt];
    let result =
        tokio::time::timeout(std::time::Duration::from_secs(10), client.chat(&msgs, &[])).await;
    match result {
        Ok(Ok(resp)) if !resp.content.is_empty() => Some(resp.content),
        _ => None,
    }
}

#[tauri::command]
pub fn get_locale(state: State<'_, Arc<AppState>>) -> String {
    lock_poison_guard(&state.locale).clone()
}

#[tauri::command]
pub fn set_locale(
    lang: String,
    state: State<'_, Arc<AppState>>,
    app: AppHandle,
) -> Result<(), String> {
    let lang = lang.trim().to_lowercase();
    if lang != "zh" && lang != "en" {
        return Err(format!("Unsupported locale: {lang}"));
    }
    *lock_poison_guard(&state.locale) = lang.clone();
    let path = data_dir().join("locale.json");
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let content = serde_json::json!({ "lang": &lang });
    if let Ok(json) = serde_json::to_string_pretty(&content) {
        let _ = std::fs::write(&path, json);
    }
    let _ = app.emit("language-changed", serde_json::json!({ "lang": &lang }));
    Ok(())
}

/// Add or update a messaging platform configuration in mykey.toml.
/// Agent calls this once with credentials — no TOML editing needed.
#[tauri::command]
pub fn add_platform(
    state: State<'_, Arc<AppState>>,
    name: String,
    app_id: Option<String>,
    app_secret: Option<String>,
    bot_token: Option<String>,
    default_model: Option<String>,
    proxy: Option<String>,
    allowed_users: Option<Vec<String>>,
    sandbox: Option<bool>,
) -> Result<String, String> {
    let _lock = lock_poison_guard(&CONFIG_WRITE_LOCK);
    let path = std::path::Path::new(&state.config_path);
    let content = std::fs::read_to_string(path).map_err(|e| format!("read config: {e}"))?;

    let mut root: toml::Value = toml::from_str(&content).map_err(|e| format!("parse TOML: {e}"))?;
    let root_table = root.as_table_mut().ok_or("root is not a table")?;

    let platforms = root_table
        .entry("platforms")
        .or_insert_with(|| toml::Value::Table(toml::Table::new()));
    let platforms_table = platforms.as_table_mut().ok_or("platforms is not a table")?;

    let mut entry = toml::Table::new();
    entry.insert("enabled".into(), toml::Value::Boolean(true));
    if let Some(id) = &app_id {
        entry.insert("app_id".into(), toml::Value::String(id.clone()));
    }
    if let Some(secret) = &app_secret {
        entry.insert("app_secret".into(), toml::Value::String(secret.clone()));
    }
    if let Some(token) = &bot_token {
        entry.insert("bot_token".into(), toml::Value::String(token.clone()));
    }
    if let Some(model) = &default_model {
        entry.insert("default_model".into(), toml::Value::String(model.clone()));
    }
    if let Some(p) = &proxy {
        entry.insert("proxy".into(), toml::Value::String(p.clone()));
    }
    if let Some(s) = sandbox {
        entry.insert("sandbox".into(), toml::Value::Boolean(s));
    }
    if let Some(users) = &allowed_users {
        if !users.is_empty() {
            let arr: Vec<toml::Value> = users
                .iter()
                .map(|u| toml::Value::String(u.clone()))
                .collect();
            entry.insert("allowed_users".into(), toml::Value::Array(arr));
        }
    }

    platforms_table.insert(name.clone(), toml::Value::Table(entry));

    let output = toml::to_string(&root).map_err(|e| format!("serialize TOML: {e}"))?;
    // Atomic write (tmp + rename) so a crash can't truncate the config.
    let tmp = path.with_extension(format!("toml.tmp.{}", std::process::id()));
    std::fs::write(&tmp, &output).map_err(|e| format!("write config: {e}"))?;
    std::fs::rename(&tmp, path).map_err(|e| format!("rename config: {e}"))?;
    // mykey.toml holds platform secrets (app_secret / bot_token) — restrict
    // to owner-only so other local users can't read them.
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600));
    }

    Ok(format!("Platform '{name}' configured in mykey.toml. Rebuild with `cargo build --release` and restart."))
}

#[tauri::command]
pub fn get_crystallization(state: State<'_, Arc<AppState>>) -> bool {
    state
        .crystallization_enabled
        .load(std::sync::atomic::Ordering::Relaxed)
}

#[tauri::command]
pub fn set_crystallization(enabled: bool, state: State<'_, Arc<AppState>>) {
    state
        .crystallization_enabled
        .store(enabled, std::sync::atomic::Ordering::Relaxed);
}

#[tauri::command]
pub fn get_full_access(state: State<'_, Arc<AppState>>) -> bool {
    state.full_access.load(std::sync::atomic::Ordering::Relaxed)
}

#[tauri::command]
pub fn set_full_access(enabled: bool, state: State<'_, Arc<AppState>>) {
    state
        .full_access
        .store(enabled, std::sync::atomic::Ordering::Relaxed);
}

#[cfg(test)]
mod tests {
    use super::*;
    use oz_core_types::{ContentBlock, Message};

    fn sample_checkpoint() -> oz_core::checkpoint::LoopCheckpoint {
        oz_core::checkpoint::LoopCheckpoint {
            turn: 33,
            timestamp: 0.0,
            messages: vec![
                Message::user("[FILE:task.md] 构建后端"),
                Message::assistant_with_blocks(vec![ContentBlock::tool_use(
                    "call_1",
                    "read",
                    serde_json::json!({"file_path": "/tmp/a.py"}),
                )]),
                Message::user_with_blocks(vec![ContentBlock::tool_result(
                    "call_1",
                    "file contents",
                )]),
                Message::assistant_with_blocks(vec![ContentBlock::text("后端已完成")]),
                Message::assistant_with_blocks(vec![ContentBlock::tool_use(
                    "call_2",
                    "todoupdate",
                    serde_json::json!({"id": "t1", "status": "completed"}),
                )]),
            ],
            history_info: vec![],
            full_response: "后端已完成".into(),
            exit_reason: Some("end_turn".into()),
            session_id: Some("s1".into()),
            plan: Default::default(),
            todos: vec![],
            interventions: vec![],
            full_thinking: None,
            git_sha: None,
            git_branch: None,
            git_origin_url: None,
        }
    }

    #[test]
    fn checkpoint_messages_preserve_existing_prefix() {
        let cp = sample_checkpoint();
        let existing = vec![serde_json::json!({"role": "user", "content": "触发消息"})];
        let out = checkpoint_messages_to_store(&cp, &existing);
        assert_eq!(out[0]["role"], "user");
        assert_eq!(out[0]["content"], "触发消息");
        // existing trigger + checkpoint's first user is skipped (dedup),
        // so: 1 existing + 1 user(tool_result) + 2 assistant + 1 assistant(tool)
        assert_eq!(out.len(), 5);
    }

    /// Regression: a session whose last turn ended with tool results
    /// persists as [.., user(trigger), assistant, user(tool_results, ""),
    /// assistant]. regenerate must walk back to the trigger instead of
    /// seeding an empty user message (which made the runner bail with
    /// "No user message to process").
    #[test]
    fn regenerate_seed_skips_tool_result_carriers() {
        let mut msgs = vec![
            serde_json::json!({"role": "user", "content": "写一个脚本"}),
            serde_json::json!({"role": "assistant", "content": "正在执行"}),
            serde_json::json!({"role": "user", "content": "", "tool_results": [{"tool_use_id": "c1", "content": "ok"}]}),
            serde_json::json!({"role": "assistant", "content": "完成"}),
        ];
        assert_eq!(
            pop_regenerate_seed(&mut msgs),
            Some("写一个脚本".to_string())
        );
        // Everything after the trigger was popped.
        assert_eq!(msgs.len(), 0);
    }

    #[test]
    fn regenerate_seed_finds_last_user_with_text() {
        let mut msgs = vec![
            serde_json::json!({"role": "user", "content": "第一轮"}),
            serde_json::json!({"role": "assistant", "content": "a"}),
            serde_json::json!({"role": "user", "content": "第二轮"}),
            serde_json::json!({"role": "assistant", "content": "b"}),
        ];
        assert_eq!(pop_regenerate_seed(&mut msgs), Some("第二轮".to_string()));
        // Only the trailing assistant turn was popped; the first round stays.
        assert_eq!(msgs.len(), 2);
    }

    #[test]
    fn regenerate_seed_empty_when_no_user_text() {
        let mut msgs = vec![
            serde_json::json!({"role": "system", "content": "[Compression summary]"}),
            serde_json::json!({"role": "user", "content": "", "tool_results": []}),
        ];
        assert_eq!(pop_regenerate_seed(&mut msgs), None);
        assert!(msgs.is_empty());
    }

    #[test]
    fn checkpoint_tool_use_becomes_stream_events() {
        let cp = sample_checkpoint();
        let out = checkpoint_messages_to_store(&cp, &[]);
        // assistant with tool_use -> streamEvents with tool_input_*
        let asst = out.iter().find(|m| m["role"] == "assistant").unwrap();
        let ev = asst["streamEvents"].as_array().unwrap();
        assert!(ev.iter().any(|e| e["type"] == "tool_input_start"));
        assert!(ev.iter().any(|e| e["type"] == "tool_input_available"));
        assert!(ev.iter().any(|e| e["type"] == "tool_output_available"));
    }

    #[test]
    fn checkpoint_tool_result_becomes_user_tool_results() {
        let cp = sample_checkpoint();
        let out = checkpoint_messages_to_store(&cp, &[]);
        let user_tr = out
            .iter()
            .find(|m| m["role"] == "user" && m.get("tool_results").is_some());
        assert!(user_tr.is_some(), "tool_result user message must exist");
        let tr = user_tr.unwrap()["tool_results"].as_array().unwrap();
        assert_eq!(tr[0]["tool_use_id"], "call_1");
    }

    #[test]
    fn checkpoint_text_becomes_text_delta() {
        let cp = sample_checkpoint();
        let out = checkpoint_messages_to_store(&cp, &[]);
        let asst_text = out.iter().find(|m| {
            m["role"] == "assistant" && m["content"].as_str().unwrap_or("").contains("后端已完成")
        });
        assert!(asst_text.is_some(), "assistant text message must exist");
        let ev = asst_text.unwrap()["streamEvents"].as_array().unwrap();
        assert!(ev.iter().any(|e| e["type"] == "text_delta"));
    }

    /// End-to-end: run the full recovery path against a REAL checkpoint
    /// directory (the long-task session), verifying load_best picks the
    /// latest turn and the store is populated with renderable messages.
    #[test]
    fn recover_from_real_checkpoint_populates_store() {
        // Real checkpoint dir for the long-task session.
        let cp_dir = "/Users/macstu/Documents/apps/openzen/tests/longtask/2/openzen/checkpoints";
        if !std::path::Path::new(cp_dir).exists() {
            eprintln!("skipping: real checkpoint dir not present");
            return;
        }
        let session_id = "fe54c2c0-4150-4db3-bdf4-086543a1ab1d";
        let working_dir = "/Users/macstu/Documents/apps/openzen/tests/longtask/2";

        // load_best must pick the LATEST turn (033, turn 33), not an older
        // one with more messages.
        let loaded = oz_core::checkpoint::load_best_loop_checkpoint(
            std::path::Path::new(cp_dir),
            session_id,
        );
        assert!(loaded.is_some(), "checkpoint must load");
        let cp = loaded.unwrap();
        assert!(cp.turn >= 30, "latest turn expected, got {}", cp.turn);

        // recover into a fresh store (simulating restart).
        let mut store = oz_server::webui::sessions::SessionStore::new();
        store.create_with_id(session_id, "test");
        if let Some(e) = store.get_mut(session_id) {
            e.working_dir = Some(working_dir.to_string());
            e.messages
                .push(serde_json::json!({"role": "user", "content": "[FILE:trigger]" }));
        }
        recover_session_from_checkpoints(&mut store, session_id, working_dir);

        let entry = store.get(session_id).expect("session exists");
        // The trigger user message is preserved; checkpoint messages follow.
        assert!(
            entry.messages.len() >= 2,
            "expected rebuilt conversation, got {}",
            entry.messages.len()
        );
        assert_eq!(entry.messages[0]["role"], "user");
        // Dedup: the checkpoint's first user message (the same trigger)
        // must NOT be replayed — only one trigger bubble.
        let trigger_count = entry
            .messages
            .iter()
            .filter(|m| {
                m.get("role").and_then(|v| v.as_str()) == Some("user")
                    && m.get("content")
                        .and_then(|v| v.as_str())
                        .map(|s| s.contains("trigger"))
                        .unwrap_or(false)
            })
            .count();
        assert_eq!(
            trigger_count, 1,
            "trigger message must not be duplicated, found {trigger_count}"
        );
        // At least one assistant message with renderable streamEvents.
        let has_assistant = entry.messages.iter().any(|m| {
            m.get("role").and_then(|v| v.as_str()) == Some("assistant")
                && m.get("streamEvents")
                    .and_then(|v| v.as_array())
                    .map(|a| !a.is_empty())
                    .unwrap_or(false)
        });
        assert!(
            has_assistant,
            "expected assistant message with streamEvents"
        );
        // Todos restored from checkpoint.
        assert!(!entry.todos.is_empty(), "todos must be restored");
    }
}
