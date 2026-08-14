use async_trait::async_trait;
use oz_core_types::{ToolContext, ToolError, ToolOutput, ToolDefinition, ToolFunction};

use crate::registry::ToolHandler;

pub fn definition() -> ToolDefinition {
    let tool = SubmitPlanTool;
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
        let tool = SubmitPlanTool;
        let rt = tokio::runtime::Runtime::new().expect("Failed to create tokio runtime");
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

/// Submit the execution plan for a complex task BEFORE writing files.
///
/// The plan's steps become pending todos (trackable, gated by the checklist
/// before respond), and the plan marker gates the gentle-reminder nudge.
/// Call once after exploring/understanding the task; update later via
/// todowrite/todoupdate as reality drifts.
pub struct SubmitPlanTool;

#[async_trait]
impl ToolHandler for SubmitPlanTool {
    fn name(&self) -> String { "submit_plan".to_string() }
    fn description(&self) -> String {
        "Submit the execution plan for a complex task BEFORE writing files. \
         goal + steps[]; the steps become pending todos that gate the final respond. \
         Call once after exploring the task; keep steps verifiable (one action each)."
            .to_string()
    }
    fn description_zh(&self) -> String {
        "在写文件【之前】提交复杂任务的执行计划。goal + steps[]，steps 会成为待办清单并约束最终 respond。\
         探索完任务后调用一次；每步保持单一可验证动作；后续用 todowrite/todoupdate 更新。"
            .to_string()
    }
    fn parameters(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "goal": {
                    "type": "string",
                    "description": "The overall task goal (one sentence)"
                },
                "steps": {
                    "type": "array",
                    "items": { "type": "string" },
                    "description": "Verifiable execution steps, in order"
                }
            },
            "required": ["goal", "steps"]
        })
    }

    async fn execute(&self, args: serde_json::Value, _ctx: &ToolContext) -> Result<ToolOutput, ToolError> {
        let goal = args["goal"].as_str().unwrap_or("").to_string();
        let steps: Vec<String> = args["steps"]
            .as_array()
            .map(|arr| {
                arr.iter()
                    .filter_map(|v| v.as_str().map(|s| s.to_string()))
                    .filter(|s| !s.trim().is_empty())
                    .collect()
            })
            .unwrap_or_default();

        if goal.is_empty() || steps.is_empty() {
            return Err(ToolError::Custom(
                "submit_plan requires a non-empty goal and at least one step".into(),
            ));
        }

        Ok(ToolOutput::success_with_prompt(
            serde_json::json!({
                "status": "planned",
                "goal": goal,
                "step_count": steps.len(),
            }),
            &format!("\n[submit_plan] plan recorded: {} step(s) → todos (pending)", steps.len()),
        ))
    }
}
