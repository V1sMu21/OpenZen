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

/// Grace window granted to the stdout/stderr readers after the direct child
/// has exited.
///
/// `Child::wait_with_output()` waits for the pipes to reach EOF, and a
/// descendant that inherited them (a `cmd &` with no redirect, a dev server,
/// a node/python process tree) keeps them open for as long as it lives. A
/// command that finished in milliseconds therefore blocked until the tool's
/// declared timeout — up to 1800s — and then reported a bogus timeout with
/// its output thrown away. This grace is all a well-behaved pipeline needs to
/// flush; after it we stop reading and return what we have.
const PIPE_DRAIN_GRACE: std::time::Duration = std::time::Duration::from_millis(1200);

/// Cap on the output `code_run` keeps per stream (stdout, stderr).
///
/// A command can print gigabytes inside its declared timeout — a progress
/// loop, `yes`, a verbose build — and every byte used to end up in the tool
/// result, the session store and the next prompt. Keep the head (early
/// context) and the tail (the error or result that matters), and say in
/// between how much was dropped.
const OUTPUT_HEAD_CAP: usize = 256 * 1024;
const OUTPUT_TAIL_CAP: usize = 256 * 1024;

/// Bounded pipe accumulator: head + tail with a dropped-byte count, so a
/// process that spews output can neither grow memory without limit nor push a
/// multi-gigabyte tool result into the session store.
#[derive(Default)]
struct PipeBuf {
    head: Vec<u8>,
    tail: std::collections::VecDeque<u8>,
    dropped: usize,
}

impl PipeBuf {
    fn push(&mut self, chunk: &[u8]) {
        if self.head.len() < OUTPUT_HEAD_CAP {
            let room = OUTPUT_HEAD_CAP - self.head.len();
            let take = room.min(chunk.len());
            self.head.extend_from_slice(&chunk[..take]);
            self.push_tail(&chunk[take..]);
        } else {
            self.push_tail(chunk);
        }
    }

    fn push_tail(&mut self, chunk: &[u8]) {
        if chunk.is_empty() {
            return;
        }
        if chunk.len() >= OUTPUT_TAIL_CAP {
            self.dropped += self.tail.len() + chunk.len() - OUTPUT_TAIL_CAP;
            self.tail.clear();
            self.tail
                .extend(chunk[chunk.len() - OUTPUT_TAIL_CAP..].iter().copied());
            return;
        }
        self.tail.extend(chunk.iter().copied());
        while self.tail.len() > OUTPUT_TAIL_CAP {
            self.tail.pop_front();
            self.dropped += 1;
        }
    }

    fn finish(&mut self) -> String {
        let head = String::from_utf8_lossy(&self.head).to_string();
        let tail = String::from_utf8_lossy(self.tail.make_contiguous()).to_string();
        if self.dropped == 0 {
            return format!("{head}{tail}");
        }
        format!(
            "{head}\n[... {} bytes omitted: output capped at {} bytes (head+tail kept) ...]\n{tail}",
            self.dropped,
            OUTPUT_HEAD_CAP + OUTPUT_TAIL_CAP
        )
    }
}

/// Drain one pipe to EOF into a bounded buffer on a background task, so a pipe
/// that stays open can never block the caller.
async fn read_pipe_into<R>(reader: Option<R>, buf: std::sync::Arc<std::sync::Mutex<PipeBuf>>)
where
    R: tokio::io::AsyncRead + Unpin,
{
    use tokio::io::AsyncReadExt;
    let Some(mut reader) = reader else { return };
    let mut chunk = [0u8; 8192];
    loop {
        match reader.read(&mut chunk).await {
            Ok(0) | Err(_) => break,
            Ok(n) => {
                let mut out = buf.lock().unwrap_or_else(|e| e.into_inner());
                out.push(&chunk[..n]);
            }
        }
    }
}

/// Take whatever the readers collected so far (leaving the buffer empty).
fn take_pipe_buf(buf: &std::sync::Arc<std::sync::Mutex<PipeBuf>>) -> String {
    let mut guard = buf.lock().unwrap_or_else(|e| e.into_inner());
    guard.finish()
}

