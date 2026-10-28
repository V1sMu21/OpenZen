use async_trait::async_trait;
use oz_core_types::{ToolContext, ToolError, ToolOutput};

use crate::registry::ToolHandler;

/// `INTERRUPT` + `HUMAN_INTERVENTION` means "pause and wait for the user",
/// NOT "exit the loop". The agent loop handles the wait via the per-session
/// `ask_user_rx` channel and resumes the same run with the user's reply.
pub struct AskUserTool;

#[async_trait]
impl ToolHandler for AskUserTool {
    fn name(&self) -> String {
        "ask_user".to_string()
    }
    fn description(&self) -> String {
        "Ask user a question. Pauses until user responds. — it does NOT treat the reply as a brand-new conversation.".to_string()
    }
    fn parameters(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "question": {
                    "type": "string",
                    "description": "Question to ask the user"
                },
                "candidates": {
                    "type": "array",
                    "items": { "type": "string" },
                    "description": "Optional candidate answers"
                }
            },
            "required": ["question"]
        })
    }

    async fn execute(
        &self,
        args: serde_json::Value,
        _ctx: &ToolContext,
    ) -> Result<ToolOutput, ToolError> {
        // Subagents are not allowed to block on the user: no dialog is
        // watching their session. They return the question in their final
        // answer (`## NEEDS_INPUT`) and the parent decides whether to ask.
        if _ctx.is_subagent {
            return Ok(ToolOutput::bad_json(
                "ask_user is not available inside a subagent; return the question to the parent in your final answer under a `## NEEDS_INPUT` section.",
            ));
        }
        let question = args["question"].as_str().unwrap_or("").trim().to_string();
        // An empty question is a broken call, not a human-intervention
        // request. In the 2026-10-23 desktop session a gateway tear produced
        // eight `INTERRUPT` results with `question == ""`; the loop then
        // waited five minutes for each one, sequentially, with nothing the
        // user could answer. Fail immediately so the model gets a precise
        // error and retries the call.
        if question.is_empty() {
            return Err(ToolError::Custom(
                "ask_user: missing `question` argument — no question was received. \
                 Re-issue the call with a non-empty `question`."
                    .into(),
            ));
        }
        let candidates = args
            .get("candidates")
            .and_then(|v| v.as_array())
            .cloned()
            .unwrap_or_default();

        Ok(ToolOutput {
            data: serde_json::json!({
                "status": "INTERRUPT",
                "intent": "HUMAN_INTERVENTION",
                "data": {
                    "question": question,
                    "candidates": candidates,
                }
            }),
            next_prompt: None,
            should_exit: false,
            images: vec![],
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use oz_core_types::ToolContext as TC;

    #[tokio::test]
    async fn subagent_cannot_park_on_the_user() {
        let mut c = ctx();
        c.is_subagent = true;
        let out = AskUserTool
            .execute(serde_json::json!({"question": "which directory?"}), &c)
            .await
            .expect("execute");
        let body = format!("{out:?}");
        assert!(body.contains("not available inside a subagent"), "{body}");
    }

    fn ctx() -> TC {
        TC {
            is_subagent: false,
            working_dir: "/tmp".into(),
            assets_dir: "/tmp".into(),
            script_dir: "/tmp".into(),
            lang: "en".into(),
            skill_mcp_dir: None,
            harness_dir: None,
            session_id: String::new(),
            tool_budget_secs: 0,
        }
    }

    #[tokio::test]
    async fn test_ask_user_basic() {
        let r = AskUserTool
            .execute(serde_json::json!({"question": "what now?"}), &ctx())
            .await
            .unwrap();
        assert_eq!(r.data["status"], "INTERRUPT");
        assert_eq!(r.data["data"]["question"], "what now?");
        assert!(!r.should_exit, "ask_user must NOT exit the loop");
    }

    #[tokio::test]
    async fn test_ask_user_with_candidates() {
        let r = AskUserTool
            .execute(
                serde_json::json!({"question": "choose?", "candidates": ["a", "b"]}),
                &ctx(),
            )
            .await
            .unwrap();
        assert_eq!(r.data["data"]["candidates"].as_array().unwrap().len(), 2);
    }

    #[tokio::test]
    async fn test_ask_user_missing_question_is_a_tool_error() {
        let r = AskUserTool.execute(serde_json::json!({}), &ctx()).await;
        assert!(r.is_err(), "an empty question must not become a wait state");
        let err = r.unwrap_err().to_string();
        assert!(
            err.contains("question"),
            "error should name the missing key"
        );

        let r2 = AskUserTool
            .execute(serde_json::json!({"question": "   "}), &ctx())
            .await;
        assert!(r2.is_err(), "whitespace-only question is not askable");
    }

    #[tokio::test]
    async fn test_ask_user_no_candidates_defaults_empty() {
        let r = AskUserTool
            .execute(serde_json::json!({"question": "what?"}), &ctx())
            .await
            .unwrap();
        let candidates = r.data["data"]["candidates"].as_array().unwrap();
        assert!(candidates.is_empty());
    }

    #[tokio::test]
    async fn test_ask_user_does_not_exit() {
        let r = AskUserTool
            .execute(serde_json::json!({"question": "continue?"}), &ctx())
            .await
            .unwrap();
        assert!(
            !r.should_exit,
            "ask_user must keep the loop alive so the user reply can resume the same run"
        );
    }

    #[tokio::test]
    async fn test_ask_user_intent_is_human_intervention() {
        let r = AskUserTool
            .execute(serde_json::json!({"question": "hello?"}), &ctx())
            .await
            .unwrap();
        assert_eq!(r.data["intent"], "HUMAN_INTERVENTION");
    }

    #[tokio::test]
    async fn test_ask_user_next_prompt_is_none() {
        let r = AskUserTool
            .execute(serde_json::json!({"question": "x"}), &ctx())
            .await
            .unwrap();
        assert!(r.next_prompt.is_none());
    }

    #[tokio::test]
    async fn test_ask_user_name_and_description() {
        assert_eq!(AskUserTool.name(), "ask_user");
        assert!(!AskUserTool.description().is_empty());
    }

    #[tokio::test]
    async fn test_ask_user_parameters_has_required_question() {
        let params = AskUserTool.parameters();
        let required = params["required"].as_array().unwrap();
        assert!(required.contains(&serde_json::json!("question")));
    }
}

#[linkme::distributed_slice(crate::registry::TOOL_FACTORIES)]
fn register_ask_user(reg: &mut crate::registry::ToolRegistry) {
    reg.register(crate::ask_user::AskUserTool);
}
