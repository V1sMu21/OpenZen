//! Butler runtime bridge: task store, session bootstrap, model policy and the
//! scheduler entry points.
//!
//! The butler reuses the ordinary worker prompt (Erya). It differs in three
//! ways only: a first-turn brief, an owned task graph, and a scheduler that
//! dispatches background workers.

use std::sync::{Arc, OnceLock};

use serde_json::json;
use tauri::State;

use oz_core_types::session_role::{ModelSource, SessionRole};
use oz_steward::butler::{Approval, ButlerStore, Task, TaskEvent};

use crate::{lock_poison_guard, AppState};

pub mod scheduler;

/// Injected once, before the first user turn of a butler session. Kept short
/// on purpose: it is a charter, not a second persona.
pub const BUTLER_BRIEF: &str = "\
[尔雅] 用户只跟你说话，活由后台工人干。你是尔雅，不是另一个人格。
- 先判：天气/短查询等只读小事可直接做（≤6 次工具）；要改文件/跑命令/多步调研就 task_create 派工。
- 先拆：≤6 项，每项要能验收；小于 15 分钟的合并。spec 存任务图，不写 task_spec.md。
- 再派：派工后立刻把控制权还给用户，不要占着对话等结果。
- 只说三种话：开始、卡住、完成；其余时间安静。
- 完成与否由验收器判定，你只转述结论与证据，不要自称「已验证」。
- 推不出的选最小可逆假设并记账；不可逆的事进待批，不代做、不代批。
- 全部结束后一条 digest：完成 N（verified/unverified）/ 证据 / 未决 / 下一步。";

/// Tools that only orchestrate: always available to a butler.
pub const BUTLER_ORCHESTRATION_TOOLS: &[&str] = &[
    "task_create",
    "task_list",
    "task_get",
    "task_note",
    "run_list",
    "project_list",
    "history_search",
    "ask_user",
    "respond",
    "submit_plan",
    "schedule_reminder",
    "long_term",
    "no_tool",
];

/// Extra read-only/search tools for a request classified as simple (weather,
/// a quick lookup, one file). Mutating/execution tools are never here.
pub const BUTLER_SIMPLE_TOOLS: &[&str] = &[
    "task_create",
    "task_list",
    "task_get",
    "task_note",
    "run_list",
    "project_list",
    "history_search",
    "ask_user",
    "respond",
    "submit_plan",
    "schedule_reminder",
    "long_term",
    "no_tool",
    "web_search",
    "web_fetch",
    "read",
    "ls",
    "glob",
    "grep",
    "artifact_find",
];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RequestClass {
    /// Instant lookup (weather/time/news): answer directly, no dispatch.
    Lookup,
    /// Short read-only task: direct tools, may escalate to task_create.
    Simple,
    /// Must decompose and dispatch; execution tools are removed.
    Complex,
}

