//! Plan review (方案审查).
//!
//! An agent submits a plan → the plan lands under `<working_dir>/.openzen/plans/`
//! → a `plan` tab opens in the side panel → the user approves or rejects it with
//! an optional free-text note → the decision is written next to the plan.
//!
//! Two entry points:
//! - `plan_submit` (IPC): the in-app agent or any UI code path.
//! - the inbox: an out-of-process agent drops `*.json` into
//!   `.openzen/plans/inbox/` and [`import_inbox`] picks it up (polled by the
//!   background task in `lib.rs`, and on demand via `plan_scan_inbox`).
//!
//! Files are the source of truth: `<id>.json` and `<id>.decision.json` are
//! plain JSON, safe to commit and diff.

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use serde::{Deserialize, Serialize};
use tauri::{AppHandle, Emitter, State};

use crate::AppState;

/// Side-panel artifact type rendered by `PlanReview.svelte`.
pub const PLAN_TAB_TYPE: &str = "plan";

// ───────────────────────────── data ─────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PlanOption {
    pub label: String,
    #[serde(default)]
    pub detail: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PlanDecision {
    /// "approved" | "rejected"
    pub decision: String,
    #[serde(default)]
    pub option: String,
    #[serde(default)]
    pub comment: String,
    pub decided_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Plan {
    pub id: String,
    pub title: String,
    #[serde(default)]
    pub summary: String,
    #[serde(default)]
    pub markdown: String,
    #[serde(default)]
    pub options: Vec<PlanOption>,
    #[serde(default)]
    pub est_cost: String,
    /// "pending" | "approved" | "rejected"
    pub status: String,
    pub created_at: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub decision: Option<PlanDecision>,
    /// Absolute path of `<id>.json`; filled on read, not round-tripped.
    #[serde(default)]
    pub path: String,
}

/// Shape accepted from the inbox and from `plan_submit`.
#[derive(Debug, Clone, Deserialize)]
pub struct PlanDraft {
    pub title: String,
    #[serde(default)]
    pub summary: String,
    #[serde(default)]
    pub markdown: String,
    #[serde(default)]
    pub options: Vec<PlanOption>,
    #[serde(default)]
    pub est_cost: String,
}

// ───────────────────────────── storage ─────────────────────────────

fn plans_dir(workdir: &str) -> PathBuf {
    Path::new(workdir).join(".openzen").join("plans")
}

fn inbox_dir(workdir: &str) -> PathBuf {
    plans_dir(workdir).join("inbox")
}

fn ensure_dirs(workdir: &str) -> Result<(), String> {
    fs::create_dir_all(inbox_dir(workdir)).map_err(|e| format!("create plans dir: {e}"))
}

fn new_id() -> String {
    let now = chrono::Local::now();
    format!("{}-{}", now.format("%Y%m%d-%H%M%S"), &uuid_like()[..4])
}

fn uuid_like() -> String {
    use std::time::{SystemTime, UNIX_EPOCH};
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.subsec_nanos())
        .unwrap_or(0);
    format!("{nanos:08x}")
}

fn now_string() -> String {
    chrono::Local::now().format("%Y-%m-%d %H:%M:%S").to_string()
}

fn is_plan_file(name: &str) -> bool {
    name.ends_with(".json") && !name.ends_with(".decision.json")
}

/// Every plan in the working directory, newest first.
pub fn list_plans(workdir: &str) -> Vec<Plan> {
    let dir = plans_dir(workdir);
    let mut out = Vec::new();
    let Ok(entries) = fs::read_dir(&dir) else {
        return out;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if !path.is_file() {
            continue;
        }
        let name = match path.file_name().and_then(|n| n.to_str()) {
            Some(n) => n,
            None => continue,
        };
        if !is_plan_file(name) {
            continue;
        }
        if let Ok(raw) = fs::read_to_string(&path) {
            if let Ok(mut plan) = serde_json::from_str::<Plan>(&raw) {
                plan.path = path.to_string_lossy().to_string();
                out.push(plan);
            }
        }
    }
    out.sort_by(|a, b| b.created_at.cmp(&a.created_at));
    out
}

/// One plan plus its decision (if any).
pub fn read_plan(workdir: &str, id: &str) -> Option<Plan> {
    let path = plans_dir(workdir).join(format!("{id}.json"));
    let raw = fs::read_to_string(&path).ok()?;
    let mut plan: Plan = serde_json::from_str(&raw).ok()?;
    plan.path = path.to_string_lossy().to_string();
    if let Ok(dec) = fs::read_to_string(plans_dir(workdir).join(format!("{id}.decision.json"))) {
        if let Ok(d) = serde_json::from_str::<PlanDecision>(&dec) {
            plan.status = d.decision.clone();
            plan.decision = Some(d);
        }
    }
    Some(plan)
}

