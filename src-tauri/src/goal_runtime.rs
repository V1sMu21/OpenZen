//! Goal Mode runtime bridge. Goals are persisted in a dedicated SQLite DB and
//! driven through the same `send_message_core` path as ordinary chat, so Stop,
//! approvals, SSE and the running-agent guard keep working unchanged.

use std::sync::{Arc, OnceLock};

use tokio::sync::mpsc;

use serde_json::json;
use tauri::{AppHandle, Emitter, State};

use oz_steward::goal::{
    intent, GateRunner, GoalBudget, GoalGate, GoalGuardian, GoalRecord, GoalRuntime,
    GoalRuntimeConfig, GoalSpec, GoalStatus, GoalStore,
};
use oz_steward::report::{Report, ReportComposer};

use crate::{commands, lock_poison_guard, AppState};

struct GoalService {
    store: Arc<GoalStore>,
    runtime: GoalRuntime,
}

static SERVICE: OnceLock<Arc<GoalService>> = OnceLock::new();

struct Continuation {
    app: AppHandle,
    state: Arc<AppState>,
    session_id: String,
    goal_id: String,
    model_name: Option<String>,
}

static CONTINUE_TX: OnceLock<mpsc::UnboundedSender<Continuation>> = OnceLock::new();

fn service() -> Option<Arc<GoalService>> {
    SERVICE.get().cloned()
}

pub fn init() {
    if SERVICE.get().is_some() {
        return;
    }
    let path = crate::data_dir().join("goals.db");
    let store = match GoalStore::open(&path) {
        Ok(s) => Arc::new(s),
        Err(e) => {
            tracing::warn!("goal store unavailable: {e}");
            return;
        }
    };
    if let Err(e) = store.sweep_expired_leases() {
        tracing::warn!("goal lease sweep failed: {e}");
    }
    let runner_id = format!("desktop-{}-{}", std::process::id(), uuid::Uuid::new_v4());
    let runtime = GoalRuntime::new(store.clone(), runner_id).with_config(GoalRuntimeConfig {
        lease_ttl_ms: 30 * 60_000,
    });
    let _ = SERVICE.set(Arc::new(GoalService { store, runtime }));

    // One serialized continuation worker breaks the async type cycle:
    // run_agent → on_run_finished → queue → send_message_core → run_agent.
    // The hook only enqueues; it never directly calls send_message_core.
    let (tx, mut rx) = mpsc::unbounded_channel::<Continuation>();
    let _ = CONTINUE_TX.set(tx);
    tauri::async_runtime::spawn(async move {
        while let Some(req) = rx.recv().await {
            for _ in 0..60 {
                let busy =
                    lock_poison_guard(&req.state.running_agents).contains_key(&req.session_id);
                if !busy {
                    break;
                }
                tokio::time::sleep(std::time::Duration::from_millis(500)).await;
            }
            let Some(svc) = service() else { continue };
            let Ok(Some(goal)) = svc.store.get(&req.goal_id) else {
                continue;
            };
            let prompt = continuation_prompt(&svc, &goal);
            if let Err(e) = Box::pin(commands::send_message_core(
                prompt,
                req.session_id.clone(),
                None,
                req.model_name,
                None,
                req.app.clone(),
                req.state.clone(),
                false,
            ))
            .await
            {
                let _ = svc
                    .runtime
                    .pause(&goal.id, &format!("continuation failed: {e}"));
            }
        }
    });
}

fn find_for_session(svc: &GoalService, session_id: &str) -> Result<Option<GoalRecord>, String> {
    Ok(svc
        .store
        .list(100)
        .map_err(|e| e.to_string())?
        .into_iter()
        .find(|g| {
            g.spec.session_id == session_id
                && matches!(g.status, GoalStatus::Active | GoalStatus::Verifying)
        }))
}

fn prompt(goal: &GoalRecord, continuation: bool) -> String {
    let gates = goal
        .spec
        .gates
        .iter()
        .map(|g| format!("{g:?}"))
        .collect::<Vec<_>>()
        .join("\n");
    format!(
        "[Goal Mode] {}\n\n目标：{}\n\n验收 gate（运行时执行，你只能看到结果，不能修改）：\n{}\n\n预算：attempt {}/{}，tokens {}/{}。{}\n规则：\n- 不要只回复“完成了”；完成由 runtime gate 判定。\n- 失败时读 gate 输出，换策略，不要重复同一个调用。\n- 需要用户决定/权限时停下来，不要伪造证据。\n- 当前不是普通对话，而是 Goal attempt。",
        if continuation { "继续 attempt" } else { "开始目标" },
        goal.spec.objective,
        gates,
        goal.attempts + 1,
        goal.spec.budget.max_attempts,
        goal.tokens_used,
        goal.spec.budget.max_tokens,
        if goal.spec.auto_continue {
            "失败后自动下一 attempt。"
        } else {
            "失败后暂停，等用户继续。"
        }
    )
}

