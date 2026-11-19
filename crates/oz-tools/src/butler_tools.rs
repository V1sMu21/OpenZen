//! Butler task tools.
//!
//! These are the only way a model can add work to the task graph. They write
//! to the same SQLite file the scheduler reads (`OPENZEN_BUTLER_DB`), which
//! the desktop app sets at startup. A model cannot move a task to a terminal
//! state: only the verifier does that.

use std::path::{Path, PathBuf};

use async_trait::async_trait;
use oz_core_types::{ToolContext, ToolError, ToolOutput};
use oz_steward::butler::spec::command_is_known_test_runner;
use oz_steward::butler::{compose, ButlerStore, Reversibility, Task};
use oz_steward::goal::spec::GoalGate;

use crate::registry::ToolHandler;

fn store() -> Result<ButlerStore, ToolError> {
    let path = std::env::var("OPENZEN_BUTLER_DB")
        .map_err(|_| ToolError::Custom("butler store is not configured".into()))?;
    ButlerStore::open(path).map_err(|e| ToolError::Custom(format!("butler store: {e}")))
}

fn project_key(ctx: &ToolContext) -> String {
    ctx.working_dir.clone()
}

fn projects_file() -> Option<PathBuf> {
    std::env::var("OPENZEN_PROJECTS_FILE")
        .ok()
        .map(PathBuf::from)
        .or_else(|| {
            std::env::var("OPENZEN_DATA_DIR")
                .ok()
                .map(|d| PathBuf::from(d).join("projects.json"))
        })
}

fn load_projects() -> Vec<serde_json::Value> {
    let Some(path) = projects_file() else {
        return Vec::new();
    };
    let Ok(text) = std::fs::read_to_string(path) else {
        return Vec::new();
    };
    serde_json::from_str::<Vec<serde_json::Value>>(&text).unwrap_or_default()
}

/// Resolve a project by id, exact name, or root path (in that order), then by
/// case-insensitive name. Returns `(id, name, root)`.
fn resolve_project(raw: &str) -> Result<(String, String, String), ToolError> {
    let raw = raw.trim();
    if raw.is_empty() {
        return Err(ToolError::Custom("task_create: `project` is empty".into()));
    }
    let projects = load_projects();
    let field = |p: &serde_json::Value, key: &str| {
        p.get(key)
            .and_then(|v| v.as_str())
            .unwrap_or_default()
            .to_string()
    };
    let exact = projects
        .iter()
        .find(|p| field(p, "id") == raw || field(p, "name") == raw || field(p, "root_path") == raw);
    let hit = exact.or_else(|| {
        let lower = raw.to_lowercase();
        projects
            .iter()
            .find(|p| field(p, "name").to_lowercase() == lower)
    });
    match hit {
        Some(p) => Ok((field(p, "id"), field(p, "name"), field(p, "root_path"))),
        None => {
            let names: Vec<String> = projects
                .iter()
                .take(12)
                .map(|p| format!("{} ({})", field(p, "name"), field(p, "root_path")))
                .collect();
            Err(ToolError::Custom(format!(
                "task_create: unknown project `{raw}`. Available: {}",
                if names.is_empty() {
                    "(none registered)".to_string()
                } else {
                    names.join(", ")
                }
            )))
        }
    }
}

pub struct ProjectListTool;

#[async_trait]
impl ToolHandler for ProjectListTool {
    fn name(&self) -> String {
        "project_list".into()
    }
    fn description(&self) -> String {
        "List registered projects: id, name, and root path. Call this before task_create when a task belongs to a project; pass the project name/id/path as `project`.".into()
    }
    fn description_zh(&self) -> String {
        "列出已登记项目：id、名称、根目录。任务属于某个项目时，先调用它，再把项目名/id/路径传给 task_create 的 project。".into()
    }
    fn parameters(&self) -> serde_json::Value {
        serde_json::json!({"type": "object", "properties": {
            "status": {"type": "string", "description": "Optional status filter"},
            "limit": {"type": "integer", "description": "Max rows (default 10)"},
            "offset": {"type": "integer", "description": "Skip N rows"}
        }})
    }
    async fn execute(
        &self,
        _args: serde_json::Value,
        _ctx: &ToolContext,
    ) -> Result<ToolOutput, ToolError> {
        let rows: Vec<serde_json::Value> = load_projects()
            .iter()
            .map(|p| {
                serde_json::json!({
                    "id": p.get("id").and_then(|v| v.as_str()).unwrap_or_default(),
                    "name": p.get("name").and_then(|v| v.as_str()).unwrap_or_default(),
                    "root_path": p.get("root_path").and_then(|v| v.as_str()).unwrap_or_default(),
                })
            })
            .collect();
        Ok(ToolOutput::success(serde_json::json!({"projects": rows})))
    }
}

