//! Tauri implementation of the subagent host.
//!
//! A child is a real OpenZen session: created in the store, given one user
//! message (the delegated prompt), run through the normal agent loop, then
//! removed. Its transcript is exported to a sidecar (P3) so the child can be
//! inspected — and continued (P5) — without parking it in `sessions.json`.
//!
//! Layout: `{data_dir}/openzen/agents/<parent>/<child>.jsonl` +
//! `<child>.meta.json`. A child created by `spawn_agent` is identified by the
//! caller-chosen id in `SubagentRequest.child_id`, so a background job can
//! cancel it before the loop has started.

use std::path::PathBuf;
use std::sync::Arc;

use async_trait::async_trait;
use oz_core::subagent::{
    depth_of, end_continue, forget, forget_label, register_child, release_slot, set_label,
    try_acquire_slot, try_begin_continue, SubagentHost, SubagentRequest, SubagentResult,
};
use tauri::{AppHandle, Emitter};

use crate::{lock_poison_guard, AppState};

/// A finished child kept warm for `send_message` (P5 Activation-lite).
pub struct CachedChild {
    pub parent: String,
    pub label: String,
    pub messages: Vec<serde_json::Value>,
    pub model: Option<String>,
    pub at: i64,
}

const CACHE_TTL_SECS: i64 = 30 * 60;
const CACHE_MAX: usize = 8;

pub struct TauriSubagentHost {
    pub app: AppHandle,
    pub state: Arc<AppState>,
    pub cache: std::sync::Mutex<std::collections::HashMap<String, CachedChild>>,
}

impl TauriSubagentHost {
    fn cache_put(&self, child: &str, entry: CachedChild) {
        if let Ok(mut m) = self.cache.lock() {
            let now = chrono::Utc::now().timestamp();
            m.retain(|_, c| now - c.at < CACHE_TTL_SECS);
            if m.len() >= CACHE_MAX {
                if let Some(oldest) = m.iter().min_by_key(|(_, c)| c.at).map(|(k, _)| k.clone()) {
                    m.remove(&oldest);
                }
            }
            m.insert(child.to_string(), entry);
        }
    }

    fn cache_take(&self, child: &str) -> Option<CachedChild> {
        self.cache.lock().ok()?.remove(child)
    }
}

/// Ids come from the frontend, so they must never be able to escape the
/// agents directory (`../`). Session ids are uuid-ish; enforce that shape.
fn valid_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 64
        && id
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
}

fn agents_root() -> PathBuf {
    crate::data_dir().join("openzen").join("agents")
}

fn sidecar_paths(parent: &str, child: &str) -> (PathBuf, PathBuf) {
    let dir = agents_root().join(parent);
    (
        dir.join(format!("{child}.jsonl")),
        dir.join(format!("{child}.meta.json")),
    )
}

fn write_atomic(path: &std::path::Path, body: &str) -> std::io::Result<()> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let tmp = path.with_extension("tmp");
    std::fs::write(&tmp, body)?;
    std::fs::rename(&tmp, path)
}

fn write_sidecar(
    parent: &str,
    child: &str,
    messages: &[serde_json::Value],
    meta: &serde_json::Value,
) -> std::io::Result<()> {
    let (jsonl, meta_path) = sidecar_paths(parent, child);
    if let Some(dir) = jsonl.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let mut body = String::new();
    for m in messages {
        body.push_str(&serde_json::to_string(m).unwrap_or_default());
        body.push('\n');
    }
    write_atomic(&jsonl, &body)?;
    write_atomic(&meta_path, &serde_json::to_string_pretty(meta)?)
}

/// Mark every sidecar still claiming `running` as interrupted. Called once at
/// startup: a process cannot survive an app restart, so such children are dead.
pub fn mark_interrupted_children(state: &AppState) {
    let root = agents_root();
    let Ok(dirs) = std::fs::read_dir(&root) else {
        return;
    };
    for dir in dirs.flatten() {
        let Ok(files) = std::fs::read_dir(dir.path()) else {
            continue;
        };
        for f in files.flatten() {
            let p = f.path();
            if p.extension().and_then(|e| e.to_str()) != Some("json") {
                continue;
            }
            let Ok(text) = std::fs::read_to_string(&p) else {
                continue;
            };
            let Ok(mut meta) = serde_json::from_str::<serde_json::Value>(&text) else {
                continue;
            };
            if meta.get("status").and_then(|s| s.as_str()) == Some("running") {
                meta["status"] = serde_json::json!("interrupted");
                let _ = std::fs::write(&p, serde_json::to_string_pretty(&meta).unwrap_or_default());
                tracing::warn!(
                    "[subagent] marked interrupted child {}",
                    meta.get("id").and_then(|v| v.as_str()).unwrap_or("?")
                );
            }
        }
    }

    // Crash/quit leaves the child row in sessions.json; the store would show
    // a permanent "↳ label" ghost. Children are the only sessions using that
    // prefix, so remove them.
    let mut removed = 0usize;
    {
        let mut store = crate::lock_poison_guard(&state.sessions);
        let ids: Vec<String> = store
            .list()
            .into_iter()
            .filter(|s| s.name.starts_with('↳'))
            .map(|s| s.id.clone())
            .collect();
        for id in &ids {
            if store.delete(id) {
                removed += 1;
            }
        }
    }
    if removed > 0 {
        tracing::warn!("[subagent] removed {removed} orphan child session row(s)");
    }
}