/// SIGKILL the whole process group created for this child.
///
/// Killing only the direct child left the real work behind — `sh -c "npm
/// start"` dies, node lives on, and the tool still claimed the process was
/// killed while the port stayed occupied.
#[cfg(unix)]
fn kill_process_tree(pid: Option<u32>) {
    let Some(pid) = pid else { return };
    // SAFETY: kill(2) with a negative pid signals the process group the child
    // was placed in via `process_group(0)`. No memory is touched.
    unsafe {
        libc::kill(-(pid as i32), libc::SIGKILL);
    }
}

#[cfg(not(unix))]
fn kill_process_tree(_pid: Option<u32>) {}

/// Spawn a child with kill-on-drop so that cancelling the surrounding
/// future (tool timeout, user stop, run abort) never leaves an orphan
/// `sh`/`python3` behind — a 7x24 agent leaks one process per timeout
/// otherwise. Returns the collected output.
///
/// The wait is bounded by `timeout` and by [`PIPE_DRAIN_GRACE`], never by the
/// lifetime of an inherited pipe, and whatever output was collected is always
/// returned (partial output on timeout, instead of the empty string this used
/// to swallow).
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
    // Own process group: a timeout can then kill the whole tree, and a
    // runaway command can never signal this daemon's own group.
    #[cfg(unix)]
    command.process_group(0);
    let mut child = command
        .spawn()
        .map_err(|e| ToolError::Custom(format!("{what} execution failed: {e}. Verify the command syntax and that required tools are installed.")))?;
    let pid = child.id();

    let out_buf = std::sync::Arc::new(std::sync::Mutex::new(PipeBuf::default()));
    let err_buf = std::sync::Arc::new(std::sync::Mutex::new(PipeBuf::default()));
    let mut out_task = tokio::spawn(read_pipe_into(child.stdout.take(), out_buf.clone()));
    let mut err_task = tokio::spawn(read_pipe_into(child.stderr.take(), err_buf.clone()));

    // Wait for the direct child only — never for pipe EOF.
    let waited = tokio::time::timeout(
        std::time::Duration::from_secs(timeout.max(1)),
        child.wait(),
    )
    .await;
    let timed_out = waited.is_err();
    if timed_out {
        kill_process_tree(pid);
        let _ = child.start_kill();
    }

    // Bounded drain: give the readers a moment to flush, then stop reading.
    let _ = tokio::time::timeout(PIPE_DRAIN_GRACE, async {
        let _ = (&mut out_task).await;
        let _ = (&mut err_task).await;
    })
    .await;
    let drained = out_task.is_finished() && err_task.is_finished();
    out_task.abort();
    err_task.abort();

    let stdout = take_pipe_buf(&out_buf);
    let mut stderr = take_pipe_buf(&err_buf);

    if timed_out {
        if !stderr.is_empty() && !stderr.ends_with('\n') {
            stderr.push('\n');
        }
        stderr.push_str(&format!(
            "{what} timed out after {timeout}s (process group killed; output above is partial)"
        ));
        return Ok(serde_json::json!({
            "exit_code": -1,
            "stdout": stdout,
            "stderr": stderr,
            "timeout": true,
            "partial_output": true,
        }));
    }

    let exit_code = waited
        .ok()
        .and_then(|r| r.ok())
        .and_then(|status| status.code())
        .unwrap_or(-1);
    let mut result = serde_json::json!({
        "exit_code": exit_code,
        "stdout": stdout,
        "stderr": stderr,
    });
    if !drained {
        // A background descendant is still holding the pipes. It keeps
        // running — we do not kill intentional background work — but the
        // model must know the output may be incomplete and that redirecting
        // is how you get an immediate return.
        result["detached_output"] = serde_json::json!(true);
        const NOTE: &str = "\n[code_run] the command exited but a background descendant still holds \
             stdout/stderr; it keeps running detached and the output above may be truncated. \
             Redirect long-lived output (e.g. `nohup cmd >/tmp/cmd.log 2>&1 &`) so the tool \
             returns immediately.";
        let existing = result["stderr"].as_str().unwrap_or("").to_string();
        result["stderr"] = serde_json::json!(format!("{existing}{NOTE}"));
    }
    Ok(result)
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

    #[test]
    fn pipe_buf_small_output_is_verbatim() {
        let mut buf = PipeBuf::default();
        buf.push(b"hello ");
        buf.push(b"world\n");
        assert_eq!(buf.finish(), "hello world\n");
    }

    #[test]
    fn pipe_buf_keeps_head_and_tail_and_counts_dropped() {
        let mut buf = PipeBuf::default();
        buf.push(b"start");
        buf.push(&vec![b'x'; OUTPUT_HEAD_CAP]);
        buf.push(&vec![b'y'; OUTPUT_TAIL_CAP + 10]);
        let out = buf.finish();
        assert!(out.starts_with("startxxx"), "head lost: {}", &out[..32]);
        assert!(out.ends_with("yyy"), "tail lost");
        assert!(out.contains("bytes omitted"));
        assert!(
            out.len() <= OUTPUT_HEAD_CAP + OUTPUT_TAIL_CAP + 256,
            "cap not honoured: {} bytes",
            out.len()
        );
    }

    /// A background descendant that inherits stdout/stderr must not extend the
    /// tool call: `sh -c 'sleep 30 & echo launched'` finishes in milliseconds,
    /// yet `wait_with_output()` used to block on the inherited pipes until the
    /// declared timeout and then report a bogus timeout with empty output.
    #[tokio::test]
    async fn test_shell_returns_immediately_when_descendant_holds_pipes() {
        let tool = CodeRunTool;
        let started = std::time::Instant::now();
        let result = tool
            .execute(
                serde_json::json!({"code": "sleep 30 & echo launched", "timeout": 20}),
                &make_ctx(),
            )
            .await
            .unwrap();
        let elapsed = started.elapsed();
        let stdout = result.data["stdout"].as_str().unwrap_or("");
        assert!(stdout.contains("launched"), "stdout was {stdout:?}");
        assert_eq!(result.data["exit_code"], 0);
        assert!(
            result.data["timeout"].is_null(),
            "must not report a timeout: {}",
            result.data
        );
        assert_eq!(result.data["detached_output"], true);
        assert!(
            elapsed.as_secs() < 8,
            "blocked on the inherited pipe for {elapsed:?}"
        );
    }

    /// On timeout: keep the output the command already produced, and kill the
    /// whole process tree (the old code killed only the direct `sh`, leaving
    /// the real work running, and discarded every byte it had written).
    #[tokio::test]
    async fn test_timeout_keeps_partial_output_and_kills_process_tree() {
        let tool = CodeRunTool;
        let marker = "oz_code_run_tree_marker_7f3c";
        let script = format!(
            "echo {marker}_started; sh -c 'sleep 47 # {marker}' & sleep 300 # {marker}"
        );
        let started = std::time::Instant::now();
        let result = tool
            .execute(
                serde_json::json!({"code": script, "timeout": 2}),
                &make_ctx(),
            )
            .await
            .unwrap();
        let elapsed = started.elapsed();

        let stdout = result.data["stdout"].as_str().unwrap_or("");
        assert!(
            stdout.contains(&format!("{marker}_started")),
            "partial output was discarded: {stdout:?}"
        );
        assert_eq!(result.data["timeout"], true);
        assert_eq!(result.data["partial_output"], true);
        assert!(result.data["stderr"]
            .as_str()
            .unwrap_or("")
            .contains("timed out"));
        assert!(
            elapsed.as_secs() < 15,
            "timeout path took {elapsed:?}"
        );

        // The descendant must be gone too (process-group kill).
        std::thread::sleep(std::time::Duration::from_millis(600));
        let ps = std::process::Command::new("ps")
            .args(["-Ao", "command"])
            .output()
            .expect("ps");
        let listing = String::from_utf8_lossy(&ps.stdout);
        assert!(
            !listing.contains(marker),
            "timed-out descendants survived the kill:\n{listing}"
        );
    }

    #[linkme::distributed_slice(crate::registry::TOOL_FACTORIES)]
    fn register_code_run(reg: &mut crate::registry::ToolRegistry) {
        reg.register(crate::code_run::CodeRunTool);
    }
}