fn preview(task: &Task) -> serde_json::Value {
    serde_json::json!({
        "task_id": task.id,
        "status": task.status.as_str(),
        "gates": task.accept.iter().map(|g| format!("{g:?}")).collect::<Vec<_>>(),
    })
}

/// Explicit verification commands execute later as the task author. By
/// default only known test/build runners are accepted; arbitrary scripts need
/// the process-level OPENZEN_ALLOW_CUSTOM_VERIFY opt-in.
fn validate_verify_commands(gates: &[GoalGate]) -> Result<(), String> {
    for gate in gates {
        match gate {
            GoalGate::StrongCommand { command, .. } => {
                if !oz_steward::butler::spec::verify_command_shape_is_safe(command) {
                    return Err(format!(
                        "task_create: verify command must be a single command (shell operators/redirects refused): `{command}`"
                    ));
                }
                if !command_is_known_test_runner(command)
                    && !oz_steward::butler::spec::custom_verify_commands_allowed()
                {
                    return Err(format!(
                        "task_create: verify command is not a known test runner: `{command}`. Use cargo/npm/pytest/go test/tsc, or set OPENZEN_ALLOW_CUSTOM_VERIFY=1 to explicitly allow custom scripts."
                    ));
                }
            }
            GoalGate::All { gates } | GoalGate::Any { gates } => {
                validate_verify_commands(gates)?;
            }
            _ => {}
        }
    }
    Ok(())
}

/// Reject deps that do not exist, belong to another project, or would create
/// a cycle through the new task. The scheduler can then trust every dep id.
fn validate_deps(store: &ButlerStore, task: &Task) -> Result<(), ToolError> {
    if task.deps.is_empty() {
        return Ok(());
    }
    let mut stack = task.deps.clone();
    let mut seen = std::collections::HashSet::new();
    while let Some(id) = stack.pop() {
        if id == task.id {
            return Err(ToolError::Custom(
                "task_create: dependency cycle (task depends on itself)".into(),
            ));
        }
        if !seen.insert(id.clone()) {
            continue;
        }
        let Some(dep) = store
            .get(&id)
            .map_err(|e| ToolError::Custom(e.to_string()))?
        else {
            return Err(ToolError::Custom(format!(
                "task_create: dependency `{id}` does not exist; create it first"
            )));
        };
        if dep.project_id != task.project_id {
            return Err(ToolError::Custom(format!(
                "task_create: dependency `{id}` belongs to project `{}`, not `{}`",
                dep.project_id, task.project_id
            )));
        }
        for next in dep.deps {
            if next == task.id {
                return Err(ToolError::Custom(
                    "task_create: dependency cycle through the new task".into(),
                ));
            }
            stack.push(next);
        }
    }
    Ok(())
}

pub struct TaskCreateTool;