fn continuation_prompt(svc: &GoalService, goal: &GoalRecord) -> String {
    let last = svc
        .store
        .attempts(&goal.id, 1)
        .ok()
        .and_then(|a| a.into_iter().next());
    let failure = last
        .as_ref()
        .map(|a| {
            let detail = a
                .gates
                .iter()
                .filter(|g| !g.passed)
                .map(|g| {
                    format!(
                        "gate {}: exit={:?} {}",
                        g.gate_index,
                        g.exit_code,
                        g.output.chars().take(800).collect::<String>()
                    )
                })
                .collect::<Vec<_>>()
                .join("\n");
            format!(
                "上次 attempt {} 失败（{}）。\n{}",
                a.attempt_no,
                a.failure_class.as_deref().unwrap_or("gate_failed"),
                detail
            )
        })
        .unwrap_or_else(|| "上次 attempt 没有可读 gate 失败信息。".into());
    format!("{}\n\n{}", prompt(goal, true), failure)
}

fn stop_session(state: &Arc<AppState>, session_id: &str) {
    let mut map = lock_poison_guard(&state.stop_signals);
    if let Some(signal) = map.get(session_id) {
        signal.store(true, std::sync::atomic::Ordering::Relaxed);
    } else {
        map.insert(
            session_id.to_string(),
            Arc::new(std::sync::atomic::AtomicBool::new(true)),
        );
    }
}

fn workspace_for(state: &Arc<AppState>, session_id: &str) -> std::path::PathBuf {
    let stored = {
        let sessions = lock_poison_guard(&state.sessions);
        sessions.get(session_id).and_then(|s| s.working_dir.clone())
    };
    stored
        .map(std::path::PathBuf::from)
        .unwrap_or_else(crate::default_workspace_dir)
}

/// Automatic Goal Mode: promote a long-running/verification-loop task before
/// its first run. The same run becomes attempt 1; no manual create/start is
/// needed. Returns a system-reminder to prepend to the user message.
pub fn maybe_auto_start(_state: &Arc<AppState>, session_id: &str, message: &str) -> Option<String> {
    let svc = service()?;
    if svc.store.list(100).ok()?.iter().any(|g| {
        g.spec.session_id == session_id
            && !matches!(g.status, GoalStatus::Achieved | GoalStatus::Abandoned)
    }) {
        return None;
    }
    let intent = intent::detect(message);
    if !intent.should_goal {
        return None;
    }
    let id = uuid::Uuid::new_v4().to_string();
    let title = message
        .lines()
        .next()
        .unwrap_or("Goal")
        .chars()
        .take(60)
        .collect::<String>();
    let mut spec = GoalSpec::new(&id, title, message);
    spec.session_id = session_id.to_string();
    spec.gates = vec![intent.gate.clone()];
    spec.auto_continue = true;
    spec.require_human_acceptance = matches!(intent.gate, GoalGate::Human { .. });
    let goal = svc.store.create(spec).ok()?;
    if svc.runtime.start(&goal.id).is_err() {
        return None;
    }
    let _ = svc.store.append_event(
        &goal.id,
        "auto_start",
        &format!("auto Goal Mode: {}", intent.reason),
    );
    crate::steward::ensure_goal_ticket(
        &goal.id,
        session_id,
        &goal.spec.title,
        &goal.spec.objective,
        &goal.spec.constraints,
    );
    Some(format!(
        "[Goal Mode 自动开启] 检测到任务符合长任务/验收循环（{}）。\n目标：{}\n验收：{:?}\n完成由 runtime gate 判定；失败会自动下一个 bounded attempt。可用目标面板暂停/接管。",
        intent.reason, goal.spec.objective, goal.spec.gates
    ))
}

/// Write the Goal's evidence into the same Steward report table used by
/// ordinary tickets. A goal can only be Achieved when all gate evidence is
/// present; otherwise the unified report is Unverified.
fn sync_steward_report(svc: &GoalService, goal_id: &str) -> Result<Report, String> {
    let goal = svc
        .store
        .get(goal_id)
        .map_err(|e| e.to_string())?
        .ok_or_else(|| "goal not found".to_string())?;
    let gates = svc
        .store
        .attempts(goal_id, 1)
        .map_err(|e| e.to_string())?
        .first()
        .map(|a| a.gates.clone())
        .unwrap_or_default();
    let store =
        crate::steward::store_handle().ok_or_else(|| "steward store unavailable".to_string())?;
    ReportComposer::new(store)
        .compose_goal(
            &goal.id,
            &goal.spec.objective,
            goal.status,
            &gates,
            goal.attempts,
        )
        .map_err(|e| e.to_string())
}