fn write_plan(workdir: &str, plan: &Plan) -> Result<PathBuf, String> {
    ensure_dirs(workdir)?;
    let mut clean = plan.clone();
    clean.path = String::new();
    let path = plans_dir(workdir).join(format!("{}.json", plan.id));
    let json = serde_json::to_string_pretty(&clean).map_err(|e| e.to_string())?;
    fs::write(&path, json).map_err(|e| format!("write plan: {e}"))?;
    Ok(path)
}

/// Create a plan and return it with `path` filled in.
fn create_plan(workdir: &str, draft: PlanDraft) -> Result<Plan, String> {
    let mut plan = Plan {
        id: new_id(),
        title: draft.title,
        summary: draft.summary,
        markdown: draft.markdown,
        options: draft.options,
        est_cost: draft.est_cost,
        status: "pending".into(),
        created_at: now_string(),
        decision: None,
        path: String::new(),
    };
    let path = write_plan(workdir, &plan)?;
    plan.path = path.to_string_lossy().to_string();
    Ok(plan)
}

fn write_decision(
    workdir: &str,
    id: &str,
    decision: &str,
    option: &str,
    comment: &str,
) -> Result<Plan, String> {
    if decision != "approved" && decision != "rejected" {
        return Err("decision must be approved|rejected".into());
    }
    let mut plan = read_plan(workdir, id).ok_or_else(|| format!("plan {id} not found"))?;
    let record = PlanDecision {
        decision: decision.to_string(),
        option: option.to_string(),
        comment: comment.to_string(),
        decided_at: now_string(),
    };
    let dec_path = plans_dir(workdir).join(format!("{id}.decision.json"));
    let json = serde_json::to_string_pretty(&record).map_err(|e| e.to_string())?;
    fs::write(&dec_path, json).map_err(|e| format!("write decision: {e}"))?;

    plan.status = decision.to_string();
    plan.decision = Some(record);
    write_plan(workdir, &plan)?;
    Ok(plan)
}

// ───────────────────────────── side panel ─────────────────────────────

/// Open (or focus) the plan's tab in the side panel.
fn show_tab(app: &AppHandle, state: &Arc<AppState>, plan: &Plan) {
    if let Err(e) = crate::sidepanel::commands::register_and_show(
        app,
        state,
        PLAN_TAB_TYPE.to_string(),
        plan.path.clone(),
        plan.title.clone(),
    ) {
        tracing::warn!("[plans] open side-panel tab failed: {e}");
    }
}

// ───────────────────────────── inbox ─────────────────────────────

/// Import every `*.json` dropped into `.openzen/plans/inbox/`.
///
/// The file is removed after a successful import so a restart does not
/// resurrect the same plan; parse failures are left in place for inspection.
pub fn import_inbox(app: &AppHandle, state: &Arc<AppState>) -> Result<usize, String> {
    ensure_dirs(&state.working_dir)?;
    let plans = take_inbox_drafts(&state.working_dir)?;
    for plan in &plans {
        show_tab(app, state, plan);
        let _ = app.emit("plan:changed", plan);
    }
    Ok(plans.len())
}

/// Turn every valid `*.json` in the inbox into a persisted plan and delete the
/// inbox file. Separated from [`import_inbox`] so the file-level contract can
/// be tested without an `AppHandle`.
pub fn take_inbox_drafts(workdir: &str) -> Result<Vec<Plan>, String> {
    ensure_dirs(workdir)?;
    let dir = inbox_dir(workdir);
    let mut created = Vec::new();
    let Ok(entries) = fs::read_dir(&dir) else {
        return Ok(created);
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) != Some("json") {
            continue;
        }
        let Ok(raw) = fs::read_to_string(&path) else {
            continue;
        };
        let Ok(draft) = serde_json::from_str::<PlanDraft>(&raw) else {
            tracing::warn!(
                "[plans] inbox file is not a valid draft: {}",
                path.display()
            );
            continue;
        };
        match create_plan(workdir, draft) {
            Ok(plan) => {
                let _ = fs::remove_file(&path);
                created.push(plan);
            }
            Err(e) => tracing::warn!("[plans] import failed: {e}"),
        }
    }
    Ok(created)
}

// ───────────────────────────── commands ─────────────────────────────

#[tauri::command]
pub fn plan_list(state: State<'_, Arc<AppState>>) -> Vec<Plan> {
    list_plans(&state.working_dir)
}

#[tauri::command]
pub fn plan_get(state: State<'_, Arc<AppState>>, id: String) -> Result<Plan, String> {
    read_plan(&state.working_dir, &id).ok_or_else(|| format!("plan {id} not found"))
}

#[tauri::command]
pub fn plan_submit(
    app: AppHandle,
    state: State<'_, Arc<AppState>>,
    title: String,
    markdown: String,
    options: Option<Vec<PlanOption>>,
    est_cost: Option<String>,
    summary: Option<String>,
) -> Result<Plan, String> {
    let draft = PlanDraft {
        title,
        summary: summary.unwrap_or_default(),
        markdown,
        options: options.unwrap_or_default(),
        est_cost: est_cost.unwrap_or_default(),
    };
    let plan = create_plan(&state.working_dir, draft)?;
    show_tab(&app, state.inner(), &plan);
    let _ = app.emit("plan:changed", &plan);
    Ok(plan)
}