#[async_trait]
impl ToolHandler for TaskCreateTool {
    fn name(&self) -> String {
        "task_create".into()
    }
    fn description(&self) -> String {
        "Create ONE butler task in the task graph. The task is queued; the scheduler dispatches it to a background worker and an independent verifier decides completion. Give a concrete goal and the files it should produce. Acceptance gates are derived from the workspace and deliverables — do not write your own pass/fail claim.".into()
    }
    fn description_zh(&self) -> String {
        "在任务图里创建【一条】管家任务。任务进入队列后由调度器派给后台工人，完成与否由独立验收器判定。goal 要具体，deliverables 写清要产出的文件；验收门由系统从工作区和交付物推导，不要自己宣称完成。".into()
    }
    fn parameters(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "goal": { "type": "string", "description": "One concrete, verifiable task" },
                "detail": { "type": "string", "description": "Constraints, files to read, what to return" },
                "deps": { "type": "array", "items": { "type": "string" }, "description": "task ids that must be verified first" },
                "deliverables": { "type": "array", "items": { "type": "string" }, "description": "workspace-relative files this task must produce" },
                "project": { "type": "string", "description": "Project name, id, or root path. Omit for the shared workspace." },
                "model": { "type": "string", "description": "Optional per-task model override" },
                "reversibility": { "type": "string", "enum": ["read_only", "workspace_write", "outside_write", "external_effect", "destructive"], "description": "Default workspace_write. Anything requiring approval is queued, not blocked." }
            },
            "required": ["goal"]
        })
    }
    async fn execute(
        &self,
        args: serde_json::Value,
        ctx: &ToolContext,
    ) -> Result<ToolOutput, ToolError> {
        let goal = args
            .get("goal")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .trim();
        if goal.is_empty() {
            return Ok(ToolOutput::bad_json("task_create: `goal` is required"));
        }
        let detail = args.get("detail").and_then(|v| v.as_str()).unwrap_or("");
        let deps: Vec<String> = args
            .get("deps")
            .and_then(|v| v.as_array())
            .map(|a| {
                a.iter()
                    .filter_map(|x| x.as_str().map(String::from))
                    .collect()
            })
            .unwrap_or_default();
        let deliverables: Vec<String> = args
            .get("deliverables")
            .and_then(|v| v.as_array())
            .map(|a| {
                a.iter()
                    .filter_map(|x| x.as_str().map(String::from))
                    .collect()
            })
            .unwrap_or_default();
        let model = args.get("model").and_then(|v| v.as_str()).map(String::from);
        let requested_reversibility = args
            .get("reversibility")
            .and_then(|v| v.as_str())
            .and_then(|s| Reversibility::parse(s).ok())
            .unwrap_or_default();
        // Workers have full write/exec capabilities, so "read_only" is a
        // label, not a capability. Never let the model downgrade below
        // workspace_write, and promote clearly destructive work.
        let text = format!("{goal}\n{detail}").to_lowercase();
        let destructive = [
            "rm -rf",
            "rm -r",
            "删除",
            "drop table",
            "truncate",
            "发布",
            "部署",
            "上传",
            "外发",
            "覆盖安装",
            "覆盖现有",
            "git push --force",
        ]
        .iter()
        .any(|k| text.contains(k));
        let reversibility = if destructive {
            Reversibility::Destructive
        } else if requested_reversibility == Reversibility::ReadOnly {
            Reversibility::WorkspaceWrite
        } else {
            requested_reversibility
        };

        let (root, project_name) = match args.get("project").and_then(|v| v.as_str()) {
            Some(raw) if !raw.trim().is_empty() => {
                let (_id, name, root) = resolve_project(raw)?;
                (root, Some(name))
            }
            _ => (project_key(ctx), None),
        };

        // A project-bound caller must not create work in another project;
        // otherwise a worker/butler prompt injection can open an arbitrary
        // project's workspace with full capabilities.
        let current_is_project = load_projects()
            .iter()
            .any(|p| p.get("root_path").and_then(|v| v.as_str()) == Some(ctx.working_dir.as_str()));
        if (ctx.is_subagent || current_is_project)
            && Path::new(&root) != Path::new(&ctx.working_dir)
        {
            return Ok(ToolOutput::bad_json(format!(
                "task_create: project `{root}` is outside the caller's project `{}`; cross-project dispatch needs an explicit user action",
                ctx.working_dir
            )));
        }
        let mut task = Task::new(&root, ctx.session_id.clone(), goal);
        task.project_name = project_name;
        task.detail = detail.to_string();
        task.deps = deps;
        let verify_cmds: Vec<String> = args
            .get("verify")
            .and_then(|v| v.as_array())
            .map(|a| {
                a.iter()
                    .filter_map(|v| v.as_str())
                    .map(str::trim)
                    .filter(|s| !s.is_empty())
                    .take(8)
                    .map(str::to_string)
                    .collect()
            })
            .unwrap_or_default();
        // A data chain that supplies its own strong checks is safe to make
        // strict by default: the dependency must be verified before the child
        // consumes its output. An explicit false still wins for legacy flows.
        task.require_verified_deps = args
            .get("strict_deps")
            .and_then(|v| v.as_bool())
            .unwrap_or(!task.deps.is_empty() && !verify_cmds.is_empty());
        task.model = model;
        task.reversibility = reversibility;
        let mut compose_goal = goal.to_string();
        for cmd in &verify_cmds {
            compose_goal.push_str("\n[verify] ");
            compose_goal.push_str(cmd);
        }
        task.accept = compose(&compose_goal, Path::new(&root), &deliverables);
        if let Err(e) = validate_verify_commands(&task.accept) {
            return Ok(ToolOutput::bad_json(e));
        }
        let s = store()?;
        // Dependencies are a contract, not model-supplied labels. Reject
        // missing/cross-project/cyclic deps at creation; silently treating a
        // missing dep as "satisfied" lets a worker run before its input exists.
        validate_deps(&s, &task)?;
        if let Some(dup) = s
            .list(Some(&task.project_id), 500)
            .map_err(|e| ToolError::Custom(e.to_string()))?
            .into_iter()
            .find(|t| !t.status.is_terminal() && t.goal == task.goal)
        {
            return Ok(ToolOutput::bad_json(format!(
                "task_create: an open task with the same goal already exists: {}",
                dup.id
            )));
        }
        s.create_task(&task)
            .map_err(|e| ToolError::Custom(e.to_string()))?;
        let _ = s.clear_digest(&task.project_id);
        let _ = s.append_event(&task.id, "created", goal);
        Ok(ToolOutput::success(preview(&task)))
    }
}