/// Read a child's sidecar transcript and metadata.
pub fn read_child(child_id: &str) -> Result<(serde_json::Value, Vec<serde_json::Value>), String> {
    if !valid_id(child_id) {
        return Err("invalid child id".into());
    }
    let root = agents_root();
    let dirs = std::fs::read_dir(&root).map_err(|e| format!("no agents dir: {e}"))?;
    for dir in dirs.flatten() {
        let meta_path = dir.path().join(format!("{child_id}.meta.json"));
        if !meta_path.exists() {
            continue;
        }
        let meta: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&meta_path).map_err(|e| e.to_string())?)
                .map_err(|e| e.to_string())?;
        let jsonl = dir.path().join(format!("{child_id}.jsonl"));
        let mut messages = Vec::new();
        if let Ok(text) = std::fs::read_to_string(&jsonl) {
            for line in text.lines() {
                if let Ok(v) = serde_json::from_str(line) {
                    messages.push(v);
                }
            }
        }
        return Ok((meta, messages));
    }
    Err(format!("child {child_id} not found"))
}

impl TauriSubagentHost {
    fn register_child(&self, parent: &str, child: &str) {
        let mut map = lock_poison_guard(&self.state.child_sessions);
        map.entry(parent.to_string())
            .or_default()
            .push(child.to_string());
    }

    fn unregister_child(&self, parent: &str, child: &str) {
        let mut map = lock_poison_guard(&self.state.child_sessions);
        if let Some(list) = map.get_mut(parent) {
            list.retain(|c| c != child);
            if list.is_empty() {
                map.remove(parent);
            }
        }
    }

    /// Stop a child by setting its loop's stop signal.
    pub fn cancel_child(&self, child: &str) {
        if let Some(sig) = lock_poison_guard(&self.state.stop_signals).get(child) {
            sig.store(true, std::sync::atomic::Ordering::SeqCst);
        }
    }

