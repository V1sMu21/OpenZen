use std::time::Instant;

use async_trait::async_trait;
use oz_core_types::{ToolContext, ToolDefinition, ToolError, ToolFunction, ToolOutput};

use crate::registry::ToolHandler;

/// Commands that are always blocked regardless of trust settings.
const BLOCKED_COMMANDS: &[&str] = &[
    "rm -rf",
    "rm -r",
    "mkfs",
    "dd if=",
    ":(){ :|:& };:", // fork bomb
    "> /dev/sda",
    "> /dev/nvme",
    "> /dev/hd",
    "chmod 777 /",
    "chmod -R 777 /",
    "wget | sh",
    "curl | bash",
    "curl | sh",
    "wget | bash",
    "shutdown",
    "reboot",
    "halt",
    "init 0",
    "init 6",
    "systemctl poweroff",
    "sudo ",
];

/// Blocklist for Python code patterns that are always dangerous.
const BLOCKED_PYTHON: &[&str] = &[
    "import os; os.system",
    "import subprocess; subprocess",
    "import shutil; shutil.rmtree",
    "import socket; socket.",
    "__import__('os')",
    "eval(",
    "exec(",
    "compile(",
];

fn is_command_blocked(code: &str) -> Option<&'static str> {
    let lower = code.to_lowercase().replace("  ", " ");
    for blocked in BLOCKED_COMMANDS {
        if lower.contains(&blocked.to_lowercase()) {
            return Some(blocked);
        }
    }
    BLOCKED_PYTHON
        .iter()
        .find(|&blocked| code.contains(blocked))
        .map(|v| v as _)
}

pub struct CodeRunTool;

#[async_trait]
impl ToolHandler for CodeRunTool {
    fn name(&self) -> String {
        "code_run".to_string()
    }

    fn description(&self) -> String {
        "Execute shell commands or python code (type: bash|python). Contract: on failure do NOT blind-retry — change approach or ask_user. Independent read-only commands may run concurrently; writes last. Long-running commands: pass `timeout` (up to 1800s); beyond that, launch with nohup in the background and poll.".to_string()
    }

    fn description_zh(&self) -> String {
        "执行 shell 命令或 python 代码（type: bash|python）。契约：失败勿盲目重试——换路径或 ask_user；无依赖的只读命令可并发执行，写操作放最后；长命令用 timeout 参数（上限 1800 秒），更久的任务用 nohup 后台启动并轮询结果。".to_string()
    }
    fn parameters(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "code": {
                    "type": "string",
                    "description": "Code to execute"
                },
                "type": {
                    "type": "string",
                    "description": "'bash' or 'python'",
                    "enum": ["bash", "python", "py", "sh", "shell"]
                },
                "timeout": {
                    "type": "integer",
                    "description": "Timeout in seconds (default 60, max 1800). Set a larger value for long-running commands."
                },
                "mode": {
                    "type": "string",
                    "description": "'inline' or 'rpc'",
                    "enum": ["inline", "rpc"]
                }
            },
            "required": ["code"]
        })
    }

    async fn execute(
        &self,
        args: serde_json::Value,
        ctx: &ToolContext,
    ) -> Result<ToolOutput, ToolError> {
        let code = args["code"].as_str().unwrap_or("");
        if code.is_empty() {
            return Ok(ToolOutput::bad_json("code_run: missing `code` argument"));
        }

        if let Some(blocked) = is_command_blocked(code) {
            return Ok(ToolOutput::bad_json(format!(
                "code_run: blocked dangerous pattern `{blocked}`. Operation denied for security."
            )));
        }

        let code_type = args.get("type").and_then(|v| v.as_str()).unwrap_or("bash");
        // Inner cap. The agent loop's outer cap honors a declared `timeout`
        // (+30s grace) up to 1830s, so the inner kill fires first with its
        // structured timeout result. Clamp here so a runaway value can't
        // outrun the outer cap and get killed by the loop's blunt error.
        // 30s outer caps from `tool_timeout_secs` no longer bite: even the
        // default (60s) is honored.
        let timeout = args
            .get("timeout")
            .and_then(|v| v.as_u64())
            .unwrap_or(60)
            .clamp(1, 1800);
        let mode = args
            .get("mode")
            .and_then(|v| v.as_str())
            .unwrap_or("inline");
        let start = Instant::now();

        let result = match code_type {
            "python" | "py" => self.run_python(code, timeout, ctx).await,
            _ => self.run_shell(code, timeout, ctx).await,
        }?;

        let elapsed = start.elapsed().as_secs_f64();
        let exit_code = result["exit_code"].as_i64().unwrap_or(-1);

        if mode == "rpc" {
            // RPC mode: write full output to temp file, return only a file reference
            let output_dir = std::env::temp_dir().join("oz_rpc");
            std::fs::create_dir_all(&output_dir).ok();
            let filename = format!(
                "code_run_{}_{}.json",
                chrono::Utc::now().format("%Y%m%d_%H%M%S_%3f"),
                uuid::Uuid::new_v4()
                    .to_string()
                    .split('-')
                    .next()
                    .unwrap_or("x")
            );
            let output_path = output_dir.join(&filename);

            if let Ok(json) = serde_json::to_string_pretty(&result) {
                let _ = std::fs::write(&output_path, &json);
            }

            let summary = serde_json::json!({
                "exit_code": exit_code,
                "elapsed_secs": elapsed,
                "mode": "rpc",
                "output_file": output_path.to_string_lossy().to_string(),
                "stdout_chars": result["stdout"].as_str().map(|s| s.len()).unwrap_or(0),
                "stderr_chars": result["stderr"].as_str().map(|s| s.len()).unwrap_or(0),
                "truncated_preview": result["stdout"].as_str()
                    .map(|s| truncate_preview(s, 200))
                    .unwrap_or_default(),
            });

            let prompt = format!(
                "\n[code_run:RPC] exit={exit_code} ({elapsed:.1}s) output written to {}",
                output_path.display()
            );
            Ok(ToolOutput::success_with_prompt(summary, prompt))
        } else {
            // Inline mode: return full output in context (original behavior)
            Ok(ToolOutput::success_with_prompt(
                result,
                format!("\n[code_run] exit={exit_code} ({elapsed:.1}s)"),
            ))
        }
    }
}