pub struct TaskListTool;

#[async_trait]
impl ToolHandler for TaskListTool {
    fn name(&self) -> String {
        "task_list".into()
    }
    fn description(&self) -> String {
        "List butler tasks in this workspace: id, goal, status, attempts, dependencies, and the recorded assumptions/skipped items. Use it before creating follow-up tasks so you do not duplicate work.".into()
    }
    fn description_zh(&self) -> String {
        "列出当前工作区的管家任务：id、目标、状态、尝试次数、依赖，以及已记录的假设/跳过项。拆新任务前先看一眼，避免重复。".into()
    }
    fn parameters(&self) -> serde_json::Value {
        serde_json::json!({"type": "object", "properties": {
            "status": {"type": "string"},
            "limit": {"type": "integer"},
            "offset": {"type": "integer"}
        }})
    }
    async fn execute(
        &self,
        args: serde_json::Value,
        ctx: &ToolContext,
    ) -> Result<ToolOutput, ToolError> {
        let s = store()?;
        let limit = args
            .get("limit")
            .and_then(|v| v.as_u64())
            .unwrap_or(10)
            .clamp(1, 50) as usize;
        let offset = args.get("offset").and_then(|v| v.as_u64()).unwrap_or(0) as usize;
        let status_filter = args
            .get("status")
            .and_then(|v| v.as_str())
            .map(str::to_string);
        let tasks = s
            .list(Some(&project_key(ctx)), 200)
            .map_err(|e| ToolError::Custom(e.to_string()))?;
        let rows: Vec<serde_json::Value> = tasks
            .iter()
            .filter(|t| {
                status_filter
                    .as_deref()
                    .map(|f| t.status.as_str() == f)
                    .unwrap_or(true)
            })
            .skip(offset)
            .take(limit)
            .map(|t| {
                serde_json::json!({
                    "id": t.id.chars().take(8).collect::<String>(),
                    "goal": t.goal.chars().take(60).collect::<String>(),
                    "status": t.status.as_str(),
                    "project": t.project_name,
                    "attempt": t.attempt,
                })
            })
            .collect();
        Ok(ToolOutput::success(
            serde_json::json!({"tasks": rows, "total": tasks.len()}),
        ))
    }
}

pub struct TaskNoteTool;

#[async_trait]
impl ToolHandler for TaskNoteTool {
    fn name(&self) -> String {
        "task_note".into()
    }
    fn description(&self) -> String {
        "Record one assumption, skipped item, or free note against a task. Assumptions are what let you continue without asking the user; they are shown in the final digest. Never record a verification result here.".into()
    }
    fn description_zh(&self) -> String {
        "给某条任务记一条「假设 / 跳过 / 备注」。假设是你不打断用户继续做的依据，会出现在最终 digest 里。不要在这里记验收结论。".into()
    }
    fn parameters(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "task_id": { "type": "string" },
                "kind": { "type": "string", "enum": ["assumption", "skipped", "note"] },
                "text": { "type": "string" }
            },
            "required": ["task_id", "text"]
        })
    }
    async fn execute(
        &self,
        args: serde_json::Value,
        _ctx: &ToolContext,
    ) -> Result<ToolOutput, ToolError> {
        let id = args.get("task_id").and_then(|v| v.as_str()).unwrap_or("");
        let kind = args.get("kind").and_then(|v| v.as_str()).unwrap_or("note");
        let text = args
            .get("text")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .trim();
        if id.is_empty() || text.is_empty() {
            return Ok(ToolOutput::bad_json(
                "task_note: `task_id` and `text` are required",
            ));
        }
        let s = store()?;
        s.add_note(id, kind, text)
            .map_err(|e| ToolError::Custom(e.to_string()))?;
        Ok(ToolOutput::success(serde_json::json!({"ok": true})))
    }
}