/// Deterministic first-pass classifier. It is deliberately conservative:
/// strong action verbs always mean `Complex`; only short requests without
/// file/system/action signals stay `Simple`. The model can escalate a simple
/// request by calling `task_create`; it cannot downgrade a complex one.
pub fn classify_request(text: &str) -> RequestClass {
    let t = text.trim().to_lowercase();
    if t.is_empty() {
        return RequestClass::Simple;
    }
    let chars = t.chars().count();
    // Instant lookups: short, weather/time/news asks with no engineering
    // signal. These must be answered directly, never dispatched.
    const LOOKUP_TERMS: &[&str] = &[
        "天气",
        "气温",
        "降雨",
        "空气质量",
        "汇率",
        "股价",
        "新闻",
        "几点",
        "限行",
        "weather",
    ];
    let lookup_hit = LOOKUP_TERMS.iter().any(|k| t.contains(k));
    // Irreversible/engineering verbs: even a short sentence is a job.
    const ALWAYS_COMPLEX: &[&str] = &[
        "安装",
        "覆盖",
        "重构",
        "迁移",
        "实现",
        "修复",
        "部署",
        "删除",
        "批量",
        "回滚",
        "发布",
        "install",
        "migrate",
        "refactor",
        "deploy",
        // A short "继续/接着" usually continues a complex job; keep it on the
        // dispatch path rather than letting a continuation become direct work.
        "继续",
        "接着",
        "重试",
        "重新跑",
        "continue",
    ];
    if ALWAYS_COMPLEX.iter().any(|k| t.contains(k)) {
        return RequestClass::Complex;
    }
    const SIGNALS: &[&str] = &[
        "调研",
        "分析",
        "对比",
        "审计",
        "整理",
        "清理",
        "替换",
        "更新",
        "同步",
        "测试",
        "验证",
        "生成",
        "构建",
        "设计",
        "排查",
        "技能",
        "skill",
        "项目",
        "仓库",
        "代码",
        "脚本",
        "数据库",
        "sqlite",
        "所有",
        "全部",
        "分别",
        "然后",
        "并且",
        "同时",
        "之后",
    ];
    let hits = SIGNALS.iter().filter(|k| t.contains(*k)).count();
    let multi = ["然后", "并且", "同时", "分别", "之后"]
        .iter()
        .filter(|k| t.contains(*k))
        .count();
    let pathish = t.contains('/')
        || t.contains('\\')
        || t.contains('~')
        || t.contains("repo")
        || t.contains(".py")
        || t.contains(".rs")
        || t.contains(".json")
        || t.contains(".toml");
    let wants_output = ["写", "保存", "导出", "文件", "安装", "覆盖", "派工"]
        .iter()
        .any(|k| t.contains(k));
    if lookup_hit && !wants_output && !pathish && hits == 0 && chars <= 40 {
        return RequestClass::Lookup;
    }
    if pathish && hits > 0 {
        return RequestClass::Complex;
    }
    if hits >= 2 || (hits >= 1 && chars > 40) || chars > 100 || (multi >= 1 && chars > 25) {
        return RequestClass::Complex;
    }
    RequestClass::Simple
}

/// Lookups keep search/read but lose task_create: weather must not become a
/// 20-minute worker task. If the data is unavailable, say so.
pub const BUTLER_LOOKUP_TOOLS: &[&str] = &[
    "task_list",
    "task_get",
    "task_note",
    "run_list",
    "project_list",
    "history_search",
    "ask_user",
    "respond",
    "submit_plan",
    "schedule_reminder",
    "long_term",
    "no_tool",
    "web_search",
    "web_fetch",
    "read",
    "ls",
    "glob",
    "grep",
    "artifact_find",
];

pub fn butler_tools_for(class: RequestClass) -> &'static [&'static str] {
    match class {
        RequestClass::Lookup => BUTLER_LOOKUP_TOOLS,
        RequestClass::Simple => BUTLER_SIMPLE_TOOLS,
        RequestClass::Complex => BUTLER_ORCHESTRATION_TOOLS,
    }
}

pub fn butler_system_suffix(class: RequestClass) -> String {
    let base = "\n\n[尔雅 · 调度者] 你是控制面，不是执行者。先判断这件事该不该自己动手：\n";
    let policy = match class {
        RequestClass::Lookup => {
            "这次是即时查询（天气/时间/新闻等）：直接用 web_search/web_fetch 查，1-2 次调用内回答；\n             禁止 task_create，也不要为查询派工人。数据不完整就如实说明缺口。"
        }
        RequestClass::Complex => {
            "这次是复杂请求：你没有 read/write/edit/grep/glob/ls/code_run/web_* 工具。\n             先 task_create 拆成可验收任务（绑定当前项目），然后只回复“已派工：<任务摘要>，完成后我会汇报”，\n             立即把控制权还给用户；不要自己研究，也不要等结果。"
        }
        RequestClass::Simple => {
            "这次是简单请求：你只有 web_search/web_fetch/read/ls/glob/grep 等只读工具，最多 6 次调用。\n             天气/汇率/新闻/几点这类即时查询直接回答，禁止 task_create；数据不完整就说明缺口，不要为查询派工人。\n             只有确实需要写文件、跑命令、安装/覆盖、批量处理时，才 task_create 派工；不要继续自己钻。"
        }
    };
    format!("{base}{policy}")
}

