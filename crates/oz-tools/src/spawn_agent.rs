//! `spawn_agent` — delegate a self-contained task to a child agent.
//!
//! Foreground only for now: the tool awaits the child and returns its final
//! text. Background delegation (`background: true`) is P3 and hooks into the
//! job registry.

use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use oz_core::jobs::{global as jobs, JobHooks, JobKind, JobOutcome, JobStart, JobStatus};
use oz_core::subagent::{self, SubagentRequest, MAX_DEPTH};
use oz_core_types::{ToolContext, ToolError, ToolOutput};
use tokio::sync::oneshot;

use crate::registry::ToolHandler;

/// Keep the parent's context small: a child's conclusion is what travels back.
const TEXT_CAP: usize = 8 * 1024;

pub struct SpawnAgentTool;

#[async_trait]
impl ToolHandler for SpawnAgentTool {
    fn name(&self) -> String {
        "spawn_agent".into()
    }
    fn description(&self) -> String {
        "Delegate a self-contained task to a child agent. The child shares your working directory but has its own context, and cannot ask the user questions — if it needs a decision it returns a `## NEEDS_INPUT` section in its final answer. Use it for independent research, a bounded refactor, or a verification pass you do not want cluttering this conversation.".into()
    }
    fn description_zh(&self) -> String {
        "把一个独立的子任务交给子代理。子代理共享工作目录但有自己的上下文，且不能向用户提问——需要决策时会在结果里返回 `## NEEDS_INPUT`。适合独立调研、边界清晰的重构或不想污染当前对话的验收。".into()
    }
    fn parameters(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "description": {
                    "type": "string",
                    "description": "One-line label shown in the parent's card (max 80 chars)"
                },
                "prompt": {
                    "type": "string",
                    "description": "Full task: what to do, how to verify it, and what to return"
                },
                "model": {
                    "type": "string",
                    "description": "Optional mykey.toml session name; omit to inherit the parent's model"
                },
                "max_turns": {
                    "type": "integer",
                    "description": "Turn budget for this child (default: loop default, max 40)"
                },
                "background": {
                    "type": "boolean",
                    "description": "Run detached and return a job id immediately (read with job_output, stop with job_kill)"
                },
                "output_schema": {
                    "type": "object",
                    "description": "Optional JSON Schema (object). When set, the child must answer with JSON; a valid reply is returned parsed under `structured`."
                }
            },
            "required": ["description", "prompt"]
        })
    }

    async fn execute(
        &self,
        args: serde_json::Value,
        ctx: &ToolContext,
    ) -> Result<ToolOutput, ToolError> {
        let label = args
            .get("description")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .trim();
        let prompt = args.get("prompt").and_then(|v| v.as_str()).unwrap_or("");
        if label.is_empty() || prompt.trim().is_empty() {
            return Ok(ToolOutput::bad_json(
                "spawn_agent: `description` and `prompt` are both required",
            ));
        }
        if ctx.session_id.is_empty() {
            return Ok(ToolOutput::bad_json(
                "spawn_agent: no parent session in this context",
            ));
        }
        let Some(depth) = subagent::next_depth(&ctx.session_id) else {
            return Ok(ToolOutput::bad_json(format!(
                "spawn_agent: delegation depth limit reached (max {MAX_DEPTH})"
            )));
        };
        let Some(host) = subagent::host() else {
            return Ok(ToolOutput::bad_json(
                "spawn_agent: subagents are not available in this runtime",
            ));
        };
        let child_id = uuid::Uuid::new_v4().to_string();
        let background = args
            .get("background")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);
        let mut prompt = prompt.to_string();
        if let Some(schema) = args.get("output_schema") {
            prompt.push_str(&format!(
                "\n\n[structured output] When the task is done, call submit_result with a JSON object matching this schema:\n{}",
                serde_json::to_string_pretty(schema).unwrap_or_default()
            ));
        }
        let max_turns = args
            .get("max_turns")
            .and_then(|v| v.as_u64())
            .map(|n| n.clamp(1, 40) as u32);
        if let Some(n) = max_turns {
            subagent::set_max_turns(&child_id, n);
        }
        let req = SubagentRequest {
            child_id: child_id.clone(),
            parent_session: ctx.session_id.clone(),
            label: label.chars().take(80).collect(),
            prompt,
            max_turns,
            model: args
                .get("model")
                .and_then(|v| v.as_str())
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .map(String::from),
        };
        if background {
            return start_background(host, req, label, ctx);
        }

        let started = std::time::Instant::now();
        match host.run(req).await {
            Ok(res) => {
                // Judge the FULL reply: truncating first turned a large but
                // valid JSON document into "not valid JSON: EOF".
                let needs_input = res.text.contains("NEEDS_INPUT");
                let (structured, schema_error) = match res.structured.clone() {
                    Some(v) => (v, None),
                    None => extract_structured(&res.text, &args),
                };
                let text = truncate(&res.text, TEXT_CAP);
                Ok(ToolOutput::success_with_prompt(
                    serde_json::json!({
                        "status": res.status,
                        "child_session_id": res.child_session,
                        "depth": depth,
                        "elapsed_ms": started.elapsed().as_millis() as u64,
                        "text": text,
                        "needs_input": needs_input,
                        "structured": structured,
                        "schema_error": schema_error,
                    }),
                    format!("[subagent {} · depth {depth}]\n{}", res.status, text),
                ))
            }
            Err(e) => Ok(ToolOutput::bad_json(format!(
                "spawn_agent: {e} (child_session_id: {}; its transcript may still be readable with agent_read)",
                child_id
            ))),
        }
    }
}