#[linkme::distributed_slice(crate::registry::TOOL_FACTORIES)]
pub fn register_butler_tools(reg: &mut crate::registry::ToolRegistry) {
    reg.register(TaskCreateTool);
    reg.register(TaskListTool);
    reg.register(TaskNoteTool);
}

pub struct TaskGetTool;

#[async_trait]
impl ToolHandler for TaskGetTool {
    fn name(&self) -> String {
        "task_get".into()
    }
    fn description(&self) -> String {
        "Read one butler task in full (goal, detail, status, gates, evidence, assumptions, skipped). Use after task_list when detail is needed.".into()
    }
    fn description_zh(&self) -> String {
        "读取单条管家任务的完整信息（目标、细节、状态、验收门、证据、假设、跳过项）。先 task_list，再按需 task_get。".into()
    }
    fn parameters(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "task_id": { "type": "string", "description": "Task id (full or 8-char prefix)" }
            },
            "required": ["task_id"]
        })
    }
    async fn execute(
        &self,
        args: serde_json::Value,
        ctx: &ToolContext,
    ) -> Result<ToolOutput, ToolError> {
        let id = args
            .get("task_id")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .trim();
        if id.is_empty() {
            return Ok(ToolOutput::bad_json("task_get: `task_id` is required"));
        }
        let s = store()?;
        let task = if id.len() >= 32 {
            s.get(id).map_err(|e| ToolError::Custom(e.to_string()))?
        } else {
            s.list(Some(&ctx.working_dir), 5000)
                .map_err(|e| ToolError::Custom(e.to_string()))?
                .into_iter()
                .find(|t| t.id.starts_with(id))
        };
        if let Some(t) = &task {
            if t.project_id != ctx.working_dir {
                return Ok(ToolOutput::bad_json(
                    "task_get: task belongs to another project",
                ));
            }
        }
        match task {
            Some(t) => Ok(ToolOutput::success(serde_json::json!({
                "id": t.id,
                "project": t.project_name,
                "project_root": t.project_id,
                "goal": t.goal,
                "detail": t.detail,
                "status": t.status.as_str(),
                "attempt": t.attempt,
                "deps": t.deps,
                "gates": t.accept.iter().map(|g| format!("{g:?}")).collect::<Vec<_>>(),
                "evidence": t.evidence.iter().map(|g| format!("{g:?}")).collect::<Vec<_>>(),
                "assumptions": t.assumptions,
                "skipped": t.skipped,
            }))),
            None => Ok(ToolOutput::bad_json(format!("task_get: no task `{id}`"))),
        }
    }
}

pub struct RunListTool;

#[async_trait]
impl ToolHandler for RunListTool {
    fn name(&self) -> String {
        "run_list".into()
    }
    fn description(&self) -> String {
        "List recent worker runs (id, task, project, model, status, duration, summary). Use it to recover what happened before without relying on chat history.".into()
    }
    fn description_zh(&self) -> String {
        "列出最近的工人运行记录（id、任务、项目、模型、状态、耗时、摘要）。用于在不翻聊天记录的情况下回顾之前发生了什么。".into()
    }
    fn parameters(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "limit": {"type": "integer", "description": "Max rows (default 10)"}
            }
        })
    }
    async fn execute(
        &self,
        args: serde_json::Value,
        ctx: &ToolContext,
    ) -> Result<ToolOutput, ToolError> {
        let limit = args
            .get("limit")
            .and_then(|v| v.as_u64())
            .unwrap_or(10)
            .clamp(1, 50) as usize;
        let runs = store()?
            .list_runs(Some(&ctx.working_dir), limit)
            .map_err(|e| ToolError::Custom(e.to_string()))?;
        let rows: Vec<serde_json::Value> = runs
            .iter()
            .map(|r| {
                let secs = r
                    .finished_at
                    .map(|f| (f - r.started_at).max(0) / 1000)
                    .unwrap_or(0);
                serde_json::json!({
                    "run_id": r.run_id.chars().take(8).collect::<String>(),
                    "task_id": r.task_id.chars().take(8).collect::<String>(),
                    "project": r.project_id.rsplit('/').next().unwrap_or(&r.project_id),
                    "status": r.status,
                    "secs": secs,
                    "summary": r.summary.chars().take(160).collect::<String>(),
                })
            })
            .collect();
        Ok(ToolOutput::success(serde_json::json!({"runs": rows})))
    }
}

pub struct ArtifactFindTool;

