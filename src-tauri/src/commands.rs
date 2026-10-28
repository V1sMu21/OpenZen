//! Tauri IPC command handlers for the OpenZen desktop app.

use std::collections::HashMap;
use std::path::Path;
use std::sync::Arc;

use oz_config::mykey::{MyKeyConfig, SessionType};
use oz_core_types::{LlmClient, Message};
use oz_server::webui::sessions::{SessionInfo, SessionStatus, SessionStore};
use oz_server::webui::sse_bus::SseEvent;
use tauri::{AppHandle, Emitter, Manager, State};

use crate::{
    data_dir, debug_log, debug_log_flush, lock_poison_guard, runner, AppState, ModelEntry,
    ProviderEntry, SendMessageResponse,
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

// ---------------------------------------------------------------------------
// Foreign session import (ZCode / DeepSeek Harness)
// ---------------------------------------------------------------------------

/// Where the import ledger lives: `<data root>/openzen/imported_sessions.json`.
fn import_ledger_path() -> std::path::PathBuf {
    oz_import::ImportLedger::default_path(&data_dir())
}

/// `{ sources: [{id,label,available,detail,session_count,error}] }`.
///
/// Never fails: a source that cannot be read is reported with
/// `available: false` plus its error so the dialog can still show the other.
#[tauri::command]
pub async fn scan_import_sources() -> Result<serde_json::Value, String> {
    let sources = tokio::task::spawn_blocking(|| {
        let paths = oz_import::SourcePaths::default();
        oz_import::scan_sources(&paths)
    })
    .await
    .map_err(|e| e.to_string())?;
    Ok(serde_json::json!({ "sources": sources }))
}

/// `{ source, error, sessions: [{source_id,title,directory,created_at,
/// message_count,already_imported}] }`.
///
/// A discovery failure is returned in `error` (not as an IPC rejection) so the
/// dialog can render it inline without losing the source tabs.
#[tauri::command]
pub async fn import_list_sessions(source: String) -> Result<serde_json::Value, String> {
    let requested = source.clone();
    let result = tokio::task::spawn_blocking(move || {
        let paths = oz_import::SourcePaths::default();
        let src = oz_import::parse_source(&source)?;
        oz_import::list_sessions(src, &paths).map(|sessions| (src, sessions))
    })
    .await
    .map_err(|e| e.to_string())?;

    match result {
        Ok((src, mut sessions)) => {
            // Mark rows already pulled in by an earlier import.
            let ledger = oz_import::ImportLedger::load(import_ledger_path());
            for s in sessions.iter_mut() {
                s.already_imported = ledger.is_imported(src, &s.source_id);
            }
            Ok(serde_json::json!({
                "source": src.id(),
                "error": serde_json::Value::Null,
                "sessions": sessions,
            }))
        }
        Err(err) => Ok(serde_json::json!({
            "source": requested,
            "error": err.to_string(),
            "sessions": [],
        })),
    }
}

/// Import the requested source sessions as new OpenZen sessions.
///
/// Returns `{ imported: [{source_id,session_id,message_count}],
/// errors: [{source_id,error}] }`. Each session is parsed off-thread first, so
/// a single unreadable log does not abort the whole batch; the session store is
/// only locked once the whole batch is converted.
#[tauri::command]
pub async fn import_sessions(
    source: String,
    ids: Vec<String>,
    state: State<'_, Arc<AppState>>,
) -> Result<serde_json::Value, String> {
    let state = state.inner().clone();

    if ids.is_empty() {
        return Ok(serde_json::json!({ "imported": [], "errors": [] }));
    }

    let parsed = tokio::task::spawn_blocking(move || {
        let paths = oz_import::SourcePaths::default();
        let src = oz_import::parse_source(&source)?;
        let mut ok = Vec::new();
        let mut errs = Vec::new();
        for id in ids {
            match oz_import::read_session(src, &id, &paths) {
                // A session with no conversational messages would create an
                // empty OpenZen session; report it instead of importing it.
                Ok(session) if session.messages.is_empty() => {
                    errs.push(serde_json::json!({
                        "source_id": id,
                        "error": "session contains no messages",
                    }));
                }
                Ok(session) => ok.push(session),
                Err(e) => errs.push(serde_json::json!({
                    "source_id": id,
                    "error": e.to_string(),
                })),
            }
        }
        Ok::<_, oz_import::ImportError>((src, ok, errs))
    })
    .await
    .map_err(|e| e.to_string())?;

    let (src, sessions, errors) = match parsed {
        Ok(v) => v,
        Err(e) => {
            return Ok(serde_json::json!({
                "imported": [],
                "errors": [{ "source_id": "", "error": e.to_string() }],
            }))
        }
    };

    let mut imported = Vec::new();
    let mut ledger = oz_import::ImportLedger::load(import_ledger_path());
    {
        let mut store = lock_poison_guard(&state.sessions);
        for session in sessions {
            let working_dir = session
                .directory
                .clone()
                .unwrap_or_else(|| state.working_dir.clone());
            let info = store.create_with_project(&session.title, None, None, Some(&working_dir));
            let message_count = session.messages.len();
            if let Some(entry) = store.get_mut(&info.id) {
                entry.messages = session.messages;
                entry.info.message_count = message_count;
                // Preserve the original timeline so imported history sorts
                // where it actually happened.
                if let Some(created) = session.created_at {
                    entry.created_at = created;
                    entry.info.created_at = created.to_rfc3339();
                }
            }
            ledger.record(
                src,
                &session.source_id,
                &info.id,
                &session.title,
                message_count,
            );
            imported.push(serde_json::json!({
                "source_id": session.source_id,
                "session_id": info.id,
                "message_count": message_count,
            }));
        }
        // create_with_project persists each new entry, but the message bodies
        // are attached afterwards; bump the fingerprint so the full snapshot
        // lands on disk.
        store.save();
    }
    if let Err(e) = ledger.save() {
        debug_log(&format!("[openzen] import ledger save failed: {e}"));
    }

    Ok(serde_json::json!({ "imported": imported, "errors": errors }))
}

/// Bundle the evidence a bug report needs into one directory.
///
/// Returns the directory path; the UI offers to reveal it. Deliberately
/// copies only logs, version info and a REDACTED config summary — never
/// session content or API keys.
#[tauri::command]
pub fn export_diagnostics(state: State<'_, Arc<AppState>>) -> serde_json::Value {
    let dir = crate::data_dir().join("logs");
    let stamp = chrono::Local::now().format("%Y%m%d-%H%M%S");
    let out = crate::data_dir().join(format!("diagnostics-{stamp}"));
    if let Err(e) = std::fs::create_dir_all(&out) {
        return serde_json::json!({ "error": format!("create dir: {e}") });
    }
    let mut copied: Vec<String> = Vec::new();
    if let Ok(entries) = std::fs::read_dir(&dir) {
        for entry in entries.flatten() {
            let name = entry.file_name().to_string_lossy().to_string();
            let interesting = name.starts_with("openzen.log")
                || name.starts_with("crash-")
                || name.starts_with("frontend-errors")
                || name == "audit.jsonl";
            if !interesting {
                continue;
            }
            // Cap each file so a 20 MB log does not make a 20 MB bundle.
            let data = std::fs::read(entry.path()).unwrap_or_default();
            let slice = if data.len() > 4 * 1024 * 1024 {
                &data[data.len() - 4 * 1024 * 1024..]
            } else {
                &data[..]
            };
            if std::fs::write(out.join(&name), slice).is_ok() {
                copied.push(name);
            }
        }
    }
    // Redacted environment summary: shape of the config, never its values.
    let cfg_path = crate::resolve_config_path(std::path::Path::new(&state.config_path));
    let cfg_summary = match MyKeyConfig::from_file(&cfg_path) {
        Ok(cfg) => serde_json::json!({
            "config_path": cfg_path.to_string_lossy(),
            "memory_backend": cfg.memory_backend,
            "session_count": cfg.sessions.len(),
            "model_names": cfg.sessions.keys().cloned().collect::<Vec<_>>(),
        }),
        Err(e) => {
            serde_json::json!({ "config_path": cfg_path.to_string_lossy(), "error": e.to_string() })
        }
    };
    let summary = serde_json::json!({
        "exported_at": chrono::Utc::now().to_rfc3339(),
        "version": env!("CARGO_PKG_VERSION"),
        "os": std::env::consts::OS,
        "arch": std::env::consts::ARCH,
        "data_dir": crate::data_dir().to_string_lossy(),
        "working_dir": state.working_dir,
        "memory_enabled": state.memory_enabled.load(std::sync::atomic::Ordering::SeqCst),
        "config": cfg_summary,
        "log_files": copied,
        "note": "Session content and API keys are NOT included.",
    });
    let _ = std::fs::write(
        out.join("summary.json"),
        serde_json::to_string_pretty(&summary).unwrap_or_default(),
    );
    serde_json::json!({
        "path": out.to_string_lossy(),
        "files": copied,
    })
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
            tracing::debug!("ping: config error: {e}");
            vec![]
        }
    };
    // Demoted to debug: the UI pings every 30s, and these lines were 53–66%
    // of the whole log, burying the events that matter (round5 P1-10 ③).
    tracing::debug!(
        "ping: {} models, config_path={}",
        models.len(),
        cfg_path.display()
    );
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

/// Read a computer-use screenshot back as a data URI for the ToolCallCard
/// preview. Path-restricted: only files under `{working_dir}/computer/` are
/// served (the caller passes the session's working dir — the same root the
/// capture tool wrote to), so this cannot become an arbitrary file read.
/// Async so the multi-hundred-KB read+encode runs off the main thread.
#[tauri::command]
pub async fn computer_screenshot_data(path: String, working_dir: String) -> serde_json::Value {
    let result = tokio::task::spawn_blocking(move || {
        let allowed_root = std::path::Path::new(&working_dir)
            .join(oz_tools::computer_use::SCREENSHOT_DIR)
            .canonicalize()
            .ok();
        let Some(allowed_root) = allowed_root else {
            return Err("no screenshots yet".to_string());
        };
        let canonical = std::fs::canonicalize(&path).map_err(|e| e.to_string())?;
        if !canonical.starts_with(&allowed_root) {
            return Err("path outside the screenshot directory".to_string());
        }
        oz_tools::doc_reader::read_image_base64(&canonical.to_string_lossy())
    })
    .await;
    match result {
        Ok(Ok(data_uri)) => serde_json::json!({ "data_uri": data_uri }),
        Ok(Err(e)) => serde_json::json!({ "error": e }),
        Err(e) => serde_json::json!({ "error": e.to_string() }),
    }
}

/// Frontend console bridge: persists a line to the debug log so release
/// builds (no devtools) stay diagnosable. Cheap; used by startup paths and
/// the heartbeat check. Flushes per line — this path is cold and the whole
/// point is to see the line even if the app dies right after.
///
/// `level` is used by the top-level `window.onerror` /
/// `unhandledrejection` handlers: the release webview has devtools disabled,
/// so without this channel a white screen left ZERO evidence behind
/// (round5 P1-10 ②). `error` lines are additionally appended to
/// `logs/frontend-errors.log` so they are not rotated away by heartbeat noise.
#[tauri::command]
pub fn log_frontend(
    line: Option<String>,
    level: Option<String>,
    message: Option<String>,
    stack: Option<String>,
) {
    let level = level.unwrap_or_else(|| "info".into());
    // Two call shapes reach this command: the heartbeat sends `{line}`, the
    // global error handlers send `{level, message, stack}`.
    let composed = match (message, line) {
        (Some(m), Some(l)) => format!("{l} {m}"),
        (Some(m), None) => m,
        (None, Some(l)) => l,
        (None, None) => String::new(),
    };
    let composed = match stack {
        Some(st) if !st.trim().is_empty() => {
            format!("{composed}\n{}", st.chars().take(2000).collect::<String>())
        }
        _ => composed,
    };
    let clipped: String = composed.trim_end().chars().take(4000).collect();
    if clipped.is_empty() {
        return;
    }
    match level.as_str() {
        "warn" => {
            tracing::warn!("[web] {clipped}");
            debug_log(&format!("[web] WARN {clipped}"));
        }
        "error" => {
            tracing::error!("[web] {clipped}");
            debug_log(&format!("[web] ERROR {clipped}"));
            let dir = crate::data_dir().join("logs");
            let _ = std::fs::create_dir_all(&dir);
            if let Ok(mut f) = std::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(dir.join("frontend-errors.log"))
            {
                use std::io::Write as _;
                let _ = writeln!(f, "{} [error] {clipped}", chrono::Local::now().to_rfc3339());
            }
        }
        // Heartbeat/ping traffic is the bulk of the log (measured 53–66% of
        // all lines) and buries real events. `debug` stays out of the file
        // unless the user explicitly asks for debug logging.
        "debug" => tracing::debug!("[web] {clipped}"),
        _ => debug_log(&format!("[web] {clipped}")),
    }
    if level != "debug" {
        debug_log_flush();
    }
}

/// Reveal a path in the OS file manager (Finder on macOS).
///
/// The frontend has no opener/shell plugin dependency, so it had to degrade
/// to "copy the path and press Cmd-Shift-G" — this makes the export/backup
/// buttons actually useful (round5 P1-12 ①).
#[tauri::command]
pub fn reveal_path(path: String) -> serde_json::Value {
    let p = std::path::PathBuf::from(&path);
    if !p.exists() {
        return serde_json::json!({ "ok": false, "error": "path does not exist" });
    }
    #[cfg(target_os = "macos")]
    let result = std::process::Command::new("open")
        .arg("-R")
        .arg(&p)
        .status();
    #[cfg(target_os = "windows")]
    let result = std::process::Command::new("explorer")
        .arg("/select,")
        .arg(&p)
        .status();
    #[cfg(all(unix, not(target_os = "macos")))]
    let result = std::process::Command::new("xdg-open")
        .arg(p.parent().unwrap_or(&p))
        .status();
    match result {
        Ok(st) if st.success() => serde_json::json!({ "ok": true, "path": path }),
        Ok(st) => serde_json::json!({ "ok": false, "error": format!("exit {st}") }),
        Err(e) => serde_json::json!({ "ok": false, "error": e.to_string() }),
    }
}