fn truncate(text: &str, cap: usize) -> String {
    if text.len() <= cap {
        return text.to_string();
    }
    let mut end = cap;
    while end > 0 && !text.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}\n…[truncated {} bytes]", &text[..end], text.len() - end)
}

/// Background delegation: register the run as a `Subagent` job, buffer the
/// final text as its output, and let `job_kill` cancel the child.
fn start_background(
    host: Arc<dyn subagent::SubagentHost>,
    req: SubagentRequest,
    label: &str,
    ctx: &ToolContext,
) -> Result<ToolOutput, ToolError> {
    let child_id = req.child_id.clone();
    let child_for_result = child_id.clone();
    let cancel_host = host.clone();
    let (tx, rx) = oneshot::channel::<JobOutcome>();
    let output = Arc::new(Mutex::new(String::new()));
    let output_for_read = output.clone();
    let job_id = jobs()
        .start(JobStart {
            kind: JobKind::Subagent,
            label: label.to_string(),
            owner_session: ctx.session_id.clone(),
            run: Box::new(move |_job| {
                let host = host.clone();
                let req = req.clone();
                let output = output.clone();
                tokio::spawn(async move {
                    let (status, detail, text) = match host.run(req).await {
                        Ok(res) => (JobStatus::Completed, None, res.text),
                        Err(e) => (JobStatus::Failed, Some(e), String::new()),
                    };
                    *output.lock().unwrap() = text;
                    let _ = tx.send(JobOutcome { status, detail });
                });
                Ok(JobHooks {
                    cancel: Box::new(move |_| cancel_host.cancel(&child_id)),
                    read_output: Some(Box::new(move || {
                        std::mem::take(&mut *output_for_read.lock().unwrap())
                    })),
                    done: rx,
                })
            }),
        })
        .map_err(ToolError::Custom)?;
    Ok(ToolOutput::success_with_prompt(
        serde_json::json!({
            "status": "running",
            "job_id": job_id,
            "child_session_id": child_for_result,
        }),
        format!("[background subagent] started {job_id}. Read with job_output(\"{job_id}\")."),
    ))
}

/// Optional structured output: parse the child's final text as JSON and check
/// the schema's top-level `required` keys. A mismatch is reported, not fatal —
/// the parent still gets the text.
fn extract_structured(text: &str, args: &serde_json::Value) -> (serde_json::Value, Option<String>) {
    let Some(schema) = args.get("output_schema") else {
        return (serde_json::Value::Null, None);
    };
    match serde_json::from_str::<serde_json::Value>(text) {
        Ok(v) => {
            if let Some(required) = schema.get("required").and_then(|r| r.as_array()) {
                let missing: Vec<&str> = required
                    .iter()
                    .filter_map(|k| k.as_str())
                    .filter(|k| v.get(*k).is_none())
                    .collect();
                if !missing.is_empty() {
                    return (
                        v,
                        Some(format!("missing required keys: {}", missing.join(", "))),
                    );
                }
            }
            (v, None)
        }
        Err(e) => (
            serde_json::Value::Null,
            Some(format!("reply is not valid JSON: {e}")),
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn structured_output_parses_and_reports_missing_keys() {
        let schema = serde_json::json!({
            "type": "object",
            "required": ["summary", "files"]
        });
        let args = serde_json::json!({ "output_schema": schema });
        let (v, err) = extract_structured(r#"{"summary":"ok","files":[]}"#, &args);
        assert!(err.is_none());
        assert_eq!(v["summary"], "ok");

        let (v, err) = extract_structured(r#"{"summary":"ok"}"#, &args);
        assert!(v.is_null() || v.is_object());
        assert!(err.unwrap().contains("files"));

        let (_, err) = extract_structured("not json", &args);
        assert!(err.unwrap().contains("not valid JSON"));
    }

    #[test]
    fn structured_output_handles_replies_larger_than_the_display_cap() {
        let big = format!(r#"{{"summary":"{}","files":[]}}"#, "x".repeat(9 * 1024));
        let args = serde_json::json!({
            "output_schema": {"type": "object", "required": ["summary", "files"]}
        });
        let (v, err) = extract_structured(&big, &args);
        assert!(err.is_none(), "large valid JSON must not fail: {err:?}");
        assert_eq!(v["files"], serde_json::json!([]));
    }

    #[test]
    fn structured_output_is_off_without_schema() {
        let (v, err) = extract_structured("plain text", &serde_json::json!({}));
        assert!(v.is_null());
        assert!(err.is_none());
    }

    #[tokio::test]
    async fn spawn_without_host_reports_unavailable() {
        let ctx = ToolContext::test();
        let out = SpawnAgentTool
            .execute(
                serde_json::json!({ "description": "x", "prompt": "y" }),
                &ctx,
            )
            .await
            .expect("execute");
        let body = format!("{out:?}");
        // Either no host, or no session — both are clean tool errors.
        assert!(
            body.contains("not available") || body.contains("no parent session"),
            "{body}"
        );
    }
}
