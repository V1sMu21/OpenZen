//! Model-facing background-job controls: `job_output`, `job_list`, `job_kill`.
//!
//! Every call is owner-fenced by `ToolContext.session_id`; a foreign id is
//! reported as not found. Reads are consuming (the producer hands back only
//! what is new since the previous read), and terminal reads suppress the
//! completion notice so the model is not told twice.

use std::time::Duration;

use async_trait::async_trait;
use oz_core::jobs::{global, JobSnapshot, JobStatus};
use oz_core_types::{ToolContext, ToolError, ToolOutput};

use crate::registry::ToolHandler;

// Below the loop's outer tool budget (330 s), otherwise the outer timeout
// fires first and the model gets an unrelated error while the job runs on.
const MAX_WAIT_MS: u64 = 300_000;
const KILL_WAIT: Duration = Duration::from_secs(15);

fn owner(ctx: &ToolContext) -> Result<&str, String> {
    if ctx.session_id.is_empty() {
        Err("job tools need a session id".into())
    } else {
        Ok(&ctx.session_id)
    }
}

fn status_suffix(snap: &JobSnapshot) -> String {
    let detail = snap
        .detail
        .as_deref()
        .map(|d| format!(", {d}"))
        .unwrap_or_default();
    format!("[status: {}{}]", snap.status.as_str(), detail)
}

pub struct JobOutputTool;

#[async_trait]
impl ToolHandler for JobOutputTool {
    fn name(&self) -> String {
        "job_output".into()
    }
    fn description(&self) -> String {
        "Read a background job's output. Stream jobs return only output produced since the previous read. Set wait=true to block up to timeout_ms for the job to finish; a timeout leaves it running. Every response ends with [status: ...].".into()
    }
    fn description_zh(&self) -> String {
        "读取后台任务的输出。流式任务只返回上次读取之后的新输出；wait=true 可最多等待 timeout_ms，超时不会终止任务。响应末尾带 [status: ...]。".into()
    }
    fn parameters(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "job_id": { "type": "string", "description": "Job id, e.g. bash-1" },
                "wait": { "type": "boolean", "description": "Wait for the job to settle (default false)" },
                "timeout_ms": { "type": "integer", "description": "Max wait when wait=true (default 30000, max 600000)" }
            },
            "required": ["job_id"]
        })
    }

    async fn execute(
        &self,
        args: serde_json::Value,
        ctx: &ToolContext,
    ) -> Result<ToolOutput, ToolError> {
        let id = args.get("job_id").and_then(|v| v.as_str()).unwrap_or("");
        if id.is_empty() {
            return Ok(ToolOutput::bad_json("job_output: missing `job_id`"));
        }
        let owner = match owner(ctx) {
            Ok(o) => o,
            Err(e) => return Ok(ToolOutput::bad_json(e)),
        };
        let registry = global();
        let wait = args.get("wait").and_then(|v| v.as_bool()).unwrap_or(false);
        let timeout_ms = args
            .get("timeout_ms")
            .and_then(|v| v.as_u64())
            .unwrap_or(30_000)
            .min(MAX_WAIT_MS);

        let snap = match registry.get(id) {
            Some(s) if s.owner_session == owner => s,
            _ => {
                return Ok(ToolOutput::bad_json(format!(
                    "job_output: job {id} not found"
                )))
            }
        };
        if wait && !snap.status.is_terminal() {
            // The read below returns the settled snapshot too; the wait only
            // decides *when* we read, so its return value is not stored.
            registry
                .wait(id, owner, Duration::from_millis(timeout_ms))
                .await
                .map_err(ToolError::Custom)?;
        }
        let (text, snap) = registry.read(id, owner).map_err(ToolError::Custom)?;
        if snap.status.is_terminal() {
            registry.mark_reported(id);
        }
        let body = if text.is_empty() {
            "(no new output)".to_string()
        } else {
            text
        };
        let rendered = format!("{body}\n{}", status_suffix(&snap));
        Ok(ToolOutput::success_with_prompt(
            serde_json::json!({
                "job": snap.to_json(),
                "text": body,
                "status": snap.status.as_str(),
            }),
            rendered,
        ))
    }
}