#[tauri::command]
pub fn plan_decide(
    app: AppHandle,
    state: State<'_, Arc<AppState>>,
    id: String,
    decision: String,
    option: Option<String>,
    comment: Option<String>,
) -> Result<Plan, String> {
    let plan = write_decision(
        &state.working_dir,
        &id,
        &decision,
        option.as_deref().unwrap_or(""),
        comment.as_deref().unwrap_or(""),
    )?;
    let _ = app.emit("plan:decided", &plan);
    Ok(plan)
}

/// Open (or focus) the side-panel tab for an existing plan.
#[tauri::command]
pub fn plan_open(
    app: AppHandle,
    state: State<'_, Arc<AppState>>,
    id: String,
) -> Result<Plan, String> {
    let plan = read_plan(&state.working_dir, &id).ok_or_else(|| format!("plan {id} not found"))?;
    show_tab(&app, state.inner(), &plan);
    Ok(plan)
}

/// On-demand inbox scan (the background task also calls this every 2 s).
#[tauri::command]
pub fn plan_scan_inbox(app: AppHandle, state: State<'_, Arc<AppState>>) -> Result<usize, String> {
    import_inbox(&app, state.inner())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_workdir(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("oz-plans-{tag}-{}", new_id()));
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn creates_lists_and_reads_a_plan() {
        let wd = temp_workdir("crud");
        let plan = create_plan(
            wd.to_str().unwrap(),
            PlanDraft {
                title: "Rework the session list".into(),
                summary: "styles only".into(),
                markdown: "## Steps\n1. lock row height".into(),
                options: vec![PlanOption {
                    label: "Safe".into(),
                    detail: "styles".into(),
                }],
                est_cost: "6–8 min".into(),
            },
        )
        .unwrap();
        assert_eq!(plan.status, "pending");

        let all = list_plans(wd.to_str().unwrap());
        assert_eq!(all.len(), 1);
        assert_eq!(all[0].id, plan.id);

        let one = read_plan(wd.to_str().unwrap(), &plan.id).unwrap();
        assert_eq!(one.title, "Rework the session list");
        assert_eq!(one.options.len(), 1);
        fs::remove_dir_all(&wd).ok();
    }

    #[test]
    fn decision_is_written_next_to_the_plan() {
        let wd = temp_workdir("decide");
        let plan = create_plan(
            wd.to_str().unwrap(),
            PlanDraft {
                title: "T".into(),
                summary: String::new(),
                markdown: String::new(),
                options: vec![],
                est_cost: String::new(),
            },
        )
        .unwrap();
        let decided =
            write_decision(wd.to_str().unwrap(), &plan.id, "approved", "Safe", "go").unwrap();
        assert_eq!(decided.status, "approved");
        assert!(plans_dir(wd.to_str().unwrap())
            .join(format!("{}.decision.json", plan.id))
            .exists());

        let reread = read_plan(wd.to_str().unwrap(), &plan.id).unwrap();
        assert_eq!(reread.status, "approved");
        assert_eq!(reread.decision.as_ref().unwrap().comment, "go");
        fs::remove_dir_all(&wd).ok();
    }

    #[test]
    fn rejects_unknown_decision_values() {
        let wd = temp_workdir("bad");
        let plan = create_plan(
            wd.to_str().unwrap(),
            PlanDraft {
                title: "T".into(),
                summary: String::new(),
                markdown: String::new(),
                options: vec![],
                est_cost: String::new(),
            },
        )
        .unwrap();
        assert!(write_decision(wd.to_str().unwrap(), &plan.id, "maybe", "", "").is_err());
        fs::remove_dir_all(&wd).ok();
    }

    #[test]
    fn inbox_drops_become_plans_and_are_cleared() {
        let wd = temp_workdir("inbox");
        let w = wd.to_str().unwrap();
        ensure_dirs(w).unwrap();
        let inbox = inbox_dir(w);
        let draft = serde_json::json!({
            "title": "Ship it",
            "summary": "s",
            "markdown": "## x",
            "est_cost": "2 min",
        });
        fs::write(inbox.join("a.json"), serde_json::to_string(&draft).unwrap()).unwrap();
        fs::write(inbox.join("broken.json"), "{not json").unwrap();

        let made = take_inbox_drafts(w).unwrap();
        assert_eq!(made.len(), 1, "only the valid draft is imported");
        assert_eq!(made[0].title, "Ship it");
        assert!(!inbox.join("a.json").exists(), "imported file is removed");
        assert!(
            inbox.join("broken.json").exists(),
            "bad file is kept for inspection"
        );
        assert_eq!(list_plans(w).len(), 1);
        fs::remove_dir_all(&wd).ok();
    }
}