/// Structured user feedback on one assistant message (round5 P2-28).
///
/// A correction in chat only ever became conversation text; this records a
/// quality event so the learning loop can act on it (and so a 👎 is not
/// silently lost).
#[tauri::command]
pub fn submit_message_feedback(
    session_id: String,
    message_index: i64,
    verdict: String,
    note: Option<String>,
    state: State<'_, Arc<AppState>>,
) -> serde_json::Value {
    let verdict = verdict.trim().to_lowercase();
    if verdict != "up" && verdict != "down" {
        return serde_json::json!({ "error": "verdict must be \"up\" or \"down\"" });
    }
    let working_dir = lock_poison_guard(&state.sessions)
        .get(&session_id)
        .and_then(|e| e.working_dir.clone())
        .unwrap_or_else(|| state.working_dir.clone());
    let note = note.unwrap_or_default();
    let note: String = note.trim().chars().take(500).collect();
    let summary = if note.is_empty() {
        format!("message_feedback verdict={verdict} index={message_index}")
    } else {
        format!("message_feedback verdict={verdict} index={message_index} note={note}")
    };
    oz_core::quality::log_quality_event(&working_dir, "user_feedback", &summary);
    // A 👎 is also a delivery-quality signal: record it where the quality
    // report aggregates from, so "how often is the user unhappy" is visible.
    if verdict == "down" {
        oz_core::quality::log_reflection(&working_dir, "user_rejected", &summary);
    }
    tracing::info!("[feedback] session={session_id} {summary}");
    serde_json::json!({ "ok": true, "recorded": summary })
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
                        provider_id: sess.provider.clone(),
                        context_win: sess.context_win,
                        modalities: normalize_modalities(sess.modalities.as_deref()),
                        is_local,
                        // Compare the explicit field only: the
                        // default_session_name() fallback pick walks a
                        // HashMap and is nondeterministic between reloads.
                        is_default: cfg.default_session.as_deref() == Some(name.as_str()),
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

/// Read-only memory status for the SoulCard UI: ERME soul state, semantic
/// store counters and the harness ledger Memory-entry count. Never blocks on
/// agent loops; `enabled: false` when the file backend is active.
#[tauri::command]
pub fn get_memory_status(state: State<'_, Arc<AppState>>) -> serde_json::Value {
    let Some(runtime) = state.erme() else {
        return serde_json::json!({
            "enabled": false,
            "backend": "file",
            "erne_available": false,
            "harness": { "entry_count": 0 },
        });
    };

    let stats = runtime.store.stats();
    let counters = runtime.store.counters().snapshot();
    let recall_hit_rate = if counters.recalls > 0 {
        counters.recall_hits as f64 / counters.recalls as f64
    } else {
        0.0
    };
    let soul = {
        let handle = runtime.injector.soul();
        let model = handle.read().unwrap_or_else(|e| e.into_inner());
        // `mood`/`energy`/`confidence` used to be the constants from
        // `SoulState::new()` (nothing ever called `update`), and the UI
        // rendered them as the assistant's feelings. They are now driven by
        // real interaction, but they are still a coarse heuristic — so ship
        // the facts that ARE exact (counts, timestamps) alongside them and
        // let the UI label the rest honestly (round5 P0-5 (3)).
        serde_json::json!({
            "identity": model.core.identity,
            "mood": model.state.mood,
            "energy": model.state.energy,
            "confidence": model.state.confidence,
            "state_updated_at": model.state.last_updated_nanos,
            "portrait_facts": model.user_portrait.facts.len(),
            "self_portrait_facts": model.self_portrait.facts.len(),
            "user_traits": model.user_portrait.traits.len(),
            "portrait_updated_at": model.user_portrait.last_updated_nanos,
            "self_portrait_updated_at": model.self_portrait.last_updated_nanos,
            "narrative_chapters": model.narrative.chapters.len(),
            "version": model.version,
            "heuristic": true,
        })
    };
    // Mirror what the distiller ingests (Memory kind only), so the UI number
    // matches what is actually recallable as semantic memory.
    let harness_entry_count = oz_core::harness::HarnessState::load(&crate::harness_dir())
        .entries_of(oz_core::harness::HarnessKind::Memory)
        .len();

    // The live flag, not a hardcoded true: the switch used to bounce back to
    // "on" immediately after being turned off (review A11).
    let enabled = state
        .memory_enabled
        .load(std::sync::atomic::Ordering::SeqCst);
    serde_json::json!({
        "enabled": enabled,
        "backend": if enabled { "erme" } else { "file" },
        "erne_available": true,
        "embedding_kind": runtime.store.router().l2_engine().embedding_kind(),
        "soul": soul,
        "store": {
            "total_entries": stats.total_entries,
            "l1_entries": stats.l1_entries,
            "l2_entries": stats.l2_entries,
            "l3_entries": stats.l3_entries,
            "l3_storage_bytes": stats.l3_storage_bytes,
            // Index-health signals: hnsw index growth and the last
            // consolidation time (both computed but previously invisible).
            "hnsw_entries": stats.hnsw_entries,
            "last_consolidation": stats.last_consolidation,
            "l3_tokens_used_today": stats.l3_tokens_used_today,
            "stores": counters.stores,
            "recalls": counters.recalls,
            "recall_hits": counters.recall_hits,
            "recall_misses": counters.recall_misses,
            "consolidations": counters.consolidations,
            "recall_hit_rate": recall_hit_rate,
        },
        "harness": { "entry_count": harness_entry_count },
    })
}

/// Toggle the memory backend at runtime AND persist the choice.
///
/// Before this, the only way to turn memory off was to edit `mykey.toml` and
/// restart — for a 7x24 companion that is not a switch (round5 P1-12 ①).
/// The in-process flag takes effect on the next message; the config write
/// makes it survive a restart.
#[tauri::command]
pub fn set_memory_enabled(enabled: bool, state: State<'_, Arc<AppState>>) -> serde_json::Value {
    state
        .memory_enabled
        .store(enabled, std::sync::atomic::Ordering::SeqCst);
    let backend = if enabled { "erme" } else { "file" };
    let path = crate::resolve_config_path(std::path::Path::new(&state.config_path));
    let write_result = set_toml_string_key(&path, "memory_backend", backend);
    // Read back: "the write returned Ok" is not evidence the key landed at the
    // top level (review A11).
    let verified = oz_config::mykey::MyKeyConfig::from_file(&path)
        .map(|c| c.memory_backend == backend)
        .unwrap_or(false);
    if write_result.is_ok() && !verified {
        return serde_json::json!({
            "ok": false,
            "backend": backend,
            "config_path": path.to_string_lossy(),
            "error": "config write did not take effect (key not at top level); a .bak was kept",
        });
    }
    serde_json::json!({
        "ok": write_result.is_ok(),
        "backend": backend,
        "persisted": verified,
        "config_path": path.to_string_lossy(),
        "error": write_result.err(),
    })
}

/// Set a top-level `key = "value"` in a TOML file, preserving everything
/// else. Backs the file up first; no-ops when the value is already right.
fn set_toml_string_key(path: &std::path::Path, key: &str, value: &str) -> Result<(), String> {
    let text = std::fs::read_to_string(path).map_err(|e| format!("read config: {e}"))?;
    let line = format!("{key} = \"{value}\"");
    let mut out = String::with_capacity(text.len() + line.len() + 1);
    let mut replaced = false;
    for l in text.lines() {
        let trimmed = l.trim_start();
        let is_key = trimmed.starts_with(&format!("{key} "))
            || trimmed.starts_with(&format!("{key}="))
            || trimmed.starts_with(&format!("{key}\t"));
        if is_key && !trimmed.starts_with('#') {
            out.push_str(&line);
            replaced = true;
        } else {
            out.push_str(l);
        }
        out.push('\n');
    }
    if !replaced {
        // Append BEFORE the first `[table]` header: appending at EOF puts a
        // top-level key inside whatever table happens to be last (the real
        // mykey.toml ends with `[web_search]`, so `memory_backend` became
        // `web_search.memory_backend` and was silently ignored — review A11).
        let insert_at = out
            .lines()
            .position(|l| l.trim_start().starts_with('['))
            .map(|idx| out.lines().take(idx).map(|l| l.len() + 1).sum::<usize>())
            .unwrap_or(out.len());
        out.insert_str(insert_at, &format!("{line}\n"));
    }
    let backup = path.with_extension("toml.bak");
    let _ = std::fs::copy(path, &backup);
    let tmp = path.with_extension("toml.tmp");
    std::fs::write(&tmp, out).map_err(|e| format!("write config: {e}"))?;
    std::fs::rename(&tmp, path).map_err(|e| format!("commit config: {e}"))?;
    Ok(())
}

/// Export every memory as one JSON document under the data root.
#[tauri::command]
pub fn export_memory(state: State<'_, Arc<AppState>>) -> serde_json::Value {
    let Some(runtime) = state.erme() else {
        return serde_json::json!({ "error": "memory backend not enabled" });
    };
    let entries = collect_all_memories(&runtime.store, None);
    let dir = crate::data_dir().join("openzen");
    let _ = std::fs::create_dir_all(&dir);
    let stamp = chrono::Local::now().format("%Y%m%d-%H%M%S");
    let path = dir.join(format!("memory_export_{stamp}.json"));
    let doc = serde_json::json!({
        "exported_at": chrono::Utc::now().to_rfc3339(),
        "count": entries.len(),
        "entries": entries,
    });
    match serde_json::to_string_pretty(&doc)
        .map_err(|e| e.to_string())
        .and_then(|json| std::fs::write(&path, json).map_err(|e| e.to_string()))
    {
        Ok(()) => {
            let bytes = std::fs::metadata(&path).map(|m| m.len()).unwrap_or(0);
            serde_json::json!({
                "path": path.to_string_lossy(),
                "bytes": bytes,
                "entries": entries.len(),
            })
        }
        Err(e) => serde_json::json!({ "error": e }),
    }
}

/// Every non-superseded memory in the store, newest/highest-importance first.
fn collect_all_memories(
    store: &entropy_memory_engine::memory_store::MemoryStore,
    limit: Option<usize>,
) -> Vec<serde_json::Value> {
    let mut memories = store.router().l3_engine().storage().all();
    memories.retain(|m| m.metadata.superseded_by.is_none());
    memories.sort_by_key(|m| std::cmp::Reverse(m.metadata.created_at));
    if let Some(n) = limit {
        memories.truncate(n);
    }
    memories
        .into_iter()
        .map(|m| {
            serde_json::json!({
                "id": m.id,
                "content": m.content_text(),
                "importance": m.metadata.importance,
                "tags": m.tags,
                "created_at": m.metadata.created_at,
                "alias": m.alias,
            })
        })
        .collect()
}

/// Browse memories (newest first) for the memory browser UI.
#[tauri::command]
pub fn list_memories(
    limit: Option<usize>,
    query: Option<String>,
    state: State<'_, Arc<AppState>>,
) -> serde_json::Value {
    let Some(runtime) = state.erme() else {
        return serde_json::json!({ "entries": [], "enabled": false });
    };
    let needle = query.unwrap_or_default().trim().to_lowercase();
    let mut entries = collect_all_memories(&runtime.store, None);
    if !needle.is_empty() {
        entries.retain(|e| {
            e.get("content")
                .and_then(|c| c.as_str())
                .map(|c| c.to_lowercase().contains(&needle))
                .unwrap_or(false)
        });
    }
    let total = entries.len();
    entries.truncate(limit.unwrap_or(200).clamp(1, 1000));
    serde_json::json!({ "entries": entries, "total": total, "enabled": true })
}

/// Forget one memory. Also marks any portrait fact that quoted it as
/// contradicted, so the next reflection pass does not re-derive it.
#[tauri::command]
pub fn forget_memory(id: u64, state: State<'_, Arc<AppState>>) -> serde_json::Value {
    let Some(runtime) = state.erme() else {
        return serde_json::json!({ "error": "memory backend not enabled" });
    };
    let content = runtime
        .store
        .router()
        .l3_engine()
        .get_by_id(id)
        .map(|m| m.content_text())
        .unwrap_or_default();
    let ok = runtime.store.forget(id);
    if ok && !content.is_empty() {
        // Deleting the memory alone is not enough: the distilled fact lives
        // on in the portrait (and would be re-extracted on the next pass).
        let handle = runtime.injector.soul();
        let mut guard = handle.write().unwrap_or_else(|e| e.into_inner());
        {
            let model = &mut *guard;
            let needle: String = content.chars().take(24).collect();
            let hits: Vec<String> = model
                .user_portrait
                .facts
                .iter()
                .filter(|f| {
                    !needle.is_empty()
                        && entropy_memory_engine::l0::portrait::lexical_compatibility(
                            &f.statement,
                            &content,
                        ) >= 0.5
                })
                .map(|f| f.statement.clone())
                .collect();
            for stmt in &hits {
                model.user_portrait.remove_fact(stmt);
            }
            if !hits.is_empty() {
                model.bump_version();
                tracing::info!(
                    "[memory] forget {id}: also removed {} portrait fact(s)",
                    hits.len()
                );
            }
        }
    }
    serde_json::json!({ "ok": ok, "id": id })
}

/// Clear every memory. Always writes a backup first and requires an explicit
/// confirmation flag, because this is irreversible from the UI.
#[tauri::command]
pub fn clear_memory(confirm: bool, state: State<'_, Arc<AppState>>) -> serde_json::Value {
    if !confirm {
        return serde_json::json!({ "error": "confirmation required" });
    }
    let Some(runtime) = state.erme() else {
        return serde_json::json!({ "error": "memory backend not enabled" });
    };
    let entries = collect_all_memories(&runtime.store, None);
    let dir = crate::data_dir().join("openzen");
    let _ = std::fs::create_dir_all(&dir);
    let stamp = chrono::Local::now().format("%Y%m%d-%H%M%S");
    let backup = dir.join(format!("memory_backup_{stamp}.json"));
    let doc = serde_json::json!({
        "backed_up_at": chrono::Utc::now().to_rfc3339(),
        "count": entries.len(),
        "entries": entries,
    });
    let backup_path = match serde_json::to_string_pretty(&doc)
        .map_err(|e| e.to_string())
        .and_then(|json| std::fs::write(&backup, json).map_err(|e| e.to_string()))
    {
        Ok(()) => backup.to_string_lossy().to_string(),
        Err(e) => return serde_json::json!({ "error": format!("backup failed: {e}") }),
    };
    let mut removed = 0usize;
    for id in collect_all_memories(&runtime.store, None)
        .iter()
        .filter_map(|e| e.get("id").and_then(|v| v.as_u64()))
    {
        if runtime.store.forget(id) {
            removed += 1;
        }
    }
    // Portrait facts came from those memories; keeping them would resurrect
    // the "memory" the user just cleared.
    //
    // …and the cleared model must reach DISK: this used to only touch memory,
    // so a restart re-read soul.json (185 facts) and the "cleared" memories
    // came back. `remove_portrait_fact` already saved; this path did not
    // (review A4).
    let persist_error = match runtime.injector.soul().write() {
        Ok(mut model) => {
            model.user_portrait.facts.clear();
            model.user_portrait.traits.clear();
            model.bump_version();
            let path = crate::data_dir().join("memory_erme").join("soul.json");
            match model.save_atomic(&path) {
                Ok(()) => None,
                Err(e) => Some(format!("soul.json save failed: {e}")),
            }
        }
        Err(e) => Some(format!("soul lock poisoned: {e}")),
    };
    if let Some(ref err) = persist_error {
        tracing::error!("[memory] clear_memory could not persist the soul: {err}");
    }
    tracing::warn!("[memory] cleared {removed} entries; backup at {backup_path}");
    serde_json::json!({
        "ok": persist_error.is_none(),
        "backup_path": backup_path,
        "removed": removed,
        "error": persist_error,
    })
}

/// Rename the agent: writes the user-given name into the L0 soul identity
/// (the same field `get_memory_status` exposes as `soul.identity`). The
/// write goes through the shared soul handle so the prompt injector and
/// reflection engine see it immediately, then the model is atomically
/// persisted to {data_dir}/memory_erme/soul.json so it survives restarts.
#[tauri::command]
pub fn set_soul_identity(name: String, state: State<'_, Arc<AppState>>) -> serde_json::Value {
    let Some(runtime) = state.erme() else {
        return serde_json::json!({ "error": "memory backend not enabled" });
    };
    let trimmed = name.trim();
    if trimmed.is_empty() || trimmed.chars().count() > 24 {
        return serde_json::json!({ "error": "name must be 1-24 characters" });
    }
    let handle = runtime.injector.soul();
    {
        let mut model = handle.write().unwrap_or_else(|e| e.into_inner());
        model.core.identity = trimmed.to_string();
        model.bump_version();
    }
    // The reflection engine persists soul.json on its own idle cycle; an
    // explicit save here covers app exit before that cycle runs.
    let soul_path = data_dir().join("memory_erme").join("soul.json");
    let persist_error = {
        let model = handle.read().unwrap_or_else(|e| e.into_inner());
        model.save_atomic(&soul_path).err().map(|e| e.to_string())
    };
    serde_json::json!({
        "status": "ok",
        "identity": handle.read().unwrap_or_else(|e| e.into_inner()).core.identity,
        "persist_error": persist_error,
    })
}

/// P2-18: the portrait correction loop. The user must be able to SEE what
/// the agent believes about them and DELETE a wrong fact — otherwise more
/// memory means "confidently misunderstanding you". Returns statement +
/// confidence for every portrait fact.
#[tauri::command]
pub fn get_soul_portrait(state: State<'_, Arc<AppState>>) -> serde_json::Value {
    let Some(runtime) = state.erme() else {
        return serde_json::json!({ "enabled": false, "facts": [] });
    };
    let handle = runtime.injector.soul();
    let model = handle.read().unwrap_or_else(|e| e.into_inner());
    let facts: Vec<serde_json::Value> = model
        .user_portrait
        .facts
        .iter()
        .map(|f| {
            serde_json::json!({
                "statement": f.statement,
                "confidence": f.confidence,
            })
        })
        .collect();
    serde_json::json!({ "enabled": true, "facts": facts })
}

/// Remove one portrait fact by statement (exact match as listed by
/// get_soul_portrait). Persisted immediately so the correction survives
/// a restart even if the idle cycle never runs.
#[tauri::command]
pub fn remove_portrait_fact(
    statement: String,
    state: State<'_, Arc<AppState>>,
) -> serde_json::Value {
    let Some(runtime) = state.erme() else {
        return serde_json::json!({ "error": "memory backend not enabled" });
    };
    let handle = runtime.injector.soul();
    let removed = {
        let mut model = handle.write().unwrap_or_else(|e| e.into_inner());
        let removed = model.user_portrait.remove_fact(&statement);
        if removed {
            model.bump_version();
        }
        removed
    };
    if !removed {
        return serde_json::json!({ "error": "no matching portrait fact" });
    }
    let soul_path = data_dir().join("memory_erme").join("soul.json");
    let persist_error = {
        let model = handle.read().unwrap_or_else(|e| e.into_inner());
        model.save_atomic(&soul_path).err().map(|e| e.to_string())
    };
    tracing::info!("[soul] removed portrait fact by user request: {statement}");
    serde_json::json!({ "status": "ok", "persist_error": persist_error })
}

/// ── Settings panel (docs/settings-panel-plan.md) ──────────────────────────
///
/// Model management writes back `mykey.toml` preserving unknown keys: the
/// raw TOML table is parsed, only the target session entry (or
/// `default_session`) is mutated, then the table is serialized back.
/// Encrypted configs (`mykey.toml.enc`) are re-encrypted after mutation;
/// plaintext stays plaintext.
fn write_mykey_toml<F>(config_path: &str, mutate: F) -> Result<(), String>
where
    F: FnOnce(&mut toml::Table) -> Result<(), String>,
{
    // Tauri commands run on a thread pool: gate RMW cycles on the same lock
    // the platform config writer (add_platform) uses, so concurrent writers
    // of mykey.toml cannot interleave and lose updates.
    let _gate = lock_poison_guard(&CONFIG_WRITE_LOCK);
    let path = std::path::Path::new(config_path);
    let enc_path = path.with_extension("toml.enc");
    let content = oz_config::crypto::read_config(path).map_err(|e| e.to_string())?;
    let mut table: toml::Table = content
        .parse()
        .map_err(|e| format!("TOML parse error: {e}"))?;
    mutate(&mut table)?;
    let out = toml::to_string_pretty(&table).map_err(|e| e.to_string())?;
    // Write via tmp + rename so a crash mid-write cannot leave a truncated
    // config holding every API key.
    let tmp = path.with_extension("toml.tmp");
    std::fs::write(&tmp, out).map_err(|e| e.to_string())?;
    #[cfg(unix)]
    {
        // mykey.toml holds API keys — keep the plaintext window owner-only
        // (same policy as add_platform).
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(0o600));
    }
    std::fs::rename(&tmp, path).map_err(|e| e.to_string())?;
    if enc_path.exists() {
        // encrypt_config reads the plaintext we just renamed into place,
        // writes path.enc with 0600 permissions; the plaintext copy is then
        // removed so on-disk state stays "encrypted only" as before.
        if let Err(e) = oz_config::crypto::encrypt_config(path) {
            // Never leave plaintext keys behind when encryption fails — the
            // pre-existing .enc stays in place and the error is reported.
            let _ = std::fs::remove_file(path);
            return Err(e.to_string());
        }
        if let Err(e) = std::fs::remove_file(path) {
            tracing::warn!("could not remove plaintext mykey.toml after encryption: {e}");
        }
    }
    Ok(())
}

