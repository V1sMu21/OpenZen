//! `send_message` — continue a finished subagent (P5).
//!
//! The child's transcript lives in a sidecar; the host rehydrates it, appends
//! this message, runs one more turn, and exports the transcript again. Only
//! the parent conversation that spawned the child may continue it.

use async_trait::async_trait;
use oz_core::subagent;
use oz_core_types::{ToolContext, ToolError, ToolOutput};

use crate::registry::ToolHandler;

pub struct SendMessageTool;

#[async_trait]
impl ToolHandler for SendMessageTool {
    fn name(&self) -> String {
        "send_message".into()
    }
    fn description(&self) -> String {
        "Continue a finished subagent with one more message. The child keeps its previous transcript (it is rehydrated from disk). Only the conversation that spawned it may use this.".into()
    }
    fn description_zh(&self) -> String {
        "给已结束的子代理追加一条消息并继续执行。子代理会带着之前的对话继续（从磁盘恢复）；只有发起它的会话能用。".into()
    }
    fn parameters(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "child_id": { "type": "string", "description": "child_session_id returned by spawn_agent" },
                "message": { "type": "string", "description": "What to tell the child" }
            },
            "required": ["child_id", "message"]
        })
    }

    async fn execute(
        &self,
        args: serde_json::Value,
        ctx: &ToolContext,
    ) -> Result<ToolOutput, ToolError> {
        let child_id = args.get("child_id").and_then(|v| v.as_str()).unwrap_or("");
        let message = args.get("message").and_then(|v| v.as_str()).unwrap_or("");
        if child_id.is_empty() || message.trim().is_empty() {
            return Ok(ToolOutput::bad_json(
                "send_message: `child_id` and `message` are both required",
            ));
        }
        if ctx.session_id.is_empty() {
            return Ok(ToolOutput::bad_json("send_message: no parent session"));
        }
        let Some(host) = subagent::host() else {
            return Ok(ToolOutput::bad_json(
                "send_message: subagents are not available in this runtime",
            ));
        };
        match host
            .continue_child(child_id, message, &ctx.session_id)
            .await
        {
            Ok(res) => Ok(ToolOutput::success_with_prompt(
                serde_json::json!({
                    "status": res.status,
                    "child_session_id": res.child_session,
                    "text": res.text,
                }),
                res.text,
            )),
            Err(e) => Ok(ToolOutput::bad_json(format!("send_message: {e}"))),
        }
    }
}
