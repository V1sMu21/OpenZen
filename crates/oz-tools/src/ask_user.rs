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
        let question = args["question"].as_str().unwrap_or("");
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

    fn ctx() -> TC {
        TC {
            working_dir: "/tmp".into(),
            assets_dir: "/tmp".into(),
            script_dir: "/tmp".into(),
            lang: "en".into(),
            skill_mcp_dir: None,
            harness_dir: None,
            session_id: String::new(),
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
    async fn test_ask_user_missing_question_does_not_panic() {
        let r = AskUserTool
            .execute(serde_json::json!({}), &ctx())
            .await
            .unwrap();
        assert_eq!(r.data["status"], "INTERRUPT");
        assert_eq!(r.data["data"]["question"], "");
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
