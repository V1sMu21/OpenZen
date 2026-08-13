use async_trait::async_trait;
use oz_core_types::{ToolContext, ToolError, ToolOutput, Reminder, REMINDER_TX, ToolDefinition, ToolFunction};

use crate::registry::ToolHandler;

pub fn definition() -> ToolDefinition {
    let tool = ScheduleReminderTool;
    ToolDefinition {
        type_: "function".into(),
        function: ToolFunction {
            name: tool.name().into(),
            description: tool.description().into(),
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
    fn name(&self) -> String { "schedule_reminder".to_string() }
    fn description(&self) -> String {
        "Schedule a reminder message after a delay. Supports repeating at intervals.".to_string()
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
                }
            },
            "required": ["delay_seconds", "message"]
        })
    }

    async fn execute(&self, args: serde_json::Value, ctx: &ToolContext) -> Result<ToolOutput, ToolError> {
        let delay_secs = args["delay_seconds"].as_u64().unwrap_or(60).max(5).min(3600);
        let message = args["message"].as_str().unwrap_or("").to_string();
        let repeat_count = args["repeat_count"].as_u64().unwrap_or(0).min(10) as u32;
        let repeat_interval = args["repeat_interval_seconds"].as_u64().unwrap_or(delay_secs).max(5).min(3600);

        if message.is_empty() {
            return Err(ToolError::Custom("schedule_reminder requires a non-empty message".into()));
        }

        let now_ms = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_millis() as u64)
            .unwrap_or(0);
        let fire_at_ms = now_ms + (delay_secs * 1000);

        // Session identity travels on ToolContext (per-run) instead of a
        // process-global that concurrent sessions overwrote.
        let session_id = ctx.session_id.clone();

        let tx_exists = REMINDER_TX.get().is_some();

        let reminder = Reminder {
            session_id,
            message: message.clone(),
            fire_at_ms,
            repeat_count,
            repeat_interval_secs: repeat_interval,
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
                format!("scheduled in {}s, repeating {} more times every {}s", delay_secs, repeat_count, repeat_interval)
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
                &format!("\n[schedule_reminder] {status_msg}"),
            ))
        } else {
            Err(ToolError::Custom("schedule_reminder is not available in this environment (no reminder channel)".into()))
        }
    }
}