static STORE: OnceLock<Option<Arc<ButlerStore>>> = OnceLock::new();
static PAUSED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

pub fn store() -> Option<Arc<ButlerStore>> {
    STORE
        .get_or_init(
            || match ButlerStore::open(crate::data_dir().join("butler.db")) {
                Ok(s) => Some(Arc::new(s)),
                Err(e) => {
                    tracing::warn!("butler store unavailable: {e}");
                    None
                }
            },
        )
        .clone()
}

pub fn init() {
    let path = crate::data_dir().join("butler.db");
    // The task tools in oz-tools cannot see AppState; they open the same file
    // through this env var. One data root per process, so this is stable.
    std::env::set_var("OPENZEN_BUTLER_DB", &path);
    std::env::set_var(
        "OPENZEN_PROJECTS_FILE",
        crate::data_dir().join("projects.json"),
    );
    std::env::set_var(
        "OPENZEN_SESSIONS_FILE",
        crate::data_dir().join("openzen").join("sessions.json"),
    );
    let _ = store();
}

pub fn paused() -> bool {
    PAUSED.load(std::sync::atomic::Ordering::Relaxed)
}

pub fn set_paused(paused: bool) {
    PAUSED.store(paused, std::sync::atomic::Ordering::Relaxed);
}

/// Whether this run should prepend the charter. Extracted so the rule is
/// testable without an app handle: butler role, first turn, not a resume.
pub fn should_inject_brief(is_butler: bool, message_count: usize, resume: bool) -> bool {
    is_butler && !resume && message_count <= 1
}

/// Rename sessions created under the old display name (管家) to 尔雅. Data is
/// otherwise untouched; the butler role is unchanged.
pub fn migrate_legacy_names(state: &Arc<AppState>) {
    let renames: Vec<(String, String)> = {
        let sessions = lock_poison_guard(&state.sessions);
        sessions
            .list()
            .into_iter()
            .filter(|s| s.role.is_butler() && s.name.contains("管家"))
            .map(|s| (s.id.clone(), s.name.replace("管家", "尔雅")))
            .collect()
    };
    if renames.is_empty() {
        return;
    }
    let mut sessions = lock_poison_guard(&state.sessions);
    for (id, name) in &renames {
        let _ = sessions.rename(id, name);
    }
    tracing::info!(
        "[butler] renamed {} legacy 管家 session(s) to 尔雅",
        renames.len()
    );
}

/// Compact per-turn state for the butler: active tasks, pending approvals,
/// projects. Injected as a system-reminder so status does not depend on the
/// model remembering earlier turns.
pub fn state_block(store: &ButlerStore, projects: &[String]) -> String {
    let counts = store
        .counts(None)
        .unwrap_or(oz_steward::butler::TaskCounts {
            total: 0,
            verified: 0,
            unverified: 0,
            failed: 0,
            open: 0,
        });
    let mut lines = vec![format!(
        "[状态] 任务 {}（open {} / verified {} / unverified {} / failed {}）",
        counts.total, counts.open, counts.verified, counts.unverified, counts.failed
    )];
    if let Ok(tasks) = store.list(None, 200) {
        let open: Vec<_> = tasks
            .iter()
            .filter(|t| !t.status.is_terminal())
            .take(6)
            .map(|t| {
                let p = t.project_name.as_deref().unwrap_or("workspace");
                format!(
                    "{}({}) {} · {}",
                    t.goal.chars().take(28).collect::<String>(),
                    p,
                    t.status.as_str(),
                    t.id.chars().take(8).collect::<String>()
                )
            })
            .collect();
        if !open.is_empty() {
            lines.push(format!("[活跃] {}", open.join("；")));
        }
    }
    if let Ok(pending) = store.approvals(Some(oz_steward::butler::ApprovalStatus::Pending)) {
        if !pending.is_empty() {
            lines.push(format!("[待批] {} 项", pending.len()));
        }
    }
    if !projects.is_empty() {
        lines.push(format!("[项目] {}", projects.join(" · ")));
    }
    lines.join("\n")
}