/// Model/session names become TOML section headers; reject anything that
/// would break parsing or inject into other TOML sections, plus the reserved
/// top-level keys of mykey.toml (writing a table under one of those would
/// break config parsing — e.g. `memory_backend` falling back to "file" and
/// silently disabling the ERME memory engine). The reserved list is
/// single-sourced from oz-config so a new top-level key can't drift.
fn valid_model_name(name: &str) -> bool {
    !name.is_empty()
        && name.chars().count() <= 64
        && !oz_config::mykey::RESERVED_TOP_LEVEL_KEYS.contains(&name)
        && !name.starts_with('.')
        && !name
            .chars()
            .any(|c| c.is_control() || matches!(c, '[' | ']' | '"' | '\'' | '#'))
}

/// Allowed input-modality values for `modalities` in a session entry.
const ALLOWED_MODALITIES: [&str; 4] = ["text", "image", "video", "audio"];

/// Normalize a stored modalities list: drop unknown values and duplicates,
/// and fall back to ["text"] when nothing valid remains (matches the
/// "absent = text-only" convention of SessionConfig::modalities).
fn normalize_modalities(raw: Option<&[String]>) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for m in raw.into_iter().flatten() {
        let m = m.trim().to_lowercase();
        if ALLOWED_MODALITIES.contains(&m.as_str()) && !out.contains(&m) {
            out.push(m);
        }
    }
    if out.is_empty() {
        out.push("text".to_string());
    }
    out
}

/// Build a session table from upsert args. Every field may be absent —
/// `upsert_model` fills blanks from the stored entry (edits keep existing
/// values); a brand-new standalone entry needs apibase, a provider-backed
/// one borrows apibase/apikey from `[providers.<id>]`, and every new entry
/// needs model.
fn session_table_from_args(args: &serde_json::Value) -> toml::Table {
    let context_win = args["context_win"]
        .as_u64()
        .unwrap_or(oz_config::mykey::DEFAULT_CONTEXT_WIN as u64);
    let mut t = toml::Table::new();
    for key in ["apibase", "model", "apikey"] {
        if let Some(v) = args[key].as_str().map(str::trim).filter(|s| !s.is_empty()) {
            t.insert(key.into(), toml::Value::String(v.to_string()));
        }
    }
    t.insert(
        "context_win".into(),
        toml::Value::Integer(context_win as i64),
    );
    if let Some(list) = args["modalities"].as_array() {
        let names: Vec<String> = list
            .iter()
            .filter_map(|v| v.as_str())
            .map(str::to_string)
            .collect();
        // Always write a normalized list (non-empty, deduped, whitelisted)
        // so the round-trip through from_file yields the same set.
        let normalized = normalize_modalities(Some(&names));
        t.insert(
            "modalities".into(),
            toml::Value::Array(normalized.into_iter().map(toml::Value::String).collect()),
        );
    }
    t
}

/// Create or update a model entry (session config) in mykey.toml.
#[tauri::command]
pub fn upsert_model(args: serde_json::Value, state: State<'_, Arc<AppState>>) -> serde_json::Value {
    let name = args["name"].as_str().unwrap_or("").trim().to_string();
    if !valid_model_name(&name) {
        return serde_json::json!({ "error": "invalid model name" });
    }
    match write_mykey_toml(&state.config_path, |table| {
        let mut entry = session_table_from_args(&args);
        // Fill blanks from the stored entry so an edit that leaves a field
        // empty keeps the existing value (apikey, apibase, model).
        if let Some(existing) = table.get(&name).and_then(|v| v.as_table()) {
            for key in ["apibase", "model", "apikey"] {
                if !entry.contains_key(key) {
                    if let Some(v) = existing.get(key) {
                        entry.insert(key.into(), v.clone());
                    }
                }
            }
        }
        // Provider reference: when set, the entry borrows apibase/apikey
        // from `[providers.<id>]` at parse time — inline copies are stripped
        // so credentials stay single-sourced. Unknown provider ids are
        // rejected here rather than silently dropping the entry later.
        if let Some(pid) = args["provider"]
            .as_str()
            .map(str::trim)
            .filter(|s| !s.is_empty())
        {
            // Validate against the *parsed* provider set, not raw key
            // presence: a malformed [providers.<id>] table (missing apibase)
            // is skipped by from_file, and referencing it would silently
            // drop the entry at parse time. A cheap re-parse is fine on this
            // cold path.
            let known = MyKeyConfig::from_file(&state.config_path)
                .map(|cfg| cfg.providers.contains_key(pid))
                .unwrap_or(false);
            if !known {
                return Err(format!("provider not found: {pid}"));
            }
            entry.insert("provider".into(), toml::Value::String(pid.to_string()));
            entry.remove("apibase");
            entry.remove("apikey");
        } else {
            entry.remove("provider");
            // SessionConfig deserialization requires apikey; a brand-new
            // inline entry without one gets an empty string (valid for
            // local no-auth servers).
            entry
                .entry("apikey".to_string())
                .or_insert_with(|| toml::Value::String(String::new()));
            if !entry.contains_key("apibase") {
                return Err("apibase is required".to_string());
            }
        }
        // model is required regardless of credential style: SessionConfig has
        // no default for it, so an entry without one is silently dropped by
        // every consumer's parse.
        if !entry.contains_key("model") {
            return Err("model is required".to_string());
        }
        table.insert(name.clone(), toml::Value::Table(entry));
        Ok(())
    }) {
        Ok(()) => serde_json::json!({ "status": "ok" }),
        Err(e) => serde_json::json!({ "error": e }),
    }
}

/// Delete a model entry; clears `default_session` when it pointed at it so
/// the config never keeps a dangling default.
#[tauri::command]
pub fn delete_model(name: String, state: State<'_, Arc<AppState>>) -> serde_json::Value {
    match write_mykey_toml(&state.config_path, |table| {
        if table.remove(&name).is_none() {
            return Err(format!("model not found: {name}"));
        }
        if table.get("default_session").and_then(|v| v.as_str()) == Some(name.as_str()) {
            table.remove("default_session");
        }
        Ok(())
    }) {
        Ok(()) => serde_json::json!({ "status": "ok" }),
        Err(e) => serde_json::json!({ "error": e }),
    }
}

/// Point `default_session` at an existing model entry.
#[tauri::command]
pub fn set_default_model(name: String, state: State<'_, Arc<AppState>>) -> serde_json::Value {
    match write_mykey_toml(&state.config_path, |table| {
        if !table.contains_key(&name) {
            return Err(format!("model not found: {name}"));
        }
        table.insert("default_session".into(), toml::Value::String(name.clone()));
        Ok(())
    }) {
        Ok(()) => serde_json::json!({ "status": "ok" }),
        Err(e) => serde_json::json!({ "error": e }),
    }
}

// ── Provider management ─────────────────────────────────────────────────────
//
// A provider (`[providers.<id>]`) holds one apibase + apikey shared by any
// number of model entries. Keys are write-only: list payloads carry a
// masked hint, never the literal value.

/// Mask an API key for display. Short keys are hidden entirely — showing
/// prefix+suffix on a 9-char token would reveal all but one character.
fn mask_key(key: &str) -> String {
    if key.is_empty() {
        return String::new();
    }
    let chars: Vec<char> = key.chars().collect();
    if chars.len() <= 12 {
        return "••••".to_string();
    }
    let head = if chars.len() > 20 { 4 } else { 2 };
    let prefix: String = chars[..head].iter().collect();
    let suffix: String = chars[chars.len() - head..].iter().collect();
    format!("{prefix}…{suffix}")
}

/// List configured providers with a masked key hint and reference counts
/// (the literal key never leaves the config).
#[tauri::command]
pub fn list_providers(state: State<'_, Arc<AppState>>) -> Vec<ProviderEntry> {
    let cfg_path = std::path::Path::new(&state.config_path);
    let Ok(cfg) = MyKeyConfig::from_file(cfg_path) else {
        return vec![];
    };
    let mut entries: Vec<ProviderEntry> = cfg
        .providers
        .iter()
        .map(|(id, p)| ProviderEntry {
            id: id.clone(),
            apibase: p.apibase.clone(),
            key_hint: mask_key(&p.apikey),
            model_count: cfg
                .sessions
                .values()
                .filter(|s| s.provider.as_deref() == Some(id.as_str()))
                .count(),
        })
        .collect();
    entries.sort_by(|a, b| a.id.cmp(&b.id));
    entries
}

/// Create or update `[providers.<id>]`. A blank apikey on edit keeps the
/// stored value (the UI never has the literal key to send back).
#[tauri::command]
pub fn upsert_provider(
    args: serde_json::Value,
    state: State<'_, Arc<AppState>>,
) -> serde_json::Value {
    let id = args["id"].as_str().unwrap_or("").trim().to_string();
    if !valid_model_name(&id) {
        return serde_json::json!({ "error": "invalid provider id" });
    }
    let apibase = args["apibase"].as_str().unwrap_or("").trim().to_string();
    let apikey = args["apikey"].as_str().unwrap_or("").trim().to_string();
    match write_mykey_toml(&state.config_path, |table| {
        if apibase.is_empty() {
            return Err("apibase is required".to_string());
        }
        let providers = table
            .entry("providers".to_string())
            .or_insert_with(|| toml::Value::Table(toml::Table::new()));
        let Some(pt) = providers.as_table_mut() else {
            return Err("[providers] is not a table".to_string());
        };
        let mut entry = toml::Table::new();
        entry.insert("apibase".into(), toml::Value::String(apibase));
        // Blank apikey on edit keeps the stored key (write-only field).
        if !apikey.is_empty() {
            entry.insert("apikey".into(), toml::Value::String(apikey));
        } else if let Some(existing) = pt.get(&id).and_then(|v| v.get("apikey")) {
            entry.insert("apikey".into(), existing.clone());
        } else {
            entry.insert("apikey".into(), toml::Value::String(String::new()));
        }
        pt.insert(id, toml::Value::Table(entry));
        Ok(())
    }) {
        Ok(()) => serde_json::json!({ "status": "ok" }),
        Err(e) => serde_json::json!({ "error": e }),
    }
}

