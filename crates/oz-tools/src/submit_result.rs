//! `submit_result` — captures a subagent's structured answer (P5).
//!
//! Registered for every session, but only a subagent may use it: a root chat
//! has no parent to hand the value to. The value is stored per child session
//! and consumed by the host after the run.

use async_trait::async_trait;
use oz_core::subagent;
use oz_core_types::{ToolContext, ToolError, ToolOutput};

use crate::registry::ToolHandler;

pub struct SubmitResultTool;

#[async_trait]
impl ToolHandler for SubmitResultTool {
    fn name(&self) -> String {
        "submit_result".into()
    }
    fn description(&self) -> String {
        "Submit your final structured result (subagents only) as a JSON object. Call this once, when the task is done, then finish with a short summary.".into()
    }
    fn description_zh(&self) -> String {
        "提交最终的结构化结果（仅子代理可用）：传一个 JSON 对象，调用后简短收尾。".into()
    }
    fn parameters(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "result": { "type": "object", "description": "The structured result object" }
            },
            "required": ["result"]
        })
    }

    async fn execute(
        &self,
        args: serde_json::Value,
        ctx: &ToolContext,
    ) -> Result<ToolOutput, ToolError> {
        if !ctx.is_subagent || ctx.session_id.is_empty() {
            return Ok(ToolOutput::bad_json(
                "submit_result is only available inside a subagent",
            ));
        }
        let Some(result) = args.get("result") else {
            return Ok(ToolOutput::bad_json("submit_result: missing `result`"));
        };
        if !result.is_object() {
            return Ok(ToolOutput::bad_json(
                "submit_result: `result` must be a JSON object",
            ));
        }
        subagent::set_structured(&ctx.session_id, result.clone());
        Ok(ToolOutput::success_with_prompt(
            serde_json::json!({ "status": "captured" }),
            "Result captured. Finish with a one-line summary.",
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use oz_core_types::ToolContext as TC;

    #[tokio::test]
    async fn root_session_cannot_submit() {
        let ctx = TC::test();
        let out = SubmitResultTool
            .execute(serde_json::json!({"result": {"a": 1}}), &ctx)
            .await
            .expect("execute");
        assert!(format!("{out:?}").contains("only available inside a subagent"));
    }

    #[tokio::test]
    async fn subagent_submission_is_captured() {
        let mut ctx = TC::test();
        ctx.is_subagent = true;
        ctx.session_id = "child-submit-test".into();
        let out = SubmitResultTool
            .execute(serde_json::json!({"result": {"summary": "ok"}}), &ctx)
            .await
            .expect("execute");
        assert!(format!("{out:?}").contains("captured"));
        assert_eq!(
            subagent::take_structured("child-submit-test"),
            Some(serde_json::json!({"summary": "ok"}))
        );
    }
}