/// Find or create the butler session for a project.
pub fn ensure_session_for_project(
    state: &Arc<AppState>,
    project_id: Option<&str>,
) -> Result<serde_json::Value, String> {
    let existing = {
        let sessions = lock_poison_guard(&state.sessions);
        sessions
            .list()
            .into_iter()
            .find(|s| s.role == SessionRole::Butler && s.project_id.as_deref() == project_id)
            .map(|s| (s.id, s.name, s.working_dir))
    };
    if let Some((id, name, working_dir)) = existing {
        return Ok(
            json!({"session_id": id, "name": name, "working_dir": working_dir, "created": false}),
        );
    }

    let (project_name, working_dir) = match project_id {
        Some(pid) => {
            let projects = lock_poison_guard(&state.projects);
            match projects.iter().find(|p| p.id == pid) {
                Some(p) => (Some(p.name.clone()), p.root_path.clone()),
                None => (None, state.working_dir.clone()),
            }
        }
        None => (None, state.working_dir.clone()),
    };
    let name = match &project_name {
        Some(n) => format!("尔雅 · {n}"),
        None => "尔雅".to_string(),
    };
    let info = {
        let mut sessions = lock_poison_guard(&state.sessions);
        let info = sessions.create_with_project(
            &name,
            project_id,
            project_name.as_deref(),
            Some(&working_dir),
        );
        if let Some(entry) = sessions.get_mut(&info.id) {
            entry.role = SessionRole::Butler;
            entry.info.role = SessionRole::Butler;
        }
        // `create_with_project` already persisted the Worker default; write
        // again so a restart does not lose the butler role.
        sessions.save();
        info
    };
    Ok(json!({
        "session_id": info.id,
        "name": info.name,
        "working_dir": working_dir,
        "created": true
    }))
}

/// Where a worker's model comes from. Pure so it can be unit-tested without
/// an app handle.
pub struct ModelChoice {
    pub model: String,
    pub source: ModelSource,
}

pub fn choose_worker_model(
    task_model: Option<&str>,
    configured_worker: Option<&str>,
    butler_model: Option<&str>,
    butler_source: ModelSource,
    cloud_model: Option<&str>,
    fallback: &str,
) -> ModelChoice {
    if let Some(m) = task_model.filter(|s| !s.trim().is_empty()) {
        return ModelChoice {
            model: m.to_string(),
            source: ModelSource::Task,
        };
    }
    if let Some(m) = configured_worker.filter(|s| !s.trim().is_empty()) {
        return ModelChoice {
            model: m.to_string(),
            source: ModelSource::User,
        };
    }
    match butler_model.filter(|s| !s.trim().is_empty()) {
        // A model the user picked wins even if it is local.
        Some(m) if butler_source == ModelSource::User => ModelChoice {
            model: m.to_string(),
            source: ModelSource::User,
        },
        // A defaulted model may be upgraded to the cloud preference.
        Some(m) => ModelChoice {
            model: cloud_model.unwrap_or(m).to_string(),
            source: ModelSource::Default,
        },
        None => ModelChoice {
            model: cloud_model.unwrap_or(fallback).to_string(),
            source: ModelSource::Default,
        },
    }
}

// ── commands ────────────────────────────────────────────────────────────

#[tauri::command]
pub fn butler_status(state: State<'_, Arc<AppState>>) -> serde_json::Value {
    let Some(store) = store() else {
        return json!({"available": false});
    };
    let counts = store
        .counts(None)
        .unwrap_or(oz_steward::butler::TaskCounts {
            total: 0,
            verified: 0,
            unverified: 0,
            failed: 0,
            open: 0,
        });
    let pending = store
        .approvals(Some(oz_steward::butler::ApprovalStatus::Pending))
        .map(|v| v.len())
        .unwrap_or(0);
    let _ = state;
    json!({
        "available": true,
        "paused": paused(),
        "tasks": counts.total,
        "open": counts.open,
        "verified": counts.verified,
        "unverified": counts.unverified,
        "failed": counts.failed,
        "pending_approvals": pending,
    })
}

#[tauri::command]
pub fn butler_ensure_session(
    project_id: Option<String>,
    state: State<'_, Arc<AppState>>,
) -> Result<serde_json::Value, String> {
    ensure_session_for_project(state.inner(), project_id.as_deref())
}