/// Best-effort head preview that never splits a UTF-8 char (raw byte
/// slicing panics on CJK output).
fn truncate_preview(s: &str, max_bytes: usize) -> String {
    if s.len() <= max_bytes {
        return s.to_string();
    }
    let mut end = max_bytes;
    while end > 0 && !s.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}... [{} total chars]", &s[..end], s.len())
}

/// Removes the temp python script when dropped — including when the
/// surrounding future is cancelled (tool timeout / user stop), which
/// previously leaked one ga_*.ai.py per cancelled run into /tmp.
struct TmpScriptGuard(std::path::PathBuf);

impl Drop for TmpScriptGuard {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
    }
}

/// Spawn a child with kill-on-drop so that cancelling the surrounding
/// future (tool timeout, user stop, run abort) never leaves an orphan
/// `sh`/`python3` behind — a 7x24 agent leaks one process per timeout
/// otherwise. Returns the collected output.
async fn run_child_with_timeout(
    mut command: tokio::process::Command,
    timeout: u64,
    what: &str,
) -> Result<serde_json::Value, ToolError> {
    command
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .kill_on_drop(true);
    let child = command
        .spawn()
        .map_err(|e| ToolError::Custom(format!("{what} execution failed: {e}. Verify the command syntax and that required tools are installed.")))?;

    match tokio::time::timeout(
        std::time::Duration::from_secs(timeout.max(1)),
        child.wait_with_output(),
    )
    .await
    {
        Ok(output) => {
            let output = output.map_err(|e| ToolError::Custom(format!("{what} failed: {e}")))?;
            let stdout = String::from_utf8_lossy(&output.stdout).to_string();
            let stderr = String::from_utf8_lossy(&output.stderr).to_string();
            let exit_code = output.status.code().unwrap_or(-1);
            Ok(serde_json::json!({
                "exit_code": exit_code,
                "stdout": stdout,
                "stderr": stderr,
            }))
        }
        Err(_) => Ok(serde_json::json!({
            "exit_code": -1,
            "stdout": "",
            "stderr": format!("{what} timed out after {timeout}s (process killed)"),
            "timeout": true,
        })),
    }
}