pub struct JobListTool;

#[async_trait]
impl ToolHandler for JobListTool {
    fn name(&self) -> String {
        "job_list".into()
    }
    fn description(&self) -> String {
        "List your background jobs with id, kind, status and label.".into()
    }
    fn description_zh(&self) -> String {
        "列出本会话的后台任务：id、类型、状态与标签。".into()
    }
    fn parameters(&self) -> serde_json::Value {
        serde_json::json!({ "type": "object", "properties": {} })
    }

    async fn execute(
        &self,
        _args: serde_json::Value,
        ctx: &ToolContext,
    ) -> Result<ToolOutput, ToolError> {
        let owner = match owner(ctx) {
            Ok(o) => o,
            Err(e) => return Ok(ToolOutput::bad_json(e)),
        };
        let jobs = global().list(owner);
        if jobs.is_empty() {
            return Ok(ToolOutput::success_with_prompt(
                serde_json::json!({ "jobs": [] }),
                "No background jobs.".to_string(),
            ));
        }
        let lines: Vec<String> = jobs
            .iter()
            .map(|j| {
                let detail = j.detail.as_deref().unwrap_or("");
                format!(
                    "{} [{}] {}{} — {}",
                    j.id,
                    j.kind.as_str(),
                    j.status.as_str(),
                    if detail.is_empty() {
                        String::new()
                    } else {
                        format!(" ({detail})")
                    },
                    j.label
                )
            })
            .collect();
        Ok(ToolOutput::success_with_prompt(
            serde_json::json!({ "jobs": jobs.iter().map(|j| j.to_json()).collect::<Vec<_>>() }),
            lines.join("\n"),
        ))
    }
}

pub struct JobKillTool;

#[async_trait]
impl ToolHandler for JobKillTool {
    fn name(&self) -> String {
        "job_kill".into()
    }
    fn description(&self) -> String {
        "Request cancellation of a background job. Returns only after the work actually stopped (or the 15s grace expired). The job settles as killed.".into()
    }
    fn description_zh(&self) -> String {
        "请求取消一个后台任务；等工作真正停止（或 15 秒宽限期结束）才返回。任务会结算为 killed。"
            .into()
    }
    fn parameters(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "job_id": { "type": "string" },
                "reason": { "type": "string", "description": "Why it is being stopped (forwarded to the producer)" }
            },
            "required": ["job_id"]
        })
    }

    async fn execute(
        &self,
        args: serde_json::Value,
        ctx: &ToolContext,
    ) -> Result<ToolOutput, ToolError> {
        let id = args.get("job_id").and_then(|v| v.as_str()).unwrap_or("");
        if id.is_empty() {
            return Ok(ToolOutput::bad_json("job_kill: missing `job_id`"));
        }
        let owner = match owner(ctx) {
            Ok(o) => o,
            Err(e) => return Ok(ToolOutput::bad_json(e)),
        };
        let reason = args
            .get("reason")
            .and_then(|v| v.as_str())
            .map(String::from);
        let registry = global();
        let requested = registry
            .cancel(id, owner, reason)
            .map_err(ToolError::Custom)?;
        let already = requested.status.is_terminal();
        let snap = if already {
            requested
        } else {
            registry
                .wait(id, owner, KILL_WAIT)
                .await
                .map_err(ToolError::Custom)?
        };
        registry.mark_reported(id);
        let verb = if already {
            "already finished"
        } else if snap.status == JobStatus::Killed {
            "killed"
        } else {
            snap.status.as_str()
        };
        Ok(ToolOutput::success_with_prompt(
            serde_json::json!({
                "outcome": if already { "already-finished" } else { "cancellation-requested" },
                "job": snap.to_json(),
            }),
            format!(
                "{} {} ({})\n{}",
                snap.id,
                verb,
                snap.label,
                status_suffix(&snap)
            ),
        ))
    }
}