/// Hand a task from any session to the butler: ensure its session exists,
/// post the text there and start the run. The caller then selects the returned
/// session so the user watches the same thing the background dispatcher does.
#[tauri::command]
pub async fn butler_submit(
    text: String,
    project_id: Option<String>,
    app: tauri::AppHandle,
    state: State<'_, Arc<AppState>>,
) -> Result<serde_json::Value, String> {
    let text = text.trim().to_string();
    if text.is_empty() {
        return Err("请先输入要交给管家的任务".into());
    }
    if text.chars().count() > 4000 {
        return Err("任务太长（上限 4000 字），请拆成几步".into());
    }
    tracing::info!(
        "[butler] handoff submit len={} project={:?}",
        text.len(),
        project_id
    );
    let info = ensure_session_for_project(state.inner(), project_id.as_deref())?;
    let session_id = info
        .get("session_id")
        .and_then(|v| v.as_str())
        .unwrap_or_default()
        .to_string();
    if session_id.is_empty() {
        return Err("无法创建管家会话".into());
    }
    crate::commands::send_message_core_ex(
        text,
        session_id.clone(),
        None,
        None,
        None,
        app,
        state.inner().clone(),
        false,
        false,
    )
    .await?;
    Ok(json!({"session_id": session_id, "created": info.get("created")}))
}

#[tauri::command]
pub fn butler_tasks(project_id: Option<String>) -> Result<Vec<Task>, String> {
    let store = store().ok_or_else(|| "butler store unavailable".to_string())?;
    store
        .list(project_id.as_deref(), 200)
        .map_err(|e| e.to_string())
}

#[tauri::command]
pub fn butler_task_events(task_id: String) -> Result<Vec<TaskEvent>, String> {
    let store = store().ok_or_else(|| "butler store unavailable".to_string())?;
    store.events(&task_id, 50).map_err(|e| e.to_string())
}

#[tauri::command]
pub fn butler_runs(
    project_id: Option<String>,
    limit: Option<usize>,
) -> Result<Vec<oz_steward::butler::Run>, String> {
    store()
        .ok_or_else(|| "butler store unavailable".to_string())?
        .list_runs(project_id.as_deref(), limit.unwrap_or(50))
        .map_err(|e| e.to_string())
}

#[tauri::command]
pub fn butler_run_detail(run_id: String) -> Result<serde_json::Value, String> {
    let store = store().ok_or_else(|| "butler store unavailable".to_string())?;
    let run = store
        .get_run(&run_id)
        .map_err(|e| e.to_string())?
        .ok_or_else(|| format!("run {run_id} not found"))?;
    let (meta, mut messages) = crate::subagents::read_child(&run.child_id).unwrap_or_default();
    if messages.len() > 300 {
        messages = messages.split_off(messages.len() - 300);
    }
    Ok(json!({"run": run, "meta": meta, "messages": messages}))
}