    async fn run_child(
        &self,
        req: &SubagentRequest,
        depth: u8,
        existing_messages: Option<Vec<serde_json::Value>>,
    ) -> Result<SubagentResult, String> {
        let child_id = req.child_id.clone();
        let (project_id, project_name, working_dir) = {
            let store = lock_poison_guard(&self.state.sessions);
            let Some(parent) = store.get(&req.parent_session) else {
                drop(store);
                release_slot(&req.parent_session);
                return Err("parent session not found".to_string());
            };
            (
                parent.project_id.clone(),
                parent.info.project_name.clone(),
                parent.working_dir.clone(),
            )
        };

        {
            let mut store = lock_poison_guard(&self.state.sessions);
            store.create_with_id_project(
                &child_id,
                &format!("↳ {}", req.label.chars().take(40).collect::<String>()),
                project_id.as_deref(),
                project_name.as_deref(),
                working_dir.as_deref(),
            );
            if let Some(entry) = store.get_mut(&child_id) {
                if let Some(mut history) = existing_messages {
                    history.push(serde_json::json!({
                        "role": "user",
                        "content": req.prompt,
                        "timestamp": chrono::Utc::now().to_rfc3339(),
                    }));
                    entry.messages = history;
                } else {
                    entry.messages.push(serde_json::json!({
                        "role": "user",
                        "content": req.prompt,
                        "timestamp": chrono::Utc::now().to_rfc3339(),
                    }));
                }
                entry.info.message_count = entry.messages.len();
            }
        }

        // Publish a running record first: if the app dies mid-run, startup
        // can mark it interrupted instead of losing the child entirely.
        {
            let meta = serde_json::json!({
                "id": child_id,
                "parent": req.parent_session,
                "label": req.label,
                "status": "running",
                "depth": depth,
                "started_at": chrono::Utc::now().timestamp(),
            });
            let (_, meta_path) = sidecar_paths(&req.parent_session, &child_id);
            if let Err(e) = write_atomic(
                &meta_path,
                &serde_json::to_string_pretty(&meta).unwrap_or_default(),
            ) {
                tracing::warn!("[subagent] running meta write failed: {e}");
            }
        }
        let _ = self.app.emit(
            "subagent://update",
            serde_json::json!({
                "event": "started",
                "child": {
                    "id": child_id,
                    "parent": req.parent_session,
                    "label": req.label,
                    "status": "running",
                    "needs_input": false,
                }
            }),
        );
        register_child(&child_id, depth);
        set_label(&child_id, &req.label);
        // Pre-register the stop signal so a job_kill / Stop that lands before
        // the loop starts still cancels this child.
        {
            let mut signals = lock_poison_guard(&self.state.stop_signals);
            signals
                .entry(child_id.clone())
                .or_insert_with(|| Arc::new(std::sync::atomic::AtomicBool::new(false)));
        }
        self.register_child(&req.parent_session, &child_id);
        let mut guard = ChildGuard {
            host: self,
            child_id: child_id.clone(),
            parent_id: req.parent_session.clone(),
            done: false,
        };

        let run = crate::runner::run_agent_for_session(
            &self.app,
            &self.state,
            &child_id,
            req.model.as_deref(),
            false,
            None,
            None,
        )
        .await;

        let (text, messages) = {
            let store = lock_poison_guard(&self.state.sessions);
            match store.get(&child_id) {
                Some(entry) => (
                    entry
                        .messages
                        .iter()
                        .rev()
                        .find(|m| m.get("role").and_then(|r| r.as_str()) == Some("assistant"))
                        .and_then(|m| m.get("content").and_then(|c| c.as_str()))
                        .unwrap_or("")
                        .to_string(),
                    entry.messages.clone(),
                ),
                None => (String::new(), Vec::new()),
            }
        };

        // Export before cleanup: this is the child's only durable record.
        let meta = serde_json::json!({
            "id": child_id,
            "parent": req.parent_session,
            "label": req.label,
            "status": if run.is_ok() && !text.trim().is_empty() { "completed" } else { "failed" },
            "depth": depth,
            "needs_input": text.contains("NEEDS_INPUT"),
            "finished_at": chrono::Utc::now().timestamp(),
        });
        if let Err(e) = write_sidecar(&req.parent_session, &child_id, &messages, &meta) {
            tracing::warn!("[subagent] sidecar write failed for {child_id}: {e}");
        }
        let structured = oz_core::subagent::take_structured(&child_id);
        self.cache_put(
            &child_id,
            CachedChild {
                parent: req.parent_session.clone(),
                label: req.label.clone(),
                messages: messages.clone(),
                model: req.model.clone(),
                at: chrono::Utc::now().timestamp(),
            },
        );
        let finished_status = if run.is_ok() && !text.trim().is_empty() {
            "completed"
        } else {
            "failed"
        };
        let _ = self.app.emit(
            "subagent://update",
            serde_json::json!({
                "event": "finished",
                "child": {
                    "id": child_id,
                    "parent": req.parent_session,
                    "label": req.label,
                    "status": finished_status,
                    "needs_input": text.contains("NEEDS_INPUT"),
                }
            }),
        );
        crate::notify_if_unfocused(
            &self.app,
            "子代理已完成",
            &format!("{} — {}", req.label, finished_status),
        );
        guard.cleanup();

        match run {
            Ok(()) if text.trim().is_empty() => Err(
                "child agent ended without a reply (model error or turn budget exhausted)".into(),
            ),
            Ok(()) => Ok(SubagentResult {
                child_session: child_id,
                status: "completed".into(),
                // The child may have called submit_result; prefer that over
                // parsing the visible reply.
                structured,
                text,
            }),
            Err(e) => Err(format!("child agent failed: {e}")),
        }
    }
}

/// RAII cleanup: delete the ephemeral row, drop depth/labels/slot, cancel any
/// job the child started. Runs on the panic path too.
struct ChildGuard<'a> {
    host: &'a TauriSubagentHost,
    child_id: String,
    parent_id: String,
    done: bool,
}

impl ChildGuard<'_> {
    fn cleanup(&mut self) {
        if self.done {
            return;
        }
        self.done = true;
        let cancelled =
            oz_core::jobs::global().cancel_owner(&self.child_id, Some("subagent disposed".into()));
        if cancelled > 0 {
            tracing::warn!(
                "[subagent] cancelled {cancelled} job(s) owned by disposed child {}",
                self.child_id
            );
        }
        {
            let mut store = lock_poison_guard(&self.host.state.sessions);
            store.delete(&self.child_id);
        }
        self.host.unregister_child(&self.parent_id, &self.child_id);
        forget(&self.child_id);
        forget_label(&self.child_id);
        release_slot(&self.parent_id);
    }
}