/// Delete `[providers.<id>]` and cascade-delete every model entry that
/// references it (user-approved in the UI with a confirmation count).
/// Clears a dangling `default_session` as delete_model does.
#[tauri::command]
pub fn delete_provider(id: String, state: State<'_, Arc<AppState>>) -> serde_json::Value {
    let mut deleted_models = 0usize;
    match write_mykey_toml(&state.config_path, |table| {
        let removed = table
            .get_mut("providers")
            .and_then(|v| v.as_table_mut())
            .and_then(|pt| pt.remove(&id))
            .is_some();
        if !removed {
            return Err(format!("provider not found: {id}"));
        }
        let mut orphans: Vec<String> = Vec::new();
        collect_provider_orphans(table, &id, "", &mut orphans);
        for name in &orphans {
            remove_entry_by_dotted_name(table, name);
            deleted_models += 1;
        }
        // Clear a dangling default_session for both plain and dotted names.
        if let Some(def) = table.get("default_session").and_then(|v| v.as_str()) {
            if orphans.iter().any(|n| n == def) {
                table.remove("default_session");
            }
        }
        Ok(())
    }) {
        Ok(()) => serde_json::json!({ "status": "ok", "deleted_models": deleted_models }),
        Err(e) => serde_json::json!({ "error": e }),
    }
}

/// Collect dotted names of session entries that reference `provider_id`,
/// including entries under dotted-key sections (`[qwen3.6-27b]` parses as
/// nested tables `qwen3` → `6-27b`). Only tables that look like a session
/// entry (carry `provider` plus a model-shape field) count, so unrelated
/// config tables that happen to hold a `provider` key (web_search,
/// platforms.*) are never harvested.
fn collect_provider_orphans(
    table: &toml::Table,
    provider_id: &str,
    prefix: &str,
    out: &mut Vec<String>,
) {
    for (key, value) in table {
        if prefix.is_empty() && oz_config::mykey::RESERVED_TOP_LEVEL_KEYS.contains(&key.as_str()) {
            continue;
        }
        let Some(sub) = value.as_table() else {
            continue;
        };
        let full = if prefix.is_empty() {
            key.clone()
        } else {
            format!("{prefix}.{key}")
        };
        if oz_config::mykey::provider_ref(sub) == Some(provider_id)
            && oz_config::mykey::looks_like_session_entry(sub)
        {
            out.push(full);
        } else {
            collect_provider_orphans(sub, provider_id, &full, out);
        }
    }
}

/// Remove the entry at a dotted name, pruning parent tables that become
/// empty (a dotted section only exists as nesting — `[a.b]` is a table `b`
/// inside table `a`).
fn remove_entry_by_dotted_name(table: &mut toml::Table, dotted: &str) {
    fn walk(node: &mut toml::Table, parts: &[&str]) {
        let Some((head, rest)) = parts.split_first() else {
            return;
        };
        if rest.is_empty() {
            node.remove(*head);
            return;
        }
        let parent_empty = {
            let Some(child) = node.get_mut(*head).and_then(|v| v.as_table_mut()) else {
                return;
            };
            walk(child, rest);
            child.is_empty()
        };
        if parent_empty {
            node.remove(*head);
        }
    }
    let parts: Vec<&str> = dotted.split('.').collect();
    walk(table, &parts);
}

/// ── One-time migration: dedupe repeated credentials into providers ────────
///
/// Pre-provider configs repeat the same apibase+apikey in every model entry.
/// At startup (before anything reads the config), entries sharing an exact
/// (apibase, apikey) pair — two or more of them — are merged into one
/// `[providers.<id>]` table; the entries keep all their other fields and
/// gain `provider = "<id>"` instead of the repeated credentials. The on-disk
/// file (plaintext or .enc) is copied to a `.bak-providers-<unix_ts>`
/// sibling first. Idempotent: a file that already has a `[providers]` table
/// or no duplicated pairs is left untouched. Entry names — the identity
/// referenced by default_session / profiles / the frontend — never change.
pub(crate) fn migrate_mykey_to_providers(config_path: &str) {
    let path = std::path::Path::new(config_path);
    let Ok(content) = oz_config::crypto::read_config(path) else {
        return; // no config yet — nothing to migrate
    };
    let Ok(table) = content.parse::<toml::Table>() else {
        tracing::warn!("provider migration: config does not parse as TOML, skipping");
        return;
    };
    if !needs_provider_migration(&table) {
        return;
    }

    // Backup whatever on-disk state exists (plaintext and/or encrypted)
    // before the first mutation. fs::copy preserves the 0600 perms.
    let ts = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let mut backed_up = Vec::new();
    for candidate in [path.to_path_buf(), path.with_extension("toml.enc")] {
        if candidate.exists() {
            let bak = candidate.with_file_name(format!(
                "{}.bak-providers-{ts}",
                candidate
                    .file_name()
                    .and_then(|n| n.to_str())
                    .unwrap_or("mykey.toml")
            ));
            match std::fs::copy(&candidate, &bak) {
                Ok(_) => backed_up.push(bak),
                Err(e) => {
                    // Never migrate without a backup — a bad write would be
                    // unrecoverable and the file holds every API key.
                    tracing::error!(
                        "provider migration aborted: cannot back up {}: {e}",
                        candidate.display()
                    );
                    return;
                }
            }
        }
    }

    match write_mykey_toml(config_path, apply_provider_migration) {
        Ok(()) => {
            let merged: Vec<String> = backed_up.iter().map(|p| p.display().to_string()).collect();
            tracing::info!(
                "provider migration: merged duplicated credentials into [providers] (backup: {})",
                merged.join(", ")
            );
        }
        Err(e) => tracing::error!("provider migration write failed (backup kept): {e}"),
    }
}

/// Detection pass: true when there is no `[providers]` table yet and at
/// least one (apibase, apikey) pair is shared by 2+ top-level model entries.
fn needs_provider_migration(table: &toml::Table) -> bool {
    if table.contains_key("providers") {
        return false;
    }
    credential_counts(table).values().any(|count| *count >= 2)
}

/// Count top-level model entries per exact (apibase, apikey) pair — both
/// migration callers only need "how many share this pair", never the names.
/// Only plain string credentials count; anything else stays untouched.
fn credential_counts(table: &toml::Table) -> HashMap<(String, String), usize> {
    let mut counts: HashMap<(String, String), usize> = HashMap::new();
    for (name, value) in table {
        if oz_config::mykey::RESERVED_TOP_LEVEL_KEYS.contains(&name.as_str()) {
            continue;
        }
        let Some(t) = value.as_table() else { continue };
        let (Some(apibase), Some(apikey)) = (
            t.get("apibase").and_then(|v| v.as_str()),
            t.get("apikey").and_then(|v| v.as_str()),
        ) else {
            continue;
        };
        *counts
            .entry((apibase.to_string(), apikey.to_string()))
            .or_insert(0) += 1;
    }
    counts
}

/// The write-half of the migration, run inside `write_mykey_toml` (which
/// re-reads the config under CONFIG_WRITE_LOCK). No-op unless the same
/// conditions hold on the fresh table, so concurrent edits cannot corrupt.
fn apply_provider_migration(table: &mut toml::Table) -> Result<(), String> {
    if table.contains_key("providers") {
        return Ok(()); // someone else migrated first — nothing to do
    }
    let counts = credential_counts(table);
    let mut provider_id_by_cred: HashMap<(String, String), String> = HashMap::new();
    let mut providers = toml::Table::new();
    for ((apibase, apikey), count) in &counts {
        if *count < 2 {
            continue; // single entries stay inline
        }
        let id = derive_provider_id(apibase, &providers);
        let mut entry = toml::Table::new();
        entry.insert("apibase".into(), toml::Value::String(apibase.clone()));
        entry.insert("apikey".into(), toml::Value::String(apikey.clone()));
        providers.insert(id.clone(), toml::Value::Table(entry));
        provider_id_by_cred.insert((apibase.clone(), apikey.clone()), id);
    }
    if provider_id_by_cred.is_empty() {
        return Ok(());
    }
    for (name, value) in table.iter_mut() {
        if oz_config::mykey::RESERVED_TOP_LEVEL_KEYS.contains(&name.as_str()) {
            continue;
        }
        let Some(t) = value.as_table_mut() else {
            continue;
        };
        let (Some(apibase), Some(apikey)) = (
            t.get("apibase").and_then(|v| v.as_str()),
            t.get("apikey").and_then(|v| v.as_str()),
        ) else {
            continue;
        };
        let Some(id) = provider_id_by_cred.get(&(apibase.to_string(), apikey.to_string())) else {
            continue;
        };
        t.remove("apibase");
        t.remove("apikey");
        t.insert("provider".into(), toml::Value::String(id.clone()));
    }
    table.insert("providers".into(), toml::Value::Table(providers));
    Ok(())
}

/// Derive a readable provider id from the endpoint host, e.g.
/// `http://127.0.0.1:8000/v1` → `127_0_0_1_8000`. Sanitized to TOML-safe
/// characters, deduplicated against ids already in the table.
fn derive_provider_id(apibase: &str, taken: &toml::Table) -> String {
    let without_scheme = apibase
        .split_once("://")
        .map(|(_, rest)| rest)
        .unwrap_or(apibase);
    let authority = without_scheme
        .split(['/', '?', '#'])
        .next()
        .unwrap_or(without_scheme)
        // Strip userinfo if someone put credentials in the URL.
        .rsplit('@')
        .next()
        .unwrap_or(without_scheme);
    let base: String = authority
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' || c == '_' {
                c
            } else {
                '_'
            }
        })
        .collect();
    let base = base.trim_matches('_').to_string();
    let stem = if base.is_empty() {
        "provider".to_string()
    } else {
        base
    };
    let mut id = stem.clone();
    let mut n = 1;
    while taken.contains_key(&id) {
        n += 1;
        id = format!("{stem}_{n}");
    }
    id
}

/// Skill/SOP inventory for the settings panel, sourced from the process-wide
/// store (same data the agent's skill_mcp tools serve). `active` mirrors
/// `SkillMcpMetadata::is_active()`.
#[tauri::command]
pub fn list_skill_mcp(state: State<'_, Arc<AppState>>) -> serde_json::Value {
    let Some(dir) = state.skill_mcp_dir.clone() else {
        return serde_json::json!({ "skills": [], "sops": [] });
    };
    let store = crate::get_or_init_skill_store(&state.shared_skill_store, &dir);
    let Ok(mut guard) = store.try_lock() else {
        return serde_json::json!({ "busy": true, "skills": [], "sops": [] });
    };
    guard.reload_incremental();
    let skills: Vec<serde_json::Value> = guard
        .skills
        .list()
        .iter()
        .map(|s| {
            serde_json::json!({
                "name": s.name,
                "description": s.description,
                "active": s.metadata.is_active(),
                "quality": s.quality,
                "successCount": s.metadata.success_count,
                "failureCount": s.metadata.failure_count,
            })
        })
        .collect();
    let sops: Vec<serde_json::Value> = guard
        .sops
        .all()
        .iter()
        .map(|s| {
            serde_json::json!({
                "name": s.name,
                "description": s.description,
                "active": s.metadata.is_active(),
            })
        })
        .collect();
    serde_json::json!({ "skills": skills, "sops": sops })
}

/// Enable/disable a skill or SOP by flipping its metadata `stale_flag`
/// (the inverse of `is_active()`), persisted to the artifact's meta.toml.
/// Takes effect on the next agent run (stores reload incrementally).
#[tauri::command]
pub fn toggle_skill_mcp(
    kind: String,
    name: String,
    active: bool,
    state: State<'_, Arc<AppState>>,
) -> serde_json::Value {
    let Some(dir) = state.skill_mcp_dir.clone() else {
        return serde_json::json!({ "error": "skill store not configured" });
    };
    let category = match kind.as_str() {
        "skill" => "skills",
        "sop" => "sops",
        _ => return serde_json::json!({ "error": "kind must be skill|sop" }),
    };
    let store = crate::get_or_init_skill_store(&state.shared_skill_store, &dir);
    let Ok(mut guard) = store.try_lock() else {
        return serde_json::json!({ "error": "skill store busy" });
    };
    // Source the metadata key + description/tags. Skills key their meta by
    // name (upsert_skill); SOPs key it by the md file stem (load_all), which
    // can differ from the display name — writing by display name would store
    // metadata where the loader never reads it.
    let found = match category {
        "skills" => guard
            .skills
            .list()
            .iter()
            .find(|s| s.name == name)
            .map(|s| (s.name.clone(), s.description.clone(), s.tags.clone())),
        _ => guard.sops.all().iter().find(|s| s.name == name).map(|s| {
            let key = s
                .source_path
                .file_stem()
                .and_then(|stem| stem.to_str())
                .unwrap_or(&s.name)
                .to_string();
            (key, s.description.clone(), s.tags.clone())
        }),
    };
    let Some((meta_key, description, tags)) = found else {
        return serde_json::json!({ "error": format!("not found: {name}") });
    };
    let mut meta = match guard.meta.load(category, &meta_key) {
        Ok(Some(m)) => m,
        Ok(None) => oz_core_types::skill_mcp::SkillMcpMetadata::new(&name, &description, tags),
        Err(e) => return serde_json::json!({ "error": e.to_string() }),
    };
    meta.stale_flag = !active;
    // is_active() = !stale_flag && quality_score >= 0.3 — re-activating an
    // artifact whose score has decayed below the floor needs a bump back to
    // the neutral default, or the toggle would report ok yet change nothing.
    if active && meta.quality_score < 0.3 {
        meta.quality_score = 0.5;
    }
    meta.updated_at = chrono::Utc::now().to_rfc3339();
    if let Err(e) = guard.meta.save(category, &meta_key, &meta) {
        return serde_json::json!({ "error": e.to_string() });
    }
    guard.reload_incremental();
    serde_json::json!({ "status": "ok" })
}

/// MCP server inventory from `{working_dir}/servers.toml` (the same file the
/// webui/server backend loads). Empty when the file does not exist.
#[tauri::command]
pub fn list_mcp_servers(state: State<'_, Arc<AppState>>) -> serde_json::Value {
    let path = std::path::Path::new(&state.working_dir).join("servers.toml");
    let Ok(content) = std::fs::read_to_string(&path) else {
        return serde_json::json!({ "servers": [] });
    };
    match toml::from_str::<oz_mcp::config::ServersToml>(&content) {
        Ok(cfg) => {
            let servers: Vec<serde_json::Value> = cfg
                .servers
                .iter()
                .map(|s| {
                    serde_json::json!({
                        "name": s.name,
                        "command": s.command,
                        "enabled": s.enabled,
                        "autoStart": s.auto_start,
                    })
                })
                .collect();
            serde_json::json!({ "servers": servers })
        }
        Err(e) => serde_json::json!({ "error": format!("servers.toml parse error: {e}") }),
    }
}