#[tauri::command]
pub fn butler_automations(state: State<'_, Arc<AppState>>) -> serde_json::Value {
    let reminders: Vec<serde_json::Value> = lock_poison_guard(&state.pending_reminders)
        .iter()
        .map(|r| {
            json!({
                "id": r.id,
                "kind": "reminder",
                "session_id": r.session_id,
                "message": r.message,
                "fire_at_ms": r.fire_at_ms,
                "repeat_count": r.repeat_count,
                "repeat_interval_secs": r.repeat_interval_secs,
                "persist": r.persist,
            })
        })
        .collect();
    let goals_path = crate::data_dir().join("goals.db");
    let goals = oz_steward::goal::store::GoalStore::open(&goals_path)
        .ok()
        .and_then(|s| s.list(200).ok())
        .unwrap_or_default();
    let active_goals = goals
        .iter()
        .filter(|g| {
            matches!(
                g.status,
                oz_steward::goal::GoalStatus::Draft
                    | oz_steward::goal::GoalStatus::Active
                    | oz_steward::goal::GoalStatus::Paused
                    | oz_steward::goal::GoalStatus::Verifying
                    | oz_steward::goal::GoalStatus::Blocked
            )
        })
        .count();
    let steward_path = crate::data_dir().join("steward.db");
    let steward = oz_steward::Store::open(&steward_path)
        .ok()
        .and_then(|s| s.ticket_stats().ok())
        .map(|(open, total)| json!({"open": open, "total": total}))
        .unwrap_or_else(|| json!({"open": 0, "total": 0}));
    let platforms: Vec<serde_json::Value> = std::fs::read_to_string(&state.config_path)
        .ok()
        .and_then(|t| t.parse::<toml::Value>().ok())
        .and_then(|v| v.get("platforms").and_then(|p| p.as_table()).cloned())
        .map(|table| {
            table
                .iter()
                .map(|(name, v)| {
                    json!({
                        "name": name,
                        "enabled": v.get("enabled").and_then(|e| e.as_bool()).unwrap_or(false),
                    })
                })
                .collect()
        })
        .unwrap_or_default();
    json!({
        "reminders": reminders,
        "goals": {"total": goals.len(), "active": active_goals},
        "steward": steward,
        "platforms": platforms,
        "goals_db": goals_path.to_string_lossy(),
        "scheduler": ["session_cleanup", "trust_decay"],
    })
}

#[tauri::command]
pub fn butler_cancel_reminder(
    id: Option<String>,
    index: Option<usize>,
    state: State<'_, Arc<AppState>>,
) -> Result<bool, String> {
    let removed = {
        let mut pending = lock_poison_guard(&state.pending_reminders);
        let supplied_id = id.as_deref().filter(|s| !s.is_empty());
        let pos = if let Some(id) = supplied_id {
            // An id that does not match must NOT fall back to a stale index:
            // that would delete a different reminder.
            let Some(pos) = pending.iter().position(|r| r.id == id) else {
                return Ok(false);
            };
            Some(pos)
        } else {
            index
        };
        let Some(pos) = pos else { return Ok(false) };
        if pos >= pending.len() {
            return Ok(false);
        }
        pending.remove(pos);
        true
    };
    crate::save_persistent_reminders(&state);
    Ok(removed)
}

#[tauri::command]
pub fn butler_approvals() -> Result<Vec<Approval>, String> {
    let store = store().ok_or_else(|| "butler store unavailable".to_string())?;
    store
        .approvals(Some(oz_steward::butler::ApprovalStatus::Pending))
        .map_err(|e| e.to_string())
}

#[tauri::command]
pub fn butler_decide(approval_id: String, approved: bool) -> Result<bool, String> {
    let store = store().ok_or_else(|| "butler store unavailable".to_string())?;
    let decided = store
        .decide_approval(&approval_id, approved)
        .map_err(|e| e.to_string())?;
    if let Some(approval) = &decided {
        if approved {
            if let Ok(Some(task)) = store.get(&approval.task_id) {
                if task.status == oz_steward::butler::TaskStatus::Blocked {
                    let _ = store.set_status(
                        &task.id,
                        oz_steward::butler::TaskStatus::Ready,
                        "approval granted",
                    );
                }
            }
        } else if let Ok(Some(task)) = store.get(&approval.task_id) {
            if task.status == oz_steward::butler::TaskStatus::Blocked {
                let _ = store.finish(
                    &task.id,
                    oz_steward::butler::TaskStatus::Failed,
                    &[],
                    &[],
                    &[],
                );
                let _ = store.append_event(&task.id, "approval_denied", "approval denied by user");
            }
        }
    }
    Ok(decided.is_some())
}

#[tauri::command]
pub fn butler_pause(paused: bool) -> bool {
    set_paused(paused);
    paused
}