impl Drop for ChildGuard<'_> {
    fn drop(&mut self) {
        self.cleanup();
    }
}

/// Releases the single-flight continuation slot on every return path.
struct ContinueGuard {
    child: String,
}

impl Drop for ContinueGuard {
    fn drop(&mut self) {
        end_continue(&self.child);
    }
}

#[async_trait]
impl SubagentHost for TauriSubagentHost {
    async fn run(&self, req: SubagentRequest) -> Result<SubagentResult, String> {
        let depth = depth_of(&req.parent_session) + 1;
        if depth > oz_core::subagent::MAX_DEPTH {
            return Err(format!(
                "delegation depth limit reached (max {})",
                oz_core::subagent::MAX_DEPTH
            ));
        }
        if try_acquire_slot(&req.parent_session).is_none() {
            return Err(
                "subagent capacity reached (4 per conversation); wait for one to finish".into(),
            );
        }
        self.run_child(&req, depth, None).await
    }

    async fn continue_child(
        &self,
        child_id: &str,
        message: &str,
        parent_session: &str,
    ) -> Result<SubagentResult, String> {
        let cached = self.cache_take(child_id);
        let (meta, history, cached_model, cached_label) = match cached {
            Some(c) => {
                if c.parent != parent_session {
                    return Err("child does not belong to this conversation".into());
                }
                (
                    serde_json::json!({ "parent": c.parent, "label": c.label }),
                    c.messages,
                    c.model,
                    c.label,
                )
            }
            None => {
                let (meta, history) = read_child(child_id)?;
                if meta.get("parent").and_then(|v| v.as_str()) != Some(parent_session) {
                    return Err("child does not belong to this conversation".into());
                }
                let label = meta
                    .get("label")
                    .and_then(|v| v.as_str())
                    .unwrap_or("continued child")
                    .to_string();
                (meta, history, None, label)
            }
        };
        let _ = meta;
        let label = cached_label;
        let depth = depth_of(parent_session) + 1;
        if depth > oz_core::subagent::MAX_DEPTH {
            return Err("delegation depth limit reached".into());
        }
        if try_acquire_slot(parent_session).is_none() {
            return Err("subagent capacity reached; wait for one to finish".into());
        }
        if !try_begin_continue(child_id) {
            release_slot(parent_session);
            return Err("this child is already continuing; wait for it to finish".into());
        }
        let _continue_guard = ContinueGuard {
            child: child_id.to_string(),
        };
        let req = SubagentRequest {
            child_id: child_id.to_string(),
            parent_session: parent_session.to_string(),
            label,
            prompt: message.to_string(),
            model: cached_model,
            max_turns: None,
        };
        self.run_child(&req, depth, Some(history)).await
    }

    fn cancel(&self, child_session: &str) {
        self.cancel_child(child_session);
    }
}

/// Path where a child's transcript lives (for `agent_read`).
pub fn transcript_path(child_id: &str) -> Option<PathBuf> {
    let (meta, _) = read_child(child_id).ok()?;
    let parent = meta.get("parent")?.as_str()?;
    let (jsonl, _) = sidecar_paths(parent, child_id);
    Some(jsonl)
}

/// `agent_list` helper: all children of one conversation, newest first.
pub fn list_children(parent: &str) -> Vec<serde_json::Value> {
    if !valid_id(parent) {
        return Vec::new();
    }
    let dir = agents_root().join(parent);
    let mut out = Vec::new();
    if let Ok(files) = std::fs::read_dir(&dir) {
        for f in files.flatten() {
            let p = f.path();
            if p.extension().and_then(|e| e.to_str()) != Some("json") {
                continue;
            }
            if let Ok(text) = std::fs::read_to_string(&p) {
                if let Ok(v) = serde_json::from_str::<serde_json::Value>(&text) {
                    out.push(v);
                }
            }
        }
    }
    out.sort_by_key(|v| {
        std::cmp::Reverse(v.get("finished_at").and_then(|x| x.as_i64()).unwrap_or(0))
    });
    out
}

/// `agent_read` helper: the child's JSONL transcript as text.
pub fn read_transcript(child_id: &str) -> Result<String, String> {
    let p = transcript_path(child_id).ok_or_else(|| format!("child {child_id} not found"))?;
    std::fs::read_to_string(&p).map_err(|e| e.to_string())
}
