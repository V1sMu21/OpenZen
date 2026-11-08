use std::path::Path;

use async_trait::async_trait;
use oz_core_types::{ToolContext, ToolError, ToolOutput};

use crate::file_ops::is_in_working_dir;
use crate::registry::ToolHandler;

/// Opens a file artifact in the right sidebar for preview.
/// The agent calls this after writing a file (HTML, PDF, spreadsheet, code, etc.)
/// to display it in the side panel.
pub struct OpenSidePanelTool;

#[async_trait]
impl ToolHandler for OpenSidePanelTool {
    fn name(&self) -> String {
        "open_side_panel".to_string()
    }

    fn description(&self) -> String {
        "Open a file in the right sidebar for the USER to preview. Returns only {status: OPENED} — it does NOT return file or image content to the model; use read to inspect file contents. Types: html, code, spreadsheet, pdf, terminal, diff, image, markdown, office.".to_string()
    }

    fn description_zh(&self) -> String {
        "在右侧边栏打开文件供【用户】预览。路径必须在工作目录内（截图/临时文件请保存到 work/ 或 assets/，不要用 /tmp）。只返回 {status: OPENED}，不向模型返回文件/图像内容；需要检查文件内容请用 read。类型：html、code、spreadsheet、pdf、terminal、diff、image、markdown、office。".to_string()
    }
    fn parameters(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "artifact_type": {
                    "type": "string",
                    "description": "Type: html/code/spreadsheet/pdf/terminal/diff/image/markdown/office",
                    "enum": ["html", "code", "spreadsheet", "pdf", "terminal", "diff", "image", "markdown", "office"]
                },
                "artifact_path": {
                    "type": "string",
                    "description": "File path"
                },
                "artifact_label": {
                    "type": "string",
                    "description": "Tab label (optional)"
                }
            },
            "required": ["artifact_type", "artifact_path"]
        })
    }

    async fn execute(
        &self,
        args: serde_json::Value,
        ctx: &ToolContext,
    ) -> Result<ToolOutput, ToolError> {
        let artifact_type = args["artifact_type"].as_str().unwrap_or("");
        let artifact_path = args["artifact_path"].as_str().unwrap_or("");
        let artifact_label = args["artifact_label"].as_str();

        if artifact_type.is_empty() || artifact_path.is_empty() {
            // Name only what is actually missing, and name the keys the call
            // did send. The old message said "missing artifact_type or
            // artifact_path" even when the path was present but polluted, which
            // sent the model looking in the wrong place (2026-11-02).
            let missing: Vec<&str> = [
                ("artifact_type", artifact_type),
                ("artifact_path", artifact_path),
            ]
            .into_iter()
            .filter_map(|(key, value)| value.is_empty().then_some(key))
            .collect();
            return Ok(ToolOutput::bad_json(oz_core_types::missing_arg_error(
                "open_side_panel",
                &missing,
                &args,
            )));
        }

        let valid_types = [
            "html",
            "code",
            "spreadsheet",
            "pdf",
            "terminal",
            "diff",
            "image",
            "markdown",
            "office",
        ];
        if !valid_types.contains(&artifact_type) {
            return Ok(ToolOutput::bad_json(format!(
                "open_side_panel: unsupported artifact_type '{}'. Supported: {}",
                artifact_type,
                valid_types.join(", ")
            )));
        }

        // Resolve relative paths
        let resolved = ctx.resolve_path(artifact_path);

        // Terminal is special — path is "." for current directory
        if artifact_type != "terminal" {
            let p = Path::new(&resolved);
            if !p.exists() {
                return Ok(ToolOutput::bad_json(format!(
                    "open_side_panel: file not found: {resolved}"
                )));
            }
            if !is_in_working_dir(&resolved, &ctx.working_dir) {
                return Ok(ToolOutput::bad_json(format!(
                    "open_side_panel: path '{resolved}' is outside working directory '{}'",
                    ctx.working_dir
                )));
            }
        }

        let label = artifact_label.map(|s| s.to_string()).unwrap_or_else(|| {
            Path::new(&resolved)
                .file_name()
                .map(|n| n.to_string_lossy().to_string())
                .unwrap_or_else(|| "unnamed".into())
        });

        Ok(ToolOutput {
            data: serde_json::json!({
                "status": "OPENED",
                "artifact_type": artifact_type,
                "artifact_path": resolved,
                "artifact_label": label,
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

    /// 2026-11-02: the old message said "missing artifact_type or
    /// artifact_path" even when only one was missing, so a model whose path had
    /// been polluted by the gateway re-sent the same call. Name what is
    /// actually missing and what the call did send.
    #[tokio::test]
    async fn missing_argument_error_names_only_the_missing_key() {
        let out = OpenSidePanelTool
            .execute(serde_json::json!({"artifact_path": "/tmp/a.png"}), &ctx())
            .await
            .expect("tool returns a result object");
        let text = serde_json::to_string(&out.data).unwrap_or_default();
        assert!(text.contains("artifact_type"), "{text}");
        assert!(
            !text.contains("`artifact_path`"),
            "the path was present and must not be named as missing: {text}"
        );
        assert!(
            text.contains("artifact_path"),
            "sent keys must be named: {text}"
        );
        assert!(oz_core_types::corrupt_call_reason(&out.data).is_none());
    }
}