/// Toggle an MCP server's `enabled` flag in servers.toml. Edited with
/// `toml_edit` so hand-written comments and formatting in the file survive
/// the round-trip.
#[tauri::command]
pub fn toggle_mcp_server(
    name: String,
    enabled: bool,
    state: State<'_, Arc<AppState>>,
) -> serde_json::Value {
    let path = std::path::Path::new(&state.working_dir).join("servers.toml");
    let Ok(content) = std::fs::read_to_string(&path) else {
        return serde_json::json!({ "error": "servers.toml not found" });
    };
    let mut doc: toml_edit::Document = match content.parse() {
        Ok(d) => d,
        Err(e) => {
            return serde_json::json!({ "error": format!("servers.toml parse error: {e}") });
        }
    };
    let Some(servers) = doc
        .get_mut("servers")
        .and_then(|i| i.as_array_of_tables_mut())
    else {
        return serde_json::json!({ "error": "servers.toml has no [[servers]] array" });
    };
    let Some(server) = servers
        .iter_mut()
        .find(|t| t.get("name").and_then(|v| v.as_str()) == Some(name.as_str()))
    else {
        return serde_json::json!({ "error": format!("server not found: {name}") });
    };
    server["enabled"] = toml_edit::value(enabled);
    // Atomic write: a crash mid-write must not truncate the MCP config.
    let tmp = path.with_extension("toml.tmp");
    let out = doc.to_string();
    if let Err(e) = std::fs::write(&tmp, out).and_then(|()| std::fs::rename(&tmp, &path)) {
        return serde_json::json!({ "error": e.to_string() });
    }
    serde_json::json!({ "status": "ok" })
}

/// Rough token estimate mirroring the frontend `estimateTokens` heuristic
/// (len/4). Reads `tokensIn`/`tokensOut` when a message carries real usage
/// numbers and falls back to content-length estimation otherwise.
fn message_tokens(m: &serde_json::Value) -> (u64, u64) {
    let est = || -> u64 {
        let content = &m["content"];
        let len = match content.as_str() {
            Some(s) => s.len(),
            None => content
                .as_array()
                .map(|a| {
                    a.iter()
                        .map(|b| {
                            b["text"].as_str().map(str::len).unwrap_or(0)
                                + b["content"].as_str().map(str::len).unwrap_or(0)
                        })
                        .sum::<usize>()
                })
                .unwrap_or(0),
        };
        len.div_ceil(4) as u64
    };
    (
        m["tokensIn"].as_u64().unwrap_or_else(est),
        m["tokensOut"].as_u64().unwrap_or_else(est),
    )
}

/// Aggregate token usage across the most recent sessions (default 50):
/// totals, per-day (UTC date from message timestamps) and per-model sums.
#[tauri::command]
pub fn get_token_stats(limit: Option<usize>, state: State<'_, Arc<AppState>>) -> serde_json::Value {
    // No unconditional `reload()`: re-reading + re-parsing the whole
    // (measured: 93 MB) sessions file on every stats read was the original
    // problem (round5 P1-7). `reload_if_stale()` keeps cross-process
    // correctness — webui/TUI/CLI can share one data root, and a full-file
    // write from this side would otherwise erase their new sessions — at the
    // cost of one `stat` (review finding A-F5).
    let mut store = lock_poison_guard(&state.sessions);
    store.reload_if_stale();
    let max = limit.unwrap_or(50).clamp(1, 500);
    let mut infos: Vec<_> = store.list();
    infos.sort_by(|a, b| b.created_at.cmp(&a.created_at));
    infos.truncate(max);

    let mut total_in: u64 = 0;
    let mut total_out: u64 = 0;
    let mut per_day: std::collections::BTreeMap<String, (u64, u64)> = Default::default();
    let mut per_model: std::collections::BTreeMap<String, (u64, u64)> = Default::default();
    let mut per_session: Vec<serde_json::Value> = Vec::with_capacity(infos.len());

    for info in &infos {
        let Some(entry) = store.get(&info.id) else {
            continue;
        };
        let mut s_in: u64 = 0;
        let mut s_out: u64 = 0;
        for m in &entry.messages {
            let (tin, tout) = message_tokens(m);
            s_in += tin;
            s_out += tout;
            if let Some(day) = m["timestamp"].as_str().and_then(|t| t.get(..10)) {
                let e = per_day.entry(day.to_string()).or_insert((0, 0));
                e.0 += tin;
                e.1 += tout;
            }
            if let Some(model) = m["modelInfo"]["model"].as_str() {
                let e = per_model.entry(model.to_string()).or_insert((0, 0));
                e.0 += tin;
                e.1 += tout;
            }
        }
        total_in += s_in;
        total_out += s_out;
        per_session.push(serde_json::json!({
            "id": info.id,
            "name": info.name,
            "createdAt": info.created_at,
            "messageCount": info.message_count,
            "tokensIn": s_in,
            "tokensOut": s_out,
        }));
    }

    serde_json::json!({
        "totals": { "in": total_in, "out": total_out },
        "perDay": per_day.iter().map(|(d, (i, o))| serde_json::json!({
            "day": d, "in": i, "out": o,
        })).collect::<Vec<_>>(),
        "perModel": per_model.iter().map(|(m, (i, o))| serde_json::json!({
            "model": m, "in": i, "out": o,
        })).collect::<Vec<_>>(),
        "perSession": per_session,
    })
}

/// QC-3: aggregate quality events (reflections.jsonl: failures, successes,
/// lessons, synthesized specs) across every known project working dir —
/// last-7-day counts per type plus lifetime totals. Persists the report to
/// {data_dir}/openzen/quality_report.json so nightly tooling can diff runs.
#[tauri::command]
pub fn get_quality_report(state: State<'_, Arc<AppState>>) -> serde_json::Value {
    // See get_token_stats: stat-based staleness check, not a full re-parse.
    let mut store = lock_poison_guard(&state.sessions);
    store.reload_if_stale();
    let mut dirs: Vec<String> = store
        .list()
        .iter()
        .filter_map(|s| s.working_dir.clone())
        .collect();
    drop(store);
    dirs.sort();
    dirs.dedup();

    let cutoff = chrono::Utc::now() - chrono::Duration::days(7);
    let mut week: std::collections::BTreeMap<String, u64> = Default::default();
    let mut total: std::collections::BTreeMap<String, u64> = Default::default();
    let mut unparsable: u64 = 0;
    // Per-run quality rows are the honest denominator: exactly one per run,
    // including the ones that skipped the gate (round5 P2-22).
    let mut runs_week: u64 = 0;
    let mut runs_total: u64 = 0;
    let mut gate_skipped_total: std::collections::BTreeMap<String, u64> = Default::default();
    for dir in &dirs {
        let path = Path::new(dir).join(".openzen").join("reflections.jsonl");
        let Ok(content) = std::fs::read_to_string(&path) else {
            continue;
        };
        for line in content.lines() {
            let Ok(v) = serde_json::from_str::<serde_json::Value>(line) else {
                // Torn lines used to be dropped silently, so a writer bug
                // looked identical to "no data" (round5 P2-26).
                unparsable += 1;
                continue;
            };
            let kind = v
                .get("type")
                .and_then(|t| t.as_str())
                .unwrap_or("unknown")
                .to_string();
            *total.entry(kind.clone()).or_insert(0) += 1;
            if kind == "run_quality" {
                runs_total += 1;
                if let Some(reason) = v.get("gate_skipped").and_then(|r| r.as_str()) {
                    *gate_skipped_total.entry(reason.to_string()).or_insert(0) += 1;
                }
            }
            if let Some(ts) = v
                .get("ts")
                .and_then(|t| t.as_str())
                .and_then(|raw| chrono::DateTime::parse_from_rfc3339(raw).ok())
            {
                if ts.with_timezone(&chrono::Utc) >= cutoff {
                    if kind == "run_quality" {
                        runs_week += 1;
                    }
                    *week.entry(kind).or_insert(0) += 1;
                }
            }
        }
    }

    // P2-12: denominators and rates, not just counts. The counts alone
    // could not answer "is delivery quality improving": a pass count
    // means nothing without attempts, and rework was invisible.
    let count_of = |map: &std::collections::BTreeMap<String, u64>, key: &str| -> u64 {
        map.get(key).copied().unwrap_or(0)
    };
    // The denominator is RUNS, not events: one run can emit
    // review_failed (round 1) and review_passed (round 2), which used to show
    // as a 50% pass rate for a run that passed, and a run whose second round
    // failed emitted nothing at all (review B8). `run_quality` rows are
    // exactly one per run — including every abnormal exit.
    let rates_for = |map: &std::collections::BTreeMap<String, u64>| -> serde_json::Value {
        let runs = count_of(map, "run_quality");
        let passed = count_of(map, "review_passed");
        let failed = count_of(map, "review_failed");
        let unavailable = count_of(map, "review_unavailable");
        let not_run = count_of(map, "review_not_run");
        let blocked = count_of(map, "assertion_blocked_by_policy");
        // Per-run verdicts: a run counts once, by its FINAL state.
        let deliveries = count_of(map, "delivery_success");
        let assertion_failures = count_of(map, "assertion_failed");
        let rate = |num: u64, den: u64| -> Option<f64> {
            if den == 0 {
                None
            } else {
                Some((num as f64 / den as f64 * 1000.0).round() / 1000.0)
            }
        };
        serde_json::json!({
            // Raw counters first: the UI reads these directly and a missing
            // key shows up as 0, which is indistinguishable from "never ran"
            // (round5 P2-22 / frontend contract).
            "review_passed": passed,
            "review_failed": failed,
            "review_unavailable": unavailable,
            "review_not_run": not_run,
            "assertion_failed": assertion_failures,
            "assertion_blocked_by_policy": blocked,
            "runs": runs,
            "review_attempts": passed + failed,
            // Both rates use the run count as the denominator, so they answer
            // "of the runs we ran, how many ended verified?".
            "review_pass_rate": rate(passed, runs),
            "review_rework_rate": rate(failed, runs),
            "deliveries": deliveries,
            // How often a delivery needed an assertion-failure round.
            "assertion_failure_rate": rate(assertion_failures, deliveries),
            // Coverage counts UNREVIEWED and UNAVAILABLE in the denominator:
            // "100% pass rate" over 33% coverage was the misleading number
            // this whole phase exists to fix.
            "review_coverage": rate(passed + failed, runs),
            "review_coverage_of_deliveries": rate(passed + failed, deliveries),
        })
    };

    let report = serde_json::json!({
        "generated_at": chrono::Utc::now().to_rfc3339(),
        "projects_scanned": dirs.len(),
        "window_days": 7,
        "week": week,
        "total": total,
        "runs_week": runs_week,
        "runs_total": runs_total,
        "gate_skipped_total": gate_skipped_total,
        // A torn telemetry line is a writer bug, not "no data".
        "unparsable_lines": unparsable,
        "rates_week": rates_for(&week),
        "rates_total": rates_for(&total),
    });
    let out_dir = crate::data_dir().join("openzen");
    let _ = std::fs::create_dir_all(&out_dir);
    if let Ok(json) = serde_json::to_string_pretty(&report) {
        let _ = std::fs::write(out_dir.join("quality_report.json"), json);
    }
    report
}

