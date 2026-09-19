use async_trait::async_trait;
use oz_core_types::{
    Reminder, ToolContext, ToolDefinition, ToolError, ToolFunction, ToolOutput, REMINDER_TX,
};

use crate::registry::ToolHandler;

pub fn definition() -> ToolDefinition {
    let tool = ScheduleReminderTool;
    ToolDefinition {
        type_: "function".into(),
        function: ToolFunction {
            name: tool.name(),
            description: tool.description(),
            parameters: tool.parameters(),
        },
    }
}

pub fn handler() -> crate::ToolHandler {
    std::sync::Arc::new(move |_name, args, ctx| {
        let tool = ScheduleReminderTool;
        let rt = tokio::runtime::Runtime::new().expect("Failed to create tokio runtime");
        // Pass the real ToolContext through — session_id lives on it now.
        match rt.block_on(tool.execute(args.clone(), ctx)) {
            Ok(output) => oz_core_types::StepOutcome {
                data: output.data,
                next_prompt: output.next_prompt,
                should_exit: output.should_exit,
                images: output.images,
            },
            Err(e) => oz_core_types::StepOutcome::success(serde_json::json!({
                "error": e.to_string()
            })),
        }
    })
}

/// Schedule a reminder that will inject a message into the session and trigger
/// the agent to continue after a delay. The agent can use this to implement
/// periodic tasks, delayed follow-ups, or time-based polling.
///
/// The reminder is delivered through a global channel to the Tauri backend,
/// which manages the timer and triggers the agent run when the delay expires.
pub struct ScheduleReminderTool;

#[async_trait]
impl ToolHandler for ScheduleReminderTool {
    fn name(&self) -> String {
        "schedule_reminder".to_string()
    }
    fn description(&self) -> String {
        "Schedule a reminder message. By default it is run-scoped (dies when this task finishes). Pass persist=true for reminders that must outlive the run and app restarts (e.g. 'remind me in 30 minutes'); repeat_count>0 makes it a periodic heartbeat.".to_string()
    }
    fn description_zh(&self) -> String {
        "安排提醒消息。默认随本任务结束而清除；persist=true 时跨任务与重启存活（适用于「30 分钟后提醒我」）；repeat_count>0 为周期心跳任务。".to_string()
    }
    fn parameters(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "delay_seconds": {
                    "type": "integer",
                    "description": "Seconds until first reminder (5-3600)"
                },
                "message": {
                    "type": "string",
                    "description": "Message to inject when reminder fires"
                },
                "repeat_count": {
                    "type": "integer",
                    "description": "Extra repeats after first (0-10)"
                },
                "repeat_interval_seconds": {
                    "type": "integer",
                    "description": "Seconds between repeats"
                },
                "persist": {
                    "type": "boolean",
                    "description": "true = the reminder survives this run finishing and app restarts (use for 'remind me in 20 minutes' style requests). Default false = run-scoped (heartbeats for the current task)."
                },
                "at_unix_ms": {
                    "type": "integer",
                    "description": "Optional absolute fire time (unix millis, overrides delay_seconds) — lets the user ask for a specific clock time."
                }
            },
            "required": ["message"]
        })
    }

    async fn execute(
        &self,
        args: serde_json::Value,
        ctx: &ToolContext,
    ) -> Result<ToolOutput, ToolError> {
        let delay_secs = args["delay_seconds"].as_u64().unwrap_or(60).clamp(5, 3600);
        let message = args["message"].as_str().unwrap_or("").to_string();
        let repeat_count = args["repeat_count"].as_u64().unwrap_or(0).min(10) as u32;
        let persist = args
            .get("persist")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);
        let repeat_interval = args["repeat_interval_seconds"]
            .as_u64()
            .unwrap_or(delay_secs)
            .clamp(5, 3600);

        if message.is_empty() {
            return Err(ToolError::Custom(
                "schedule_reminder requires a non-empty message".into(),
            ));
        }

        let now_ms = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_millis() as u64)
            .unwrap_or(0);
        let fire_at_ms = args
            .get("at_unix_ms")
            .and_then(|v| v.as_u64())
            .filter(|t| *t > now_ms)
            .unwrap_or(now_ms + (delay_secs * 1000));

        // Session identity travels on ToolContext (per-run) instead of a
        // process-global that concurrent sessions overwrote.
        let session_id = ctx.session_id.clone();

        let reminder = Reminder {
            session_id,
            message: message.clone(),
            fire_at_ms,
            repeat_count,
            repeat_interval_secs: repeat_interval,
            persist,
        };

        let sent = REMINDER_TX
            .get()
            .map(|tx| {
                let ok = tx.send(reminder).is_ok();
                tracing::warn!("[schedule_reminder] send result: ok={ok}");
                ok
            })
            .unwrap_or(false);

        if sent {
            let status_msg = if repeat_count > 0 {
                format!(
                    "scheduled in {}s, repeating {} more times every {}s",
                    delay_secs, repeat_count, repeat_interval
                )
            } else {
                format!("scheduled in {}s", delay_secs)
            };
            Ok(ToolOutput::success_with_prompt(
                serde_json::json!({
                    "status": "scheduled",
                    "message": message,
                    "delay_seconds": delay_secs,
                    "fire_at_ms": fire_at_ms,
                    "repeat_count": repeat_count,
                    "repeat_interval_seconds": repeat_interval,
                }),
                format!("\n[schedule_reminder] {status_msg}"),
            ))
        } else {
            Err(ToolError::Custom(
                "schedule_reminder is not available in this environment (no reminder channel)"
                    .into(),
            ))
        }
    }
}