#[async_trait]
impl ToolHandler for ArtifactFindTool {
    fn name(&self) -> String {
        "artifact_find".into()
    }
    fn description(&self) -> String {
        "Find files by name substring under a directory (default: the current workspace). Returns paths, newest first. Use before claiming an artifact exists.".into()
    }
    fn description_zh(&self) -> String {
        "按文件名子串在目录下找文件（默认当前工作区），返回路径，新的在前。宣称产物存在前先用它核实。".into()
    }
    fn parameters(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "name": {"type": "string", "description": "Filename substring, case-insensitive"},
                "root": {"type": "string", "description": "Optional directory to search"}
            },
            "required": ["name"]
        })
    }
    async fn execute(
        &self,
        args: serde_json::Value,
        ctx: &ToolContext,
    ) -> Result<ToolOutput, ToolError> {
        let needle = args
            .get("name")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_lowercase();
        if needle.trim().is_empty() {
            return Ok(ToolOutput::bad_json("artifact_find: `name` is required"));
        }
        let root = std::path::PathBuf::from(
            args.get("root")
                .and_then(|v| v.as_str())
                .unwrap_or(&ctx.working_dir),
        );
        let root_real = oz_core_types::path_trust::resolve_boundary_path(&root);
        let wd_real = oz_core_types::path_trust::resolve_boundary_path(std::path::Path::new(
            &ctx.working_dir,
        ));
        if !root_real.starts_with(&wd_real) {
            return Ok(ToolOutput::bad_json(
                "artifact_find: root must be inside the current workspace",
            ));
        }
        let mut found: Vec<(u64, String)> = Vec::new();
        let mut stack = vec![root];
        while let Some(dir) = stack.pop() {
            if found.len() >= 40 {
                break;
            }
            let Ok(entries) = std::fs::read_dir(&dir) else {
                continue;
            };
            for entry in entries.flatten() {
                let path = entry.path();
                let name = entry.file_name().to_string_lossy().to_lowercase();
                if name.starts_with('.') || name == "node_modules" || name == "target" {
                    continue;
                }
                if path.is_dir() {
                    stack.push(path);
                } else if name.contains(&needle) {
                    let mtime = entry
                        .metadata()
                        .and_then(|m| m.modified())
                        .ok()
                        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                        .map(|d| d.as_millis() as u64)
                        .unwrap_or(0);
                    found.push((mtime, path.to_string_lossy().to_string()));
                }
            }
        }
        found.sort_by_key(|b| std::cmp::Reverse(b.0));
        let paths: Vec<String> = found.into_iter().take(20).map(|(_, p)| p).collect();
        Ok(ToolOutput::success(serde_json::json!({"paths": paths})))
    }
}

pub struct HistorySearchTool;