#[tauri::command]
pub fn list_sessions(
    project_id: Option<String>,
    state: State<'_, Arc<AppState>>,
) -> Vec<SessionInfo> {
    // See get_token_stats: reload only when another process actually wrote
    // (one `stat`), instead of re-parsing the whole file on every refresh.
    let mut store = lock_poison_guard(&state.sessions);
    store.reload_if_stale();
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
pub fn get_session(
    id: String,
    offset: Option<usize>,
    limit: Option<usize>,
    state: State<'_, Arc<AppState>>,
) -> serde_json::Value {
    let mut store = lock_poison_guard(&state.sessions);
    match store.get(&id) {
        Some(entry) => {
            let wd = store
                .get(&id)
                .and_then(|e| e.working_dir.clone())
                .unwrap_or_else(|| state.working_dir.clone());
            // (`idx` is attached per message by the pagination block below —
            // that is what `submit_message_feedback` resolves against.)
            if entry.status == SessionStatus::Running {
                let agents = lock_poison_guard(&state.running_agents);
                if !agents.contains_key(&id) {
                    // Use the session's own working_dir (project root)
                    // so checkpoints are found for project sessions.
                    recover_session_from_checkpoints(&mut store, &id, &wd);
                }
            } else if store
                .get(&id)
                .map(|e| e.status != SessionStatus::Waiting && !has_assistant_message(e))
                .unwrap_or(true)
            {
                // Not running and no assistant message persisted: restore the
                // full conversation (and todos) from the checkpoint. This is
                // the "bubbles vanished after restart" case — the agent was
                // killed mid-task before after_run could save.
                recover_session_from_checkpoints(&mut store, &id, &wd);
            }

            let Some(session) = store.get(&id) else {
                return serde_json::json!({ "error": "not found" });
            };
            let mut value = serde_json::to_value(session).unwrap_or_default();

            // Pagination. `offset` counts from the END of the message
            // vector (0 = newest page), matching the web endpoint. Each
            // returned message gets its original `idx` so the frontend
            // can prepend older pages without changing Svelte keys.
            let total = value
                .get("messages")
                .and_then(|m| m.as_array())
                .map(|m| m.len())
                .unwrap_or(0);
            if let Some(messages) = value.get_mut("messages").and_then(|m| m.as_array_mut()) {
                let offset = offset.unwrap_or(0).min(total);
                let end = limit
                    .map(|limit| offset.saturating_add(limit).min(total))
                    .unwrap_or(total);
                let start = total.saturating_sub(end);
                let page: Vec<serde_json::Value> = messages[start..end]
                    .iter()
                    .enumerate()
                    .map(|(page_pos, message)| {
                        let mut message = message.clone();
                        if let Some(obj) = message.as_object_mut() {
                            obj.insert("idx".to_string(), serde_json::json!(start + page_pos));
                        }
                        message
                    })
                    .collect();
                *messages = page;
                value["total_messages"] = serde_json::json!(total);
                value["offset"] = serde_json::json!(offset);
                value["limit"] = serde_json::json!(end - start);
                value["has_more"] = serde_json::json!(start > 0);
            }
            value
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
            // `/compact` turns a carrier whose tool payload was folded away
            // into this marker (oz_core::compress::message_to_store_message) —
            // it is machine output, never a user prompt.
            if !content.is_empty() && content != "[compressed tool output]" {
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
            for tu in tool_uses.iter() {
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
            let events = oz_core_types::event::compact_stream_events(&events);
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
    // Deleting a conversation must not leave its background work running:
    // the owner row is about to disappear, so no UI could ever kill it.
    let cancelled_jobs = oz_core::jobs::global().cancel_owner(&id, Some("session deleted".into()));
    let _ = stop_child_agents(&id, &state);
    if cancelled_jobs > 0 {
        tracing::info!("[sessions] delete {id}: cancelled {cancelled_jobs} background job(s)");
    }
    let working_dir = lock_poison_guard(&state.sessions)
        .get(&id)
        .map(|e| e.working_dir.clone())
        .unwrap_or(None);
    lock_poison_guard(&state.sessions).delete(&id);
    // Reclaim the session's checkpoint + WAL files. Deleting a session used
    // to leave them behind forever (round5 P0-2).
    let dirs = crate::checkpoint_dirs_for(&working_dir);
    let mut removed = 0usize;
    for d in &dirs {
        removed += oz_core::checkpoint::purge_session_checkpoints(d, &id);
    }
    if removed > 0 {
        tracing::info!("[sessions] delete {id}: reclaimed {removed} checkpoint file(s)");
    }
    Ok(serde_json::json!({"status":"ok", "reclaimed_files": removed}))
}

/// Keep the most recent `keep` `compact_removed_*.json` dumps and at most
/// `max_bytes` in total.
///
/// The dump exists so a bad compression can be inspected by hand, but each
/// one is a whole removed window (tool results, sometimes inline images) and
/// nothing ever deleted them — an unbounded writer added in the same round
/// that bounded checkpoints (review findings C-F4 / D-8).
fn rotate_compact_dumps(dir: &std::path::Path, keep: usize, max_bytes: u64) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    let mut dumps: Vec<(std::time::SystemTime, u64, std::path::PathBuf)> = entries
        .flatten()
        .filter_map(|e| {
            let name = e.file_name().to_string_lossy().to_string();
            if !name.starts_with("compact_removed_") || !name.ends_with(".json") {
                return None;
            }
            let meta = e.metadata().ok()?;
            Some((meta.modified().ok()?, meta.len(), e.path()))
        })
        .collect();
    // Newest first.
    dumps.sort_by_key(|d| std::cmp::Reverse(d.0));
    let mut total = 0u64;
    for (i, (_, size, path)) in dumps.iter().enumerate() {
        total = total.saturating_add(*size);
        if i >= keep || total > max_bytes {
            match std::fs::remove_file(path) {
                Ok(()) => debug_log(&format!("compact: rotated out {}", path.display())),
                Err(e) => tracing::warn!("compact: failed to rotate {}: {e}", path.display()),
            }
        }
    }
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
    // Always signal. The old early-return on `status != Running` made Stop a
    // no-op in exactly the case that mattered: a session whose stored status
    // had been flipped to Stopped (ghost flip after 30 minutes without a
    // persisted message) while its agent was still executing. `stop_session`
    // also has to work as an "abort whatever is running under this id" verb.
    let was_running = lock_poison_guard(&state.running_agents).contains_key(&id)
        || lock_poison_guard(&state.detached_agents).contains_key(&id);
    // An explicit Stop dismisses the question: a dialog must not survive
    // the user's decision to abandon it. App shutdown never comes through
    // this command, so its parked question stays resumable.
    {
        let mut store = lock_poison_guard(&state.sessions);
        if let Some(entry) = store.get_mut(&id) {
            entry.pending_ask_user = None;
        }
        store.save();
    }
    stop_running_agent(&id, &state).await;
    // Stop also means "stop the work I started": background jobs owned by
    // this session would otherwise keep running with no UI able to kill them
    // (their owner row can be gone after a restart).
    let cancelled_jobs = oz_core::jobs::global().cancel_owner(&id, Some("session stopped".into()));
    let cancelled_children = stop_child_agents(&id, &state);
    Ok(serde_json::json!({
        "status": if was_running { "ok" } else { "signalled" },
        "cancelled_jobs": cancelled_jobs,
        "cancelled_subagents": cancelled_children,
    }))
}

/// Hard cap on user-supplied message bodies accepted over IPC — a webview
/// (or a buggy client) must not be able to force multi-GB strings into the
/// session store (P3/A8).
const MAX_MESSAGE_CHARS: usize = 1_000_000;

/// Session-store body for an injected message. A live interjection is
/// persisted with the SAME `[USER INTERVENTION - inject_info]` prefix the
/// agent loop pushes into the LLM context (checkpoint::apply_intervention):
/// the web layer's parseSessionMessages folds those into the preceding
/// assistant bubble, so a reload shows the card inside the agent turn
/// instead of a separate user bubble, and the next run's rebuilt history
/// carries the interjection verbatim. Without a running agent the message
/// is a plain user turn.
fn intervention_stored_content(agent_running: bool, text: &str) -> String {
    if agent_running {
        format!(
            "[USER INTERVENTION - {}]\n{}",
            oz_core::checkpoint::InterventionKind::InjectInfo,
            text
        )
    } else {
        text.to_string()
    }
}

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

    // 1. Push intervention into the agent's queue (before the store write:
    //    whether a queue exists decides how the message is persisted).
    let mut agent_running = false;
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
            agent_running = true;
            debug_log(&format!(
                "inject_message: pushed intervention to session={}",
                session_id
            ));
        } else {
            // Agent not running — plain message, no interjection semantics.
            debug_log(&format!(
                "inject_message: no running agent for session={}, stored only",
                session_id
            ));
        }
    }

    // 2. Append to the session store.
    let stored_content = intervention_stored_content(agent_running, &text);
    {
        let mut store = lock_poison_guard(&state.sessions);
        if let Some(entry) = store.get_mut(&session_id) {
            entry.messages.push(serde_json::json!({
                "role": "user",
                "content": stored_content,
                "timestamp": chrono::Utc::now().to_rfc3339(),
            }));
        }
        store.save();
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

/// Does a pending reminder survive the end of a run in `session_id`?
///
/// Durable reminders (persist=true) survive by design: "remind me in 30
/// minutes" must outlive the task that asked.
///
/// A not-yet-due entry is a pending *intent*, not a leftover of the finished
/// run. Dropping it killed every periodic task before its first fire — the
/// agent schedules the heartbeat (fire_at = now + interval), the run it was
/// scheduled from ends seconds later, and the heartbeat died with it, so
/// "report the download progress every 5 minutes" never reported once (user
/// report 2026-09-22). Overdue entries are still dropped: those are the
/// finished task's leftovers the 2026-09-03 fix was about (a dead task's
/// heartbeat must not keep firing).
///
/// `aborted` (user pressed Stop, or the run failed) kills the whole schedule:
/// the task is over, so its heartbeats must not keep waking the agent.
pub(crate) fn reminder_survives_run_end(
    r: &oz_core_types::Reminder,
    session_id: &str,
    now_ms: u64,
    aborted: bool,
) -> bool {
    r.session_id != session_id || r.persist || (!aborted && r.fire_at_ms > now_ms)
}

/// Drop a session's overdue scheduled/heartbeat reminders and tell the UI.
/// Reminders are scoped to the task that created them: when the run ends
/// (completed, stopped, or errored) its `schedule_reminder` entries must
/// not keep firing in the background — otherwise a finished task's
/// heartbeat keeps emitting `[Reminder]` events forever and the right-rail
/// cards stay "运行中" (user report 2026-09-03). Entries whose fire time is
/// still in the future survive: they are the periodic task itself (see
/// `reminder_survives_run_end`).
pub(crate) fn clear_session_reminders(
    state: &Arc<AppState>,
    app: &AppHandle,
    session_id: &str,
    aborted: bool,
) {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0);
    let removed = {
        let mut pending = lock_poison_guard(&state.pending_reminders);
        let before = pending.len();
        pending.retain(|r| reminder_survives_run_end(r, session_id, now, aborted));
        before - pending.len()
    };
    if removed > 0 {
        debug_log(&format!(
            "clear_session_reminders: dropped {} pending reminder(s) for session={}",
            removed, session_id
        ));
        // Only tell the UI to clear the right-rail cards when the backend
        // actually dropped them — a surviving pending reminder is still
        // scheduled and must keep its card.
        let _ = app.emit(
            "sse_event",
            serde_json::json!({
                "session_id": session_id,
                "event_type": "reminders_cleared",
                "data": "{}",
            }),
        );
    }
}

/// Outcome of delivering a fired reminder (`deliver_fired_reminder`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ReminderDelivery {
    /// Nothing to do: the session is gone or the concurrency cap is reached.
    Skipped,
    /// A live run took the reminder as an intervention — no new run.
    Intervened,
    /// The reminder was persisted as a user turn; the caller must call
    /// `start_reminder_run` so the agent actually executes the task.
    WakeRun,
}

/// Half one of a fired reminder: hand the message to the session.
///
/// `schedule_reminder` promises the backend "manages the timer and triggers
/// the agent run when the delay expires" (crates/oz-tools/src/schedule_reminder.rs)
/// — but the fire path only emitted SSE events, so a periodic task
/// ("report the model download progress every 5 minutes") was accepted by the
/// tool and then never executed (user report 2026-09-22). This is the missing
/// half of that contract:
///
///   * a live run for the session receives the reminder as an intervention
///     (the loop picks it up before its next LLM turn), persisted with the
///     same `[USER INTERVENTION …]` shape as a manual interjection, so the
///     transcript shows the card inside the running bubble;
///   * otherwise the reminder is persisted as a plain user turn and the
///     caller starts a fresh run for it (`start_reminder_run`), which each
///     repeat re-arms.
///
/// Persisting happens here (not in `start_reminder_run`) so the caller can
/// emit the `reminder_fired` SSE event *before* the run's own stream events —
/// the webview needs to enter the live state first, or the run's first parts
/// are wiped when it switches bubbles.
pub(crate) fn deliver_fired_reminder(
    state: &Arc<AppState>,
    app: &AppHandle,
    session_id: &str,
    message: &str,
) -> ReminderDelivery {
    let content = format!("[Reminder] {message}");

    // 1. A live run owns the session → queue it as an intervention.
    {
        let queues = lock_poison_guard(&state.intervention_queues);
        if let Some(queue) = queues.get(session_id) {
            lock_poison_guard(queue).push_back(oz_core::checkpoint::InterventionEvent {
                id: uuid::Uuid::new_v4().to_string(),
                timestamp: chrono::Utc::now().timestamp() as f64,
                kind: oz_core::checkpoint::InterventionKind::InjectInfo,
                content: content.clone(),
            });
        } else {
            // Released before the store/mutex chain below (same ordering as
            // inject_message).
            drop(queues);
            return start_reminder_wake(state, app, session_id, &content);
        }
    }
    // Persist the queued intervention so the transcript matches what the
    // running agent sees (mirrors inject_message).
    let stored = intervention_stored_content(true, &content);
    {
        let mut store = lock_poison_guard(&state.sessions);
        if let Some(entry) = store.get_mut(session_id) {
            entry.messages.push(serde_json::json!({
                "role": "user",
                "content": stored,
                "timestamp": chrono::Utc::now().to_rfc3339(),
            }));
            store.save();
        }
    }
    debug_log(&format!(
        "deliver_fired_reminder: queued as intervention session={session_id}"
    ));
    ReminderDelivery::Intervened
}

/// Wake half: persist the reminder as the run's trigger turn and report
/// whether the caller must start a run for it.
fn start_reminder_wake(
    state: &Arc<AppState>,
    app: &AppHandle,
    session_id: &str,
    content: &str,
) -> ReminderDelivery {
    // Same single-flight/cap gate as send_message: a run that appeared while
    // the caller was still emitting its SSE events still wins.
    {
        let agents = lock_poison_guard(&state.running_agents);
        if agents.contains_key(session_id) {
            return ReminderDelivery::Intervened;
        }
        if agents.len() >= 3 {
            debug_log(&format!(
                "deliver_fired_reminder: concurrency cap reached, dropping session={session_id}"
            ));
            return ReminderDelivery::Skipped;
        }
    }

    // Persist the reminder as the run's trigger turn. It is a real user turn
    // (no intervention prefix): there is no live bubble to fold it into, and
    // the frontend renders the same bubble optimistically.
    {
        let mut store = lock_poison_guard(&state.sessions);
        let Some(entry) = store.get_mut(session_id) else {
            debug_log(&format!(
                "deliver_fired_reminder: session {session_id} not found, dropping reminder"
            ));
            return ReminderDelivery::Skipped;
        };
        entry.status = SessionStatus::Running;
        entry.messages.push(serde_json::json!({
            "role": "user",
            "content": content,
            "timestamp": chrono::Utc::now().to_rfc3339(),
        }));
        store.save();
    }
    let _ = app.emit(
        "sse_event",
        serde_json::json!({
            "type": "protocol_v1",
            "data": { "type": "user_message_stored", "session_id": session_id }
        }),
    );
    debug_log(&format!(
        "deliver_fired_reminder: stored reminder turn session={session_id}"
    ));
    ReminderDelivery::WakeRun
}

/// Half two of a fired reminder: run the agent for a session whose trigger
/// turn `deliver_fired_reminder` just persisted. Mirrors the `send_message`
/// spawn (RAII guard + inner panic isolation) so a reminder-driven run cannot
/// wedge the session in "Running" or leak its JoinHandle.
pub(crate) fn start_reminder_run(state: &Arc<AppState>, app: &AppHandle, session_id: &str) {
    abort_detached_agent(session_id, state);
    {
        let agents = lock_poison_guard(&state.running_agents);
        if agents.contains_key(session_id) {
            return;
        }
        if agents.len() >= 3 {
            return;
        }
    }

    let state_clone: Arc<AppState> = state.clone();
    let app_clone = app.clone();
    let session_id_clone = session_id.to_string();
    let handle = tokio::spawn(async move {
        let _cleanup = AgentSessionGuard {
            state: state_clone.clone(),
            session_id: session_id_clone.clone(),
        };
        let inner_session = session_id_clone.clone();
        let inner_state = state_clone.clone();
        let inner_app = app_clone.clone();
        let inner = tokio::spawn(async move {
            runner::run_agent_for_session(
                &inner_app,
                &inner_state,
                &inner_session,
                None,
                false,
                None,
                None,
            )
            .await
        });
        match inner.await {
            Ok(Ok(())) => {}
            Ok(Err(e)) => {
                debug_log(&format!("reminder run_agent error: {e}"));
                emit_agent_run_error(&state_clone, &app_clone, &session_id_clone, &e.to_string());
            }
            Err(join_err) => {
                let msg = if join_err.is_panic() {
                    "agent task panicked (see openzen.log for the backtrace)".to_string()
                } else {
                    format!("agent task cancelled: {join_err}")
                };
                debug_log(&msg);
                emit_agent_run_error(&state_clone, &app_clone, &session_id_clone, &msg);
            }
        }
    });
    lock_poison_guard(&state.running_agents).insert(session_id.to_string(), handle);
    debug_log(&format!(
        "start_reminder_run: spawned agent run session={session_id}"
    ));
}

/// Unified run-failure path: reset Running → Idle in the store and emit an
/// error SSE event so the frontend's isProcessing clears. Used by the
/// send/regenerate/resume spawn wrappers for both `Err` returns and panics.
fn emit_agent_run_error(state: &Arc<AppState>, app: &AppHandle, session_id: &str, msg: &str) {
    // Safety net: an early error return (config parse, session missing, panic
    // mid-loop, …) skips the runner's own status writeback — reset
    // Running → Idle here so the UI can't be stuck on "Running".
    {
        let mut store = lock_poison_guard(&state.sessions);
        if let Some(s) = store.get_mut(session_id) {
            if s.status == SessionStatus::Running || s.status == SessionStatus::Waiting {
                // A failed run that was parked on a question still owns a
                // resumable state; keep it. A plain run failure goes Idle.
                s.status = if s.pending_ask_user.is_some() {
                    SessionStatus::Waiting
                } else {
                    SessionStatus::Idle
                };
            }
        }
        store.save();
    }
    let _ = app.emit(
        "sse_event",
        serde_json::to_value(SseEvent::error(session_id, msg)).unwrap_or_default(),
    );
    // The task is dead — its whole schedule dies with it.
    clear_session_reminders(state, app, session_id, true);
}

/// Gracefully stop a running agent: signal → wait → detach if unresponsive.
/// Never force-aborts — the stop signal causes the agent loop to exit cleanly,
/// and `after_run` must execute to persist messages.
/// Cascade a Stop into the child agents this conversation spawned. Children
/// are separate sessions, so their loops have their own stop signals.
fn stop_child_agents(parent: &str, state: &Arc<AppState>) -> usize {
    let children = lock_poison_guard(&state.child_sessions)
        .get(parent)
        .cloned()
        .unwrap_or_default();
    let signals = lock_poison_guard(&state.stop_signals);
    let mut stopped = 0;
    for c in &children {
        if let Some(sig) = signals.get(c) {
            sig.store(true, std::sync::atomic::Ordering::SeqCst);
            stopped += 1;
        }
    }
    stopped
}

async fn stop_running_agent(session_id: &str, state: &Arc<AppState>) {
    {
        let map = lock_poison_guard(&state.stop_signals);
        if let Some(sig) = map.get(session_id) {
            sig.store(true, std::sync::atomic::Ordering::SeqCst);
        }
    }
    // Wait up to 10s for graceful exit via stop_signal. With the tool-level
    // stop poll fixed (agent_loop), an in-flight tool now returns
    // "interrupted by user stop" within ~100ms, so this window is normally
    // enough. If it is not, we still detach — the task keeps unwinding on
    // its own and a new run aborts it — but we say so loudly instead of
    // pretending the session stopped.
    for _ in 0..100 {
        {
            let agents = lock_poison_guard(&state.running_agents);
            if !agents.contains_key(session_id) {
                return;
            }
        } // MutexGuard dropped before await
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    }
    // Still running after 10s — detach, don't abort mid-write.
    // The task will finish naturally when stop_signal takes effect.
    // Move the handle to detached_agents: the task keeps running, but a
    // new run for this session will abort it (see abort_detached_agent)
    // so two agents can never write the same session concurrently.
    let handle = lock_poison_guard(&state.running_agents).remove(session_id);
    if let Some(handle) = handle {
        lock_poison_guard(&state.detached_agents).insert(session_id.to_string(), handle);
        tracing::warn!(
            "[stop] session={session_id} did not acknowledge stop within 10s; \
             detached (still finishing — not aborted to avoid a half-written file)"
        );
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
        lock_poison_guard(&self.state.stop_signals).remove(&self.session_id);
        lock_poison_guard(&self.state.ask_user_rxs).remove(&self.session_id);
    }
}

#[tauri::command]
pub async fn send_message(
    message: String,
    session_id: String,
    session_name: Option<String>,
    model_name: Option<String>,
    reasoning_effort: Option<String>,
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
    let reasoning_effort_clone = reasoning_effort.clone();

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
        // Panic isolation: run the loop in an inner task so a panic (e.g. a
        // byte-slice on CJK user input) still reaches the frontend as an
        // error SSE event. Without this the UI stays isProcessing forever —
        // no new cards ever render and the stop pill never flips back
        // (observed 2026-09-02: intervention with Chinese content panicked
        // agent_loop and the session silently died).
        let inner_session = session_id_clone.clone();
        let inner_state = state_clone.clone();
        let inner_app = app_clone.clone();
        let inner_model = model_name_clone.clone();
        let inner_effort = reasoning_effort_clone.clone();
        let inner = tokio::spawn(async move {
            runner::run_agent_for_session(
                &inner_app,
                &inner_state,
                &inner_session,
                inner_model.as_deref(),
                false,
                inner_effort.as_deref(),
                None,
            )
            .await
        });
        match inner.await {
            Ok(Ok(())) => {}
            Ok(Err(e)) => {
                debug_log(&format!("run_agent error: {e}"));
                emit_agent_run_error(&state_clone, &app_clone, &session_id_clone, &e.to_string());
            }
            Err(join_err) => {
                let msg = if join_err.is_panic() {
                    "agent task panicked (see openzen.log for the backtrace)".to_string()
                } else {
                    format!("agent task cancelled: {join_err}")
                };
                debug_log(&msg);
                emit_agent_run_error(&state_clone, &app_clone, &session_id_clone, &msg);
            }
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
        // RAII cleanup — runs even if run_agent_for_session panics.
        let _cleanup = AgentSessionGuard {
            state: state_clone.clone(),
            session_id: sid.clone(),
        };
        // Panic isolation (see send_message): a panicking loop must still
        // clear the frontend's isProcessing via an error event.
        let inner = tokio::spawn({
            let app = app_clone.clone();
            let state = state_clone.clone();
            let sid = sid.clone();
            async move {
                runner::run_agent_for_session(&app, &state, &sid, None, false, None, None).await
            }
        });
        match inner.await {
            Ok(Ok(())) => {}
            Ok(Err(e)) => {
                debug_log(&format!("regenerate agent error: {e}"));
                emit_agent_run_error(&state_clone, &app_clone, &sid, &e.to_string());
            }
            Err(join_err) => {
                let msg = if join_err.is_panic() {
                    "agent task panicked (see openzen.log for the backtrace)".to_string()
                } else {
                    format!("agent task cancelled: {join_err}")
                };
                debug_log(&msg);
                emit_agent_run_error(&state_clone, &app_clone, &sid, &msg);
            }
        }
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
            s.pending_ask_user = None;
        }
        store.save();
    }

    let resume_note = lock_poison_guard(&state.resume_notes).remove(&session_id);
    let session_id = session_id.clone();
    let state_clone: Arc<AppState> = state.clone();
    let app_clone = app_handle.clone();
    let sid = session_id.clone();

    let handle = tokio::spawn(async move {
        // RAII cleanup — runs even if run_agent_for_session panics.
        let _cleanup = AgentSessionGuard {
            state: state_clone.clone(),
            session_id: sid.clone(),
        };
        // Panic isolation (see send_message): a panicking loop must still
        // clear the frontend's isProcessing via an error event.
        let inner = tokio::spawn({
            let app = app_clone.clone();
            let state = state_clone.clone();
            let sid = sid.clone();
            let model = model_name.clone();
            let note = resume_note.clone();
            async move {
                runner::run_agent_for_session(
                    &app,
                    &state,
                    &sid,
                    model.as_deref(),
                    true,
                    None,
                    note.as_deref(),
                )
                .await
            }
        });
        match inner.await {
            Ok(Ok(())) => {}
            Ok(Err(e)) => {
                debug_log(&format!("resume_agent error: {e}"));
                emit_agent_run_error(&state_clone, &app_clone, &sid, &e.to_string());
            }
            Err(join_err) => {
                let msg = if join_err.is_panic() {
                    "agent task panicked (see openzen.log for the backtrace)".to_string()
                } else {
                    format!("agent task cancelled: {join_err}")
                };
                debug_log(&msg);
                emit_agent_run_error(&state_clone, &app_clone, &sid, &msg);
            }
        }
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
    tool_use_id: Option<String>,
    app_handle: AppHandle,
    state: State<'_, Arc<AppState>>,
) -> Result<serde_json::Value, String> {
    let pending = {
        let store = lock_poison_guard(&state.sessions);
        let Some(entry) = store.get(&session_id) else {
            return Err(format!("Session {session_id} not found"));
        };
        entry.pending_ask_user.clone()
    };
    let slot_opt = lock_poison_guard(&state.ask_user_rxs)
        .get(&session_id)
        .cloned();
    let Some(slot) = slot_opt else {
        // The run that asked is gone (app restart / crash). Turn the reply
        // into a one-shot resume note: the checkpoint restores the task and
        // the note carries the question + answer the dead loop never saw.
        let Some(pending) = pending else {
            return Err(format!(
                "Session {session_id} has no live agent and no parked ask_user \
                 question; send a new message to continue"
            ));
        };
        let lang = lock_poison_guard(&state.locale).clone();
        let note = if lang == "zh" {
            format!(
                "[用户答复 · 应用重启后送达]\n你之前向用户提问：\n{}\n\n用户回答：\n{}\n\n\
                 请基于这个回答继续原任务；不要重复提问。",
                pending.question, response
            )
        } else {
            format!(
                "[USER REPLY · delivered after an app restart]\nYou previously asked:\n{}\n\n\
                 The user answered:\n{}\n\nContinue the original task with this answer; \
                 do not ask the same question again.",
                pending.question, response
            )
        };
        lock_poison_guard(&state.resume_notes).insert(session_id.clone(), note);
        {
            let mut store = lock_poison_guard(&state.sessions);
            if let Some(entry) = store.get_mut(&session_id) {
                entry.pending_ask_user = None;
                entry.status = SessionStatus::Stopped;
            }
            store.save();
        }
        let _ = app_handle.emit(
            "sse_event",
            serde_json::to_value(SseEvent::system(
                &session_id,
                "ask_user reply stored; resuming from checkpoint",
            ))
            .unwrap_or_default(),
        );
        return Ok(serde_json::json!({ "received": true, "resume": true }));
    };
    // P1-i: key by the question's tool_use_id when the caller has it;
    // otherwise the persisted pending id (page reload) keeps the reply on
    // the right question; the legacy key is the last resort.
    let key = tool_use_id
        .or_else(|| pending.map(|p| p.tool_use_id))
        .unwrap_or_else(|| "__last__".to_string());
    lock_poison_guard(&slot).insert(key, response);
    // Leave the waiting state immediately: the dialog is gone and the
    // loop is about to stream again. The AskUserAnswered event will do the
    // same for IM replies, which never reach this command.
    {
        let mut store = lock_poison_guard(&state.sessions);
        if let Some(entry) = store.get_mut(&session_id) {
            entry.status = SessionStatus::Running;
            entry.pending_ask_user = None;
        }
        store.save();
    }
    let _ = app_handle.emit(
        "sse_event",
        serde_json::to_value(SseEvent::system(
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
    model: Option<String>,
    state: State<'_, Arc<AppState>>,
    app_handle: AppHandle,
) -> Result<serde_json::Value, String> {
    let (compaction, removed_json) = {
        let mut store = lock_poison_guard(&state.sessions);
        let entry = match store.get_mut(&id) {
            Some(e) => e,
            None => return Err(format!("Session {id} not found")),
        };

        let comp_config = oz_core::CompressionConfig::default();
        // Manual /compact is a user-invoked "force" action: it must fold
        // old turns into a summary regardless of how full the context
        // window is. Passing context_win=1 bypasses the trigger threshold
        // (same trick emergency_compress uses) so compression always runs
        // down to the min_messages floor instead of no-oping on small
        // sessions. The auto-compress path in the agent loop still uses
        // the real context window from config.
        //
        // The pass runs on the same view the agent loop rebuilds for the
        // LLM (text + tool_use/tool_result traffic). The previous
        // content-only view ignored `tool_results`/`tool_use_blocks`, where
        // a real session keeps the bulk of its context (measured: 24K chars
        // of text against 665K chars of tool output in one 30-message
        // session) — so /compact measured ~1.4K chars on a 100K+ token
        // conversation, freed nothing, and still reported success (user
        // report 2026-09-22).
        let compaction = oz_core::compact_store_messages(&entry.messages, 1, &comp_config);
        let removed_json = compaction.removed.clone();
        entry.messages = compaction.messages.clone();
        store.save();
        (compaction, removed_json)
    };

    let before = compaction.before_messages;
    let after = compaction.after_messages;
    let before_chars = compaction.before_chars;
    let after_chars = compaction.after_chars;
    let saved_chars = compaction.saved_chars();
    let saved_pct = compaction.saved_pct();
    let messages_removed = compaction.removed.len();
    let metrics =
        oz_core::compress::CompressionMetrics::compute(before_chars, after_chars, before, after);
    let template_summary = oz_core::compress::build_compression_summary(&removed_json, "");

    let lang = lock_poison_guard(&state.locale).clone();
    // The LLM summary is what makes /compact a compression instead of a
    // truncation — and the only place the requested model is used. The old
    // `messages_removed >= 4` gate skipped it (and the `-model` the user
    // passed) whenever fewer than four whole messages were dropped, even
    // though the pass had just folded hundreds of KB of tool traffic and
    // reported "(template)" with 0 tokens freed. Run it whenever the pass
    // actually freed something.
    let llm_summary = if saved_chars > 0 {
        generate_compact_summary(&state, &template_summary, &lang, model.as_deref()).await
    } else {
        None
    };

    // The removed window is already gone from the store (deleted above), so
    // whatever we write here is the ONLY record of it. Two rules:
    //   1. never leave the store without a summary — when the LLM summary is
    //      unavailable (small local model, gateway timeout, 600s budget
    //      exhausted) fall back to the deterministic template instead of
    //      silently dropping the whole window (round5 P0-9);
    //   2. keep a copy of the removed messages on disk so a bad compression
    //      can be inspected/recovered by hand.
    if saved_chars > 0 {
        let (summary_text, used_llm) = match llm_summary {
            Some((ref s, _)) if !s.trim().is_empty() => (s.clone(), true),
            _ => (template_summary.clone(), false),
        };
        let dump_dir = crate::data_dir().join("openzen");
        let _ = std::fs::create_dir_all(&dump_dir);
        {
            let stamp = chrono::Local::now().format("%Y%m%d-%H%M%S");
            let path = dump_dir.join(format!("compact_removed_{stamp}.json"));
            if let Ok(json) = serde_json::to_string_pretty(&removed_json) {
                match std::fs::write(&path, json) {
                    Ok(()) => debug_log(&format!(
                        "compact: removed window dumped to {} ({} messages)",
                        path.display(),
                        removed_json.len()
                    )),
                    Err(e) => tracing::warn!("compact: failed to dump removed window: {e}"),
                }
            }
            rotate_compact_dumps(&dump_dir, 3, 20 * 1024 * 1024);
        }
        let header = if used_llm {
            "[Compression summary]"
        } else {
            "[Compression summary (template — LLM summary unavailable)]"
        };
        let mut store = lock_poison_guard(&state.sessions);
        if let Some(entry) = store.get_mut(&id) {
            entry.messages.insert(
                0,
                serde_json::json!({
                    "role": "system",
                    "content": format!("{header}: {summary_text}"),
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
    let strategy = if saved_chars == 0 && llm_summary.is_none() {
        // Nothing was foldable: say so instead of reporting a successful
        // compression that released 0 tokens.
        format!(
            "nothing to compress: {} messages / {} tokens already inside the keep window",
            before, before_tokens
        )
    } else {
        format!(
            "compressed {}→{} messages, saved {:.1}% tokens{}",
            before,
            after,
            saved_pct,
            match &llm_summary {
                Some((_, label)) => format!(" (LLM summary via {label})"),
                None => " (template)".to_string(),
            }
        )
    };
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
        "changed": saved_chars > 0,
        "metrics": metrics.summary(),
        "summary": template_summary,
        "llm_summary": llm_summary.as_ref().map(|(s, _)| s.clone()),
        "llm_model": llm_summary.as_ref().map(|(_, m)| m.clone()),
        "strategy": strategy,
    }))
}

/// Resolve a session entry by its section name or by its `model` field,
/// case-insensitively on both — users type `/compact -model lfm2.5-230m`
/// for an entry whose `model = "LFM2.5-230M"`.
fn resolve_entry(
    cfg: &MyKeyConfig,
    needle: &str,
) -> Option<(String, oz_config::mykey::SessionConfig)> {
    let n = needle.trim().to_lowercase();
    if n.is_empty() {
        return None;
    }
    if let Some((k, v)) = cfg.sessions.iter().find(|(k, _)| k.to_lowercase() == n) {
        return Some((k.clone(), v.clone()));
    }
    cfg.sessions
        .iter()
        .find(|(_, s)| s.model.to_lowercase() == n)
        .map(|(k, v)| (k.clone(), v.clone()))
}

/// Summarize the folded conversation with an LLM (structured markdown, same
/// instruction as the agent loop's auto-compression). Model resolution:
///   1. `requested` — the session's selected model, or `/compact -model X`;
///   2. `summary_model` from mykey.toml (the legacy behavior, kept for
///      callers that pass no model);
///   3. `default_session`.
///
/// Returns `(summary, model label)`, or `None` when nothing resolves or the
/// call fails — in which case the caller keeps the deterministic template.
async fn generate_compact_summary(
    state: &AppState,
    template: &str,
    lang: &str,
    requested: Option<&str>,
) -> Option<(String, String)> {
    let config_path = state.config_path.clone();
    let cfg = oz_config::mykey::MyKeyConfig::from_file(std::path::Path::new(&config_path)).ok()?;
    let resolved = requested
        .and_then(|r| resolve_entry(&cfg, r))
        .or_else(|| {
            cfg.summary_model
                .as_deref()
                .and_then(|m| resolve_entry(&cfg, m))
        })
        .or_else(|| {
            cfg.default_session
                .as_deref()
                .and_then(|d| resolve_entry(&cfg, d))
        })?;
    let (sess_name, sess_config) = resolved;
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
    let prompt = Message::user(format!(
        "{}\n\nDo NOT re-execute or continue the conversation — only summarize.\n\n---\n\n{template}",
        oz_core::compress::summary_instruction(lang)
    ));
    let msgs = [prompt];
    // Same small local summarizer as the agent loop's auto-compression needs
    // minutes for a large removed window — match `summary_wait_secs` (600s)
    // instead of a short timeout that silently degrades to the template.
    let result =
        tokio::time::timeout(std::time::Duration::from_secs(600), client.chat(&msgs, &[])).await;
    match result {
        Ok(Ok(resp)) if !resp.content.is_empty() => Some((resp.content, sess_config.model.clone())),
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
#[allow(clippy::too_many_arguments)]
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
        // Real checkpoint dir for the long-task session. Built from the
        // crate dir so the test tree carries no machine-specific absolute
        // path; the existence guard below still skips on CI.
        let repo = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
        let cp_dir = repo
            .join("tests/longtask/2/openzen/checkpoints")
            .to_string_lossy()
            .to_string();
        if !std::path::Path::new(&cp_dir).exists() {
            tracing::info!("skipping: real checkpoint dir not present");
            return;
        }
        let session_id = "fe54c2c0-4150-4db3-bdf4-086543a1ab1d";
        let working_dir = repo.join("tests/longtask/2").to_string_lossy().to_string();

        // load_best must pick the LATEST turn (033, turn 33), not an older
        // one with more messages.
        let loaded = oz_core::checkpoint::load_best_loop_checkpoint(
            std::path::Path::new(&cp_dir),
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
        recover_session_from_checkpoints(&mut store, session_id, &working_dir);

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

    /// A live interjection persists with the apply_intervention prefix so
    /// the web layer folds it into the preceding assistant bubble on
    /// reload (no separate user bubble) and the next run's history
    /// carries it verbatim.
    #[test]
    fn intervention_stored_content_uses_prefix_when_agent_running() {
        let stored = intervention_stored_content(true, "先跑完测试再部署");
        assert_eq!(
            stored,
            "[USER INTERVENTION - inject_info]\n先跑完测试再部署"
        );
        assert!(stored.starts_with("[USER INTERVENTION"));
        // The web layer's folding regex strips everything up to the first
        // newline — the user text must survive it untouched.
        let stripped = stored
            .split_once('\n')
            .map(|(_, rest)| rest)
            .unwrap_or(&stored);
        assert_eq!(stripped, "先跑完测试再部署");
    }

    #[test]
    fn intervention_stored_content_is_plain_when_agent_idle() {
        assert_eq!(intervention_stored_content(false, "普通消息"), "普通消息");
    }

    fn reminder(session: &str, fire_at_ms: u64, persist: bool) -> oz_core_types::Reminder {
        oz_core_types::Reminder {
            session_id: session.to_string(),
            message: "report".into(),
            fire_at_ms,
            repeat_count: 0,
            repeat_interval_secs: 300,
            persist,
        }
    }

    /// The core of the "定时任务一直没有成功运行" fix: a heartbeat scheduled
    /// for the future must outlive the run that created it, while a finished
    /// (or stopped) task's overdue leftovers must not.
    #[test]
    fn reminder_survives_run_end_rules() {
        let now = 1_000_000u64;
        let sid = "s1";
        // Future + run-scoped + normal finish → survives (this is the
        // periodic task; dropping it was the bug).
        assert!(reminder_survives_run_end(
            &reminder(sid, now + 300_000, false),
            sid,
            now,
            false
        ));
        // Overdue leftover of a finished run → dropped (2026-09-03 rule).
        assert!(!reminder_survives_run_end(
            &reminder(sid, now - 1, false),
            sid,
            now,
            false
        ));
        // User pressed Stop / run failed → the whole schedule dies.
        assert!(!reminder_survives_run_end(
            &reminder(sid, now + 300_000, false),
            sid,
            now,
            true
        ));
        // Durable reminders always survive.
        assert!(reminder_survives_run_end(
            &reminder(sid, now - 1, true),
            sid,
            now,
            true
        ));
        // Another session's reminders are never touched here.
        assert!(reminder_survives_run_end(
            &reminder("other", now - 1, false),
            sid,
            now,
            true
        ));
    }

    fn mig_table(body: &str) -> toml::Table {
        body.parse().unwrap()
    }

    #[test]
    fn migration_needed_only_for_duplicated_credentials() {
        let dup = mig_table(
            r#"
[a]
apikey = "sk-1"
apibase = "http://h:8000/v1"
model = "m1"

[b]
apikey = "sk-1"
apibase = "http://h:8000/v1"
model = "m2"
"#,
        );
        assert!(needs_provider_migration(&dup));

        let unique = mig_table(
            r#"
[a]
apikey = "sk-1"
apibase = "http://h:8000/v1"
model = "m1"
"#,
        );
        assert!(!needs_provider_migration(&unique));

        let with_providers = mig_table(
            r#"
[providers.p]
apikey = "sk-1"
apibase = "http://h:8000/v1"

[a]
provider = "p"
model = "m1"
"#,
        );
        assert!(!needs_provider_migration(&with_providers));
    }

    #[test]
    fn migration_merges_group_and_keeps_entry_fields() {
        let mut table = mig_table(
            r#"
default_session = "a"

[a]
apikey = "sk-1"
apibase = "http://127.0.0.1:8000/v1"
model = "m1"
context_win = 256000
max_tokens = 4096

[b]
apikey = "sk-1"
apibase = "http://127.0.0.1:8000/v1"
model = "m2"

[c]
apikey = "sk-other"
apibase = "http://other:9000/v1"
model = "m3"
"#,
        );
        apply_provider_migration(&mut table).unwrap();

        let providers = table["providers"].as_table().unwrap();
        assert_eq!(providers.len(), 1, "only the duplicated pair merges");
        let pid = providers.keys().next().unwrap();
        assert_eq!(
            providers[pid]["apibase"].as_str(),
            Some("http://127.0.0.1:8000/v1")
        );

        let a = table["a"].as_table().unwrap();
        assert_eq!(a["provider"].as_str().unwrap(), pid, "entry gains the ref");
        assert!(a.get("apibase").is_none() && a.get("apikey").is_none());
        assert_eq!(a["model"].as_str(), Some("m1"));
        assert_eq!(
            a["context_win"].as_integer(),
            Some(256000),
            "private fields kept"
        );
        assert_eq!(a["max_tokens"].as_integer(), Some(4096));

        let b = table["b"].as_table().unwrap();
        assert_eq!(b["provider"].as_str().unwrap(), pid);

        // Single-entry credentials stay inline.
        let c = table["c"].as_table().unwrap();
        assert!(c.get("provider").is_none());
        assert_eq!(c["apibase"].as_str(), Some("http://other:9000/v1"));

        assert_eq!(
            table["default_session"].as_str(),
            Some("a"),
            "names unchanged"
        );
    }

    #[test]
    fn migration_is_idempotent() {
        let mut table = mig_table(
            r#"
[a]
apikey = "sk-1"
apibase = "http://h:8000/v1"
model = "m1"

[b]
apikey = "sk-1"
apibase = "http://h:8000/v1"
model = "m2"
"#,
        );
        apply_provider_migration(&mut table).unwrap();
        let once = table.clone();
        apply_provider_migration(&mut table).unwrap();
        assert_eq!(once, table, "second pass must not change anything");
    }

    #[test]
    fn provider_id_derivation_sanitizes_and_dedups() {
        let taken = toml::Table::new();
        assert_eq!(
            derive_provider_id("http://127.0.0.1:8000/v1", &taken),
            "127_0_0_1_8000"
        );
        assert_eq!(
            derive_provider_id("https://api.example.com/v1", &taken),
            "api_example_com"
        );
        assert_eq!(derive_provider_id("::::", &taken), "provider");

        let mut taken = toml::Table::new();
        taken.insert("h".to_string(), toml::Value::Table(toml::Table::new()));
        taken.insert("h_1".to_string(), toml::Value::Table(toml::Table::new()));
        assert_eq!(
            derive_provider_id("http://h:1/x", &taken),
            "h_1_2",
            "colliding id gets a numeric suffix"
        );
    }

    #[test]
    fn orphan_collection_covers_dotted_names_and_skips_config_tables() {
        let table = mig_table(
            r#"
[providers.p]
apibase = "http://h:8000/v1"

[plain]
provider = "p"
model = "m1"

["qwen3.6-27b"]
provider = "p"
model = "m2"

[web_search]
provider = "p"

[other]
provider = "different"
model = "m3"
"#,
        );
        let mut orphans = Vec::new();
        collect_provider_orphans(&table, "p", "", &mut orphans);
        orphans.sort();
        assert_eq!(
            orphans,
            vec!["plain".to_string(), "qwen3.6-27b".to_string()],
            "dotted entries are collected; provider-less-shape config tables are not"
        );
    }

    #[test]
    fn resolve_entry_matches_name_or_model_field_case_insensitively() {
        let cfg = {
            let path = std::env::temp_dir().join("oz_cmd_test_resolve_entry.toml");
            std::fs::write(
                &path,
                r#"
[LFM2_5_230M]
apikey = "sk-test"
apibase = "http://127.0.0.1:8000/v1"
model = "LFM2.5-230M"

[big_model]
apikey = "sk-test"
apibase = "http://127.0.0.1:8000/v1"
model = "Qwen3-Coder-30B"
"#,
            )
            .unwrap();
            let cfg = MyKeyConfig::from_file(&path).unwrap();
            let _ = std::fs::remove_file(&path);
            cfg
        };

        // model-field match, case-insensitive (the `/compact -model lfm2.5-230m` case)
        let (name, sc) = resolve_entry(&cfg, "lfm2.5-230m").expect("model field match");
        assert_eq!(name, "LFM2_5_230M");
        assert_eq!(sc.model, "LFM2.5-230M");
        // exact section-name match wins
        assert_eq!(resolve_entry(&cfg, "big_model").unwrap().0, "big_model");
        // section name, case-insensitive
        assert_eq!(resolve_entry(&cfg, "BIG_MODEL").unwrap().0, "big_model");
        // unknown / empty resolve to None
        assert!(resolve_entry(&cfg, "nope").is_none());
        assert!(resolve_entry(&cfg, "   ").is_none());

        // The shipped instruction is shared with the auto-compression path.
        assert!(oz_core::compress::summary_instruction("en").contains("REQUIRED SECTIONS"));
        assert!(oz_core::compress::summary_instruction("zh").contains("必需段落"));
    }

    #[test]
    fn remove_entry_by_dotted_name_prunes_empty_parents() {
        let mut table = mig_table(
            r#"
["qwen3.6-27b"]
provider = "p"
model = "m2"

[keep]
apibase = "http://x/v1"
model = "m"
"#,
        );
        remove_entry_by_dotted_name(&mut table, "qwen3.6-27b");
        assert!(
            !table.contains_key("qwen3"),
            "empty parent table must be pruned, not left as [qwen3]"
        );
        assert!(table.contains_key("keep"));

        // Removing a non-existent dotted name is a no-op.
        remove_entry_by_dotted_name(&mut table, "nope.missing");
        assert!(table.contains_key("keep"));
    }
}

// ── Background jobs (UI surface) ──────────────────────────────────────────
//
// The model-facing tools live in `oz-tools`; these commands only read/kill
// jobs for the chat UI. Ownership is still fenced by the session id.

#[tauri::command]
pub fn job_list(session_id: String) -> serde_json::Value {
    let jobs = oz_core::jobs::global().list(&session_id);
    serde_json::json!({ "jobs": jobs.iter().map(|j| j.to_json()).collect::<Vec<_>>() })
}

#[tauri::command]
pub async fn job_output(
    session_id: String,
    job_id: String,
    wait: Option<bool>,
    timeout_ms: Option<u64>,
) -> Result<serde_json::Value, String> {
    let registry = oz_core::jobs::global();
    let snap = registry
        .get(&job_id)
        .filter(|s| s.owner_session == session_id)
        .ok_or_else(|| format!("job {job_id} not found"))?;
    if wait.unwrap_or(false) && !snap.status.is_terminal() {
        registry
            .wait(
                &job_id,
                &session_id,
                std::time::Duration::from_millis(timeout_ms.unwrap_or(30_000).min(300_000)),
            )
            .await?;
    }
    let (text, snap) = registry.read(&job_id, &session_id)?;
    if snap.status.is_terminal() {
        registry.mark_reported(&job_id);
    }
    Ok(serde_json::json!({ "text": text, "job": snap.to_json() }))
}

#[tauri::command]
pub async fn job_kill(session_id: String, job_id: String) -> Result<serde_json::Value, String> {
    let registry = oz_core::jobs::global();
    let requested = registry.cancel(&job_id, &session_id, None)?;
    let snap = if requested.status.is_terminal() {
        requested
    } else {
        registry
            .wait(&job_id, &session_id, std::time::Duration::from_secs(15))
            .await?
    };
    registry.mark_reported(&job_id);
    Ok(serde_json::json!({ "job": snap.to_json() }))
}

// ── Subagent transcript surface (P3) ─────────────────────────────────────

#[tauri::command]
pub fn agent_list(parent_session: String) -> serde_json::Value {
    serde_json::json!({ "agents": crate::subagents::list_children(&parent_session) })
}

#[tauri::command]
pub fn agent_read(child_id: String) -> Result<String, String> {
    crate::subagents::read_transcript(&child_id)
}

#[tauri::command]
pub fn agent_cancel(child_id: String, state: State<'_, Arc<AppState>>) -> Result<(), String> {
    // Cancellation is a stop-signal flip; the guard does the rest.
    if let Some(sig) = lock_poison_guard(&state.stop_signals).get(&child_id) {
        sig.store(true, std::sync::atomic::Ordering::SeqCst);
    }
    Ok(())
}