#[tauri::command]
pub async fn butler_tick(
    app: tauri::AppHandle,
    state: State<'_, Arc<AppState>>,
) -> Result<usize, String> {
    scheduler::tick(app, state.inner().clone()).await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn brief_is_injected_once_and_never_for_workers() {
        assert!(should_inject_brief(true, 1, false));
        assert!(
            !should_inject_brief(true, 2, false),
            "second turn must not re-inject"
        );
        assert!(
            !should_inject_brief(true, 1, true),
            "resume already carries it"
        );
        assert!(!should_inject_brief(false, 1, false));
    }

    #[test]
    fn butler_brief_is_small() {
        assert!(
            BUTLER_BRIEF.lines().count() <= 12,
            "brief grew into a persona"
        );
        assert!(BUTLER_BRIEF.chars().count() <= 900);
    }

    #[test]
    fn butler_tool_boundary_is_tiered() {
        assert!(BUTLER_ORCHESTRATION_TOOLS.contains(&"task_create"));
        for forbidden in ["code_run", "read", "write", "grep", "web_search"] {
            assert!(
                !BUTLER_ORCHESTRATION_TOOLS.contains(&forbidden),
                "complex butler kept {forbidden}"
            );
        }
        assert!(BUTLER_SIMPLE_TOOLS.contains(&"web_search"));
        assert!(BUTLER_SIMPLE_TOOLS.contains(&"read"));
        assert!(!BUTLER_LOOKUP_TOOLS.contains(&"task_create"));
        assert!(!BUTLER_LOOKUP_TOOLS.contains(&"code_run"));
        for forbidden in ["code_run", "write", "edit", "patch", "spawn_agent"] {
            assert!(
                !BUTLER_SIMPLE_TOOLS.contains(&forbidden),
                "simple butler kept {forbidden}"
            );
        }
        assert!(butler_system_suffix(RequestClass::Complex).contains("不要自己研究"));
        assert!(butler_system_suffix(RequestClass::Simple).contains("最多 6 次"));
    }

    #[test]
    fn complexity_classifier_separates_weather_from_skill_install() {
        assert_eq!(
            classify_request("帮我查一下明天北京天气"),
            RequestClass::Lookup
        );
        assert_eq!(
            classify_request("看一下杭州近一周天气"),
            RequestClass::Lookup
        );
        assert_eq!(classify_request("现在几点？"), RequestClass::Lookup);
        assert_eq!(classify_request("读一下 README"), RequestClass::Simple);
        assert_eq!(
            classify_request("调研一下 zcode 中关于 visualmusic 的技能，并安装到 openzen"),
            RequestClass::Complex
        );
        assert_eq!(
            classify_request("把项目里的旧 skill 全部覆盖并更新脚本"),
            RequestClass::Complex
        );
        assert_eq!(classify_request("帮我修复这个 bug"), RequestClass::Complex);
        assert_eq!(classify_request("继续"), RequestClass::Complex);
    }

    #[test]
    fn butler_brief_names_the_contract() {
        for key in ["先判", "拆", "派", "验收", "假设", "待批", "digest"] {
            assert!(BUTLER_BRIEF.contains(key), "brief lost: {key}");
        }
    }

    #[test]
    fn task_model_wins() {
        let c = choose_worker_model(
            Some("t"),
            Some("w"),
            Some("b"),
            ModelSource::User,
            Some("cloud"),
            "fb",
        );
        assert_eq!(c.model, "t");
        assert_eq!(c.source, ModelSource::Task);
    }

    #[test]
    fn explicit_local_butler_is_not_upgraded_to_cloud() {
        let c = choose_worker_model(
            None,
            None,
            Some("local-a"),
            ModelSource::User,
            Some("cloud"),
            "fb",
        );
        assert_eq!(c.model, "local-a");
    }

    #[test]
    fn defaulted_butler_prefers_cloud() {
        let c = choose_worker_model(
            None,
            None,
            Some("local-a"),
            ModelSource::Default,
            Some("cloud"),
            "fb",
        );
        assert_eq!(c.model, "cloud");
    }

    #[test]
    fn configured_worker_overrides_inheritance() {
        let c = choose_worker_model(
            None,
            Some("w"),
            Some("b"),
            ModelSource::User,
            Some("cloud"),
            "fb",
        );
        assert_eq!(c.model, "w");
        assert_eq!(c.source, ModelSource::User);
    }

    #[test]
    fn fallback_when_nothing_is_configured() {
        let c = choose_worker_model(None, None, None, ModelSource::Default, None, "fb");
        assert_eq!(c.model, "fb");
    }
}