#[tauri::command]
pub fn goal_report(goal_id: String) -> Result<Report, String> {
    let svc = service().ok_or_else(|| "goal store unavailable".to_string())?;
    sync_steward_report(&svc, &goal_id)
}

#[tauri::command]
pub fn goal_create(
    title: String,
    objective: String,
    session_id: String,
    gates: Vec<GoalGate>,
    budget: GoalBudget,
    auto_continue: bool,
    require_human_acceptance: bool,
) -> Result<GoalRecord, String> {
    let svc = service().ok_or_else(|| "goal store unavailable".to_string())?;
    let id = uuid::Uuid::new_v4().to_string();
    let mut spec = GoalSpec::new(&id, title, objective);
    spec.session_id = session_id.clone();
    spec.budget = budget;
    spec.auto_continue = auto_continue;
    spec.require_human_acceptance = require_human_acceptance;
    spec.gates = gates;
    let goal = svc.store.create(spec).map_err(|e| e.to_string())?;
    crate::steward::ensure_goal_ticket(
        &goal.id,
        &session_id,
        &goal.spec.title,
        &goal.spec.objective,
        &goal.spec.constraints,
    );
    Ok(goal)
}

#[tauri::command]
pub fn goal_list() -> Result<Vec<GoalRecord>, String> {
    service()
        .ok_or_else(|| "goal store unavailable".to_string())?
        .store
        .list(100)
        .map_err(|e| e.to_string())
}

#[tauri::command]
pub fn goal_get(goal_id: String) -> Result<Option<GoalRecord>, String> {
    service()
        .ok_or_else(|| "goal store unavailable".to_string())?
        .store
        .get(&goal_id)
        .map_err(|e| e.to_string())
}

#[tauri::command]
pub fn goal_attempts(goal_id: String) -> Result<Vec<oz_steward::goal::GoalAttempt>, String> {
    service()
        .ok_or_else(|| "goal store unavailable".to_string())?
        .store
        .attempts(&goal_id, 50)
        .map_err(|e| e.to_string())
}

#[tauri::command]
pub fn goal_guard(
    goal_id: String,
) -> Result<Option<oz_steward::goal::guardian::GoalGuardianReport>, String> {
    let svc = service().ok_or_else(|| "goal store unavailable".to_string())?;
    GoalGuardian::new(svc.store.clone())
        .inspect(&goal_id)
        .map_err(|e| e.to_string())
}

#[tauri::command]
pub async fn goal_start(
    app: AppHandle,
    state: State<'_, Arc<AppState>>,
    goal_id: String,
    model_name: Option<String>,
) -> Result<GoalStatus, String> {
    start_goal(app, state.inner().clone(), goal_id, model_name, false).await
}

#[tauri::command]
pub async fn goal_resume(
    app: AppHandle,
    state: State<'_, Arc<AppState>>,
    goal_id: String,
    model_name: Option<String>,
) -> Result<GoalStatus, String> {
    start_goal(app, state.inner().clone(), goal_id, model_name, true).await
}

async fn start_goal(
    app: AppHandle,
    state: Arc<AppState>,
    goal_id: String,
    model_name: Option<String>,
    resume: bool,
) -> Result<GoalStatus, String> {
    let svc = service().ok_or_else(|| "goal store unavailable".to_string())?;
    let goal = svc
        .store
        .get(&goal_id)
        .map_err(|e| e.to_string())?
        .ok_or_else(|| "goal not found".to_string())?;
    if goal.status.is_terminal() {
        return Err("goal is terminal".into());
    }
    svc.runtime.start(&goal_id).map_err(|e| e.to_string())?;
    let prompt = if resume {
        continuation_prompt(&svc, &goal)
    } else {
        prompt(&goal, false)
    };
    if let Err(e) = Box::pin(commands::send_message_core(
        prompt,
        goal.spec.session_id.clone(),
        None,
        model_name,
        None,
        app.clone(),
        state.clone(),
        resume,
    ))
    .await
    {
        let _ = svc.runtime.pause(&goal_id, &format!("send failed: {e}"));
        return Err(e);
    }
    Ok(GoalStatus::Active)
}