#[async_trait]
impl ToolHandler for HistorySearchTool {
    fn name(&self) -> String {
        "history_search".into()
    }
    fn description(&self) -> String {
        "Search archived chat history by keyword (all sessions). Returns session, role, and a short snippet. Use it when the user refers to something older than the current context window.".into()
    }
    fn description_zh(&self) -> String {
        "按关键词检索归档聊天历史（所有会话），返回会话、角色和短片段。用户提到超出当前上下文窗口的旧事时使用。".into()
    }
    fn parameters(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "query": {"type": "string", "description": "Keyword or phrase"},
                "limit": {"type": "integer", "description": "Max hits (default 8)"}
            },
            "required": ["query"]
        })
    }
    async fn execute(
        &self,
        args: serde_json::Value,
        ctx: &ToolContext,
    ) -> Result<ToolOutput, ToolError> {
        let q = args
            .get("query")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .trim()
            .to_lowercase();
        if q.is_empty() {
            return Ok(ToolOutput::bad_json("history_search: `query` is required"));
        }
        let limit = args
            .get("limit")
            .and_then(|v| v.as_u64())
            .unwrap_or(8)
            .clamp(1, 30) as usize;
        let path = std::env::var("OPENZEN_SESSIONS_FILE").map_err(|_| {
            ToolError::Custom("history_search: sessions file not configured".into())
        })?;
        let text = std::fs::read_to_string(path)
            .map_err(|e| ToolError::Custom(format!("history_search: {e}")))?;
        let data: serde_json::Value = serde_json::from_str(&text)
            .map_err(|e| ToolError::Custom(format!("history_search: {e}")))?;
        let project_id = load_projects().into_iter().find_map(|p| {
            (p.get("root_path").and_then(|v| v.as_str()) == Some(ctx.working_dir.as_str()))
                .then(|| p.get("id").and_then(|v| v.as_str()).map(str::to_string))
                .flatten()
        });
        let mut hits = Vec::new();
        if let Some(sessions) = data.as_object() {
            for (_id, entry) in sessions {
                let entry_project = entry
                    .get("info")
                    .and_then(|i| i.get("project_id"))
                    .and_then(|v| v.as_str());
                match (project_id.as_deref(), entry_project) {
                    (Some(want), Some(got)) if want == got => {}
                    (None, None) if _id == &ctx.session_id => {}
                    _ => continue,
                }
                let name = entry
                    .get("info")
                    .and_then(|i| i.get("name"))
                    .and_then(|v| v.as_str())
                    .unwrap_or("?");
                let role = entry
                    .get("info")
                    .and_then(|i| i.get("role"))
                    .and_then(|v| v.as_str())
                    .or_else(|| entry.get("role").and_then(|v| v.as_str()))
                    .unwrap_or("worker");
                if let Some(msgs) = entry.get("messages").and_then(|m| m.as_array()) {
                    for m in msgs {
                        let content = m
                            .get("content")
                            .and_then(|c| c.as_str())
                            .or_else(|| {
                                m.get("content")
                                    .and_then(|c| c.as_array())
                                    .and_then(|a| a.first())
                                    .and_then(|b| b.get("text"))
                                    .and_then(|t| t.as_str())
                            })
                            .unwrap_or("");
                        if let Some(pos) = content.to_lowercase().find(&q) {
                            let start = content[..pos]
                                .char_indices()
                                .rev()
                                .nth(30)
                                .map(|(i, _)| i)
                                .unwrap_or(0);
                            let end = content[pos..]
                                .char_indices()
                                .nth(120)
                                .map(|(i, _)| pos + i)
                                .unwrap_or_else(|| content.len());
                            hits.push(serde_json::json!({
                                "session": name,
                                "role": role,
                                "snippet": content[start..end].replace('\n', " "),
                            }));
                            if hits.len() >= limit {
                                break;
                            }
                        }
                    }
                }
                if hits.len() >= limit {
                    break;
                }
            }
        }
        Ok(ToolOutput::success(serde_json::json!({"hits": hits})))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    static ENV_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

    #[test]
    fn resolves_project_by_name_id_or_root_and_lists_candidates() {
        let dir = std::env::temp_dir().join(format!("oz-projects-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("projects.json");
        std::fs::write(
            &path,
            r#"[{"id":"p1","name":"visualmusic","root_path":"/tmp/visualmusic","created_at":"x"},
                 {"id":"p2","name":"openzen","root_path":"/tmp/openzen","created_at":"x"}]"#,
        )
        .unwrap();
        std::env::set_var("OPENZEN_PROJECTS_FILE", &path);
        assert_eq!(
            resolve_project("visualmusic").unwrap().2,
            "/tmp/visualmusic"
        );
        assert_eq!(resolve_project("p2").unwrap().1, "openzen");
        assert_eq!(resolve_project("/tmp/visualmusic").unwrap().0, "p1");
        let err = resolve_project("missing").unwrap_err().to_string();
        assert!(err.contains("unknown project"));
        assert!(err.contains("visualmusic"));
        std::env::remove_var("OPENZEN_PROJECTS_FILE");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn missing_store_is_an_actionable_error() {
        let _guard = ENV_LOCK.lock().await;
        std::env::remove_var("OPENZEN_BUTLER_DB");
        match store() {
            Ok(_) => panic!("store should be unavailable"),
            Err(e) => assert!(e.to_string().contains("not configured")),
        }
    }

    #[test]
    fn dependency_validation_rejects_missing_cross_project_and_cycles() {
        let store = ButlerStore::open_memory().unwrap();
        let mut task = Task::new("/p", "b", "new");
        task.deps = vec!["missing".into()];
        assert!(validate_deps(&store, &task).is_err());

        let other = Task::new("/other", "b", "other");
        store.create_task(&other).unwrap();
        task.deps = vec![other.id.clone()];
        assert!(validate_deps(&store, &task).is_err());

        let a = Task::new("/p", "b", "a");
        store.create_task(&a).unwrap();
        let mut b = Task::new("/p", "b", "b");
        b.deps = vec![a.id.clone()];
        store.create_task(&b).unwrap();
        let mut cycle = a.clone();
        cycle.deps = vec![b.id.clone()];

        let mut candidate = Task::new("/p", "b", "candidate");
        candidate.deps = vec![b.id.clone()];
        assert!(validate_deps(&store, &candidate).is_ok());
        assert!(validate_deps(&store, &cycle).is_err());
    }

    #[tokio::test]
    async fn verify_commands_make_a_dependent_task_strict() {
        use oz_steward::goal::spec::GoalGate;
        let _guard = ENV_LOCK.lock().await;
        let dir = std::env::temp_dir().join(format!("oz-verify-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let db = dir.join("butler.db");
        std::env::set_var("OPENZEN_BUTLER_DB", &db);
        let ctx = ToolContext {
            working_dir: "/p".into(),
            assets_dir: String::new(),
            script_dir: String::new(),
            lang: "en".into(),
            skill_mcp_dir: None,
            harness_dir: None,
            session_id: "s".into(),
            tool_budget_secs: 0,
            is_subagent: false,
        };
        let first = TaskCreateTool
            .execute(serde_json::json!({"goal": "A"}), &ctx)
            .await
            .unwrap();
        let a_id = first.data["task_id"].as_str().unwrap().to_string();
        let second = TaskCreateTool
            .execute(
                serde_json::json!({
                    "goal": "B",
                    "deps": [a_id],
                    "verify": ["cargo test --quiet"],
                }),
                &ctx,
            )
            .await
            .unwrap();
        let b_id = second.data["task_id"].as_str().unwrap();
        let store = ButlerStore::open(&db).unwrap();
        let b = store.get(b_id).unwrap().unwrap();
        assert!(
            b.require_verified_deps,
            "verify + deps must default to strict"
        );
        assert!(
            b.accept
                .iter()
                .any(|g| matches!(g, GoalGate::StrongCommand { .. })),
            "{:?}",
            b.accept
        );
        std::env::remove_var("OPENZEN_BUTLER_DB");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn custom_verify_command_is_rejected_at_creation() {
        let _guard = ENV_LOCK.lock().await;
        let dir = std::env::temp_dir().join(format!("oz-custom-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let db = dir.join("butler.db");
        std::env::set_var("OPENZEN_BUTLER_DB", &db);
        let ctx = ToolContext {
            working_dir: "/p".into(),
            assets_dir: String::new(),
            script_dir: String::new(),
            lang: "en".into(),
            skill_mcp_dir: None,
            harness_dir: None,
            session_id: "s".into(),
            tool_budget_secs: 0,
            is_subagent: false,
        };
        let out = TaskCreateTool
            .execute(
                serde_json::json!({
                    "goal": "dangerous",
                    "verify": ["python3 -c \"print(1)\""],
                }),
                &ctx,
            )
            .await
            .unwrap();
        assert_eq!(out.data["status"], "error", "{}", out.data);
        assert!(
            out.data["error"]
                .as_str()
                .unwrap_or("")
                .contains("not a known test runner"),
            "{}",
            out.data
        );
        let chained = TaskCreateTool
            .execute(
                serde_json::json!({
                    "goal": "chained",
                    "verify": ["cargo test --quiet; touch pwned"],
                }),
                &ctx,
            )
            .await
            .unwrap();
        assert_eq!(chained.data["status"], "error", "{}", chained.data);
        assert!(
            chained.data["error"]
                .as_str()
                .unwrap_or("")
                .contains("single command"),
            "{}",
            chained.data
        );
        std::env::remove_var("OPENZEN_BUTLER_DB");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn duplicate_open_goal_is_rejected() {
        let _guard = ENV_LOCK.lock().await;
        let dir = std::env::temp_dir().join(format!("oz-dup-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let db = dir.join("butler.db");
        std::env::set_var("OPENZEN_BUTLER_DB", &db);
        let ctx = ToolContext {
            working_dir: "/p".into(),
            assets_dir: String::new(),
            script_dir: String::new(),
            lang: "en".into(),
            skill_mcp_dir: None,
            harness_dir: None,
            session_id: "s".into(),
            tool_budget_secs: 0,
            is_subagent: false,
        };
        let args = serde_json::json!({"goal": "only once"});
        let first = TaskCreateTool.execute(args.clone(), &ctx).await.unwrap();
        assert_eq!(first.data["status"], "queued", "{}", first.data);
        let second = TaskCreateTool.execute(args, &ctx).await.unwrap();
        assert_eq!(second.data["status"], "error");
        assert!(
            second.data["error"]
                .as_str()
                .unwrap_or("")
                .contains("same goal"),
            "{}",
            second.data
        );
        std::env::remove_var("OPENZEN_BUTLER_DB");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