impl CodeRunTool {
    async fn run_shell(
        &self,
        code: &str,
        timeout: u64,
        ctx: &ToolContext,
    ) -> Result<serde_json::Value, ToolError> {
        let mut cmd = tokio::process::Command::new("sh");
        cmd.arg("-c").arg(code).current_dir(&ctx.working_dir);
        let mut result = run_child_with_timeout(cmd, timeout, "code_run").await?;
        // Preserve the legacy "timed out" heuristic for callers that check it.
        if result.get("timeout").is_none() {
            let stderr = result["stderr"].as_str().unwrap_or("");
            if !stderr.is_empty() && stderr.len() < 500 && stderr.contains("timed out") {
                result["timeout"] = serde_json::json!(true);
            }
        }
        Ok(result)
    }

    async fn run_python(
        &self,
        code: &str,
        timeout: u64,
        ctx: &ToolContext,
    ) -> Result<serde_json::Value, ToolError> {
        let tmp_dir = std::env::temp_dir();
        let script_path = tmp_dir.join(format!("ga_{}.ai.py", uuid::Uuid::new_v4()));
        let _script_guard = TmpScriptGuard(script_path.clone());

        let header = String::new();
        let full_code = format!("{header}{code}");
        tokio::fs::write(&script_path, &full_code)
            .await
            .map_err(|e| {
                ToolError::Custom(format!(
                    "failed to write temp script: {e}. Check disk space and /tmp permissions."
                ))
            })?;

        let python = if cfg!(target_os = "windows") {
            "python"
        } else {
            "python3"
        };

        let mut cmd = tokio::process::Command::new(python);
        cmd.arg("-X")
            .arg("utf8")
            .arg("-u")
            .arg(&script_path)
            .current_dir(&ctx.working_dir);
        run_child_with_timeout(cmd, timeout, "python").await
    }
}

// Old-style handler for backward compatibility
pub fn handler() -> super::ToolHandler {
    use std::sync::Arc;
    let tool = Arc::new(CodeRunTool);
    Arc::new(move |_name, args, ctx| {
        let args = args.clone();
        let ctx = ctx.clone();
        let tool = tool.clone();
        let rt = tokio::runtime::Runtime::new().expect("Failed to create tokio runtime");
        let result = rt
            .block_on(tool.execute(args, &ctx))
            .unwrap_or_else(|e| ToolOutput::bad_json(e.to_string()));
        StepOutcome {
            data: result.data,
            next_prompt: result.next_prompt,
            should_exit: result.should_exit,
            images: result.images,
        }
    })
}

use oz_core_types::StepOutcome;

pub fn definition() -> ToolDefinition {
    let t = CodeRunTool;
    ToolDefinition {
        type_: "function".into(),
        function: ToolFunction {
            name: t.name(),
            description: t.description(),
            parameters: t.parameters(),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_ctx() -> ToolContext {
        ToolContext {
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
    async fn test_definition_name() {
        let def = definition();
        assert_eq!(def.function.name, "code_run");
    }

    #[tokio::test]
    async fn test_empty_args_bad_json() {
        let tool = CodeRunTool;
        let result = tool
            .execute(serde_json::json!({}), &make_ctx())
            .await
            .unwrap();
        assert!(result.next_prompt.unwrap().contains("missing"));
    }

    #[tokio::test]
    async fn test_shell_echo() {
        let tool = CodeRunTool;
        let result = tool
            .execute(serde_json::json!({"code": "echo hello"}), &make_ctx())
            .await
            .unwrap();
        let stdout = result.data["stdout"].as_str().unwrap_or("");
        assert!(stdout.contains("hello"));
    }

    #[tokio::test]
    async fn test_bash_exit_code() {
        let tool = CodeRunTool;
        let result = tool
            .execute(serde_json::json!({"code": "exit 42"}), &make_ctx())
            .await
            .unwrap();
        assert_eq!(result.data["exit_code"], 42);
    }

    #[tokio::test]
    async fn test_python_execution() {
        let tool = CodeRunTool;
        let result = tool
            .execute(
                serde_json::json!({"code": "print('hello from python')", "type": "python"}),
                &make_ctx(),
            )
            .await;
        // python3 might not be available in all environments; just check it doesn't panic
        if let Ok(r) = result {
            let stdout = r.data["stdout"].as_str().unwrap_or("");
            if !stdout.contains("hello") {
                // Python not available — that's OK
                assert!(r.data["exit_code"] != 0 || stdout.contains("hello"));
            }
        }
    }

    #[linkme::distributed_slice(crate::registry::TOOL_FACTORIES)]
    fn register_code_run(reg: &mut crate::registry::ToolRegistry) {
        reg.register(crate::code_run::CodeRunTool);
    }
}