#[tauri::command]
pub fn goal_pause(state: State<'_, Arc<AppState>>, goal_id: String) -> Result<GoalStatus, String> {
    let svc = service().ok_or_else(|| "goal store unavailable".to_string())?;
    let goal = svc
        .store
        .get(&goal_id)
        .map_err(|e| e.to_string())?
        .ok_or_else(|| "goal not found".to_string())?;
    let status = svc
        .runtime
        .pause(&goal_id, "paused by user")
        .map_err(|e| e.to_string())?;
    stop_session(state.inner(), &goal.spec.session_id);
    Ok(status)
}

#[tauri::command]
pub fn goal_stop(state: State<'_, Arc<AppState>>, goal_id: String) -> Result<GoalStatus, String> {
    let svc = service().ok_or_else(|| "goal store unavailable".to_string())?;
    let goal = svc
        .store
        .get(&goal_id)
        .map_err(|e| e.to_string())?
        .ok_or_else(|| "goal not found".to_string())?;
    let status = svc
        .runtime
        .stop(&goal_id, "stopped by user")
        .map_err(|e| e.to_string())?;
    stop_session(state.inner(), &goal.spec.session_id);
    Ok(status)
}

#[tauri::command]
pub fn goal_accept(goal_id: String) -> Result<GoalStatus, String> {
    let svc = service().ok_or_else(|| "goal store unavailable".to_string())?;
    let status = svc
        .runtime
        .accept(&goal_id, "human accepted gate evidence")
        .map_err(|e| e.to_string())?;
    let _ = sync_steward_report(&svc, &goal_id);
    Ok(status)
}

#[tauri::command]
pub fn goal_takeover(state: State<'_, Arc<AppState>>, goal_id: String) -> Result<String, String> {
    let svc = service().ok_or_else(|| "goal store unavailable".to_string())?;
    let goal = svc
        .store
        .get(&goal_id)
        .map_err(|e| e.to_string())?
        .ok_or_else(|| "goal not found".to_string())?;
    let _ = svc.runtime.pause(&goal_id, "taken over by user");
    stop_session(state.inner(), &goal.spec.session_id);
    Ok(goal.spec.session_id)
}

/// Called at the end of every agent run. It is deliberately called only after
/// the run has produced its final message; a goal attempt is never judged from
/// the model's own "done" claim.
pub async fn on_run_finished(
    app: &AppHandle,
    state: &Arc<AppState>,
    session_id: &str,
    exit_reason: &str,
    tokens: u64,
    summary: &str,
) {
    let Some(svc) = service() else {
        return;
    };
    let Ok(Some(goal)) = find_for_session(&svc, session_id) else {
        return;
    };
    if exit_reason == "stopped_by_user" {
        let _ = svc.runtime.pause(&goal.id, "stopped_by_user");
        return;
    }
    let _ = svc.runtime.heartbeat(&goal.id);
    let workspace = workspace_for(state, session_id);
    let runner = GateRunner::new(workspace);
    let gates = runner.run_all(&goal.spec.gates).await;
    let gate_results = gates.into_iter().map(|g| g.result).collect::<Vec<_>>();
    let decision = match svc
        .runtime
        .complete_attempt(&goal.id, summary, tokens, None, gate_results)
        .await
    {
        Ok(d) => d,
        Err(e) => {
            tracing::warn!("goal attempt completion failed: {e}");
            return;
        }
    };
    let _ = app.emit(
        "goal_event",
        json!({"goal_id": goal.id, "decision": decision}),
    );
    let _ = sync_steward_report(&svc, &goal.id);
    match decision {
        oz_steward::goal::GoalDecision::Continue => {
            if let Some(tx) = CONTINUE_TX.get() {
                let _ = tx.send(Continuation {
                    app: app.clone(),
                    state: state.clone(),
                    session_id: session_id.to_string(),
                    goal_id: goal.id.clone(),
                    model_name: None,
                });
            }
        }
        oz_steward::goal::GoalDecision::Verify => {
            crate::notify_if_unfocused(
                app,
                "OpenZen · Goal 需要你",
                "验收 gate 需要人工确认；目标已停在 verifying。",
            );
        }
        oz_steward::goal::GoalDecision::Achieved
        | oz_steward::goal::GoalDecision::Blocked
        | oz_steward::goal::GoalDecision::Exhausted
        | oz_steward::goal::GoalDecision::Paused
        | oz_steward::goal::GoalDecision::Abandoned => {
            let body = format!("goal {:?}: {}", decision, goal.spec.title);
            crate::notify_if_unfocused(app, "OpenZen · Goal", &body);
        }
    }
}

/// Heartbeat while a long run is in flight; called from the runner collector.
pub fn heartbeat_for_session(session_id: &str) {
    let Some(svc) = service() else {
        return;
    };
    if let Ok(Some(goal)) = find_for_session(&svc, session_id) {
        let _ = svc.runtime.heartbeat(&goal.id);
    }
}
