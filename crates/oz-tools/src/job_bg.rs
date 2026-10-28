//! Background `code_run` producer — spawn a detached shell job, keep its output
//! in a bounded ring buffer, and register it with the process-wide JobRegistry.
//!
//! Used by `code_run { background: true }`. The job is owned by the calling
//! session; cancellation kills the whole process group (TERM, then KILL after
//! a grace period) so node/cargo/pytest descendants never leak.

use std::path::PathBuf;
use std::process::Stdio;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::process::Command;
use tokio::sync::oneshot;

use oz_core::jobs::{global, JobHooks, JobKind, JobOutcome, JobStart, JobStatus};

/// Per-job output cap. Anything beyond this is dropped from the front and the
/// reader reports a truncation marker once.
const OUTPUT_CAP: usize = 256 * 1024;
const LABEL_MAX: usize = 80;

struct Ring {
    buf: String,
    truncated: bool,
}

impl Ring {
    fn new() -> Self {
        Self {
            buf: String::new(),
            truncated: false,
        }
    }

    fn push(&mut self, line: &str) {
        if self.buf.len() + line.len() + 1 > OUTPUT_CAP {
            // Keep the newest tail — a failing build's error is at the end.
            let mut cut = self.buf.len() + line.len() + 1 - OUTPUT_CAP;
            while cut < self.buf.len() && !self.buf.is_char_boundary(cut) {
                cut += 1;
            }
            self.buf.drain(..cut.min(self.buf.len()));
            self.truncated = true;
        }
        self.buf.push_str(line);
        self.buf.push('\n');
    }

    fn take(&mut self) -> String {
        let mut out = std::mem::take(&mut self.buf);
        if std::mem::take(&mut self.truncated) {
            out.insert_str(0, "[… output truncated; earlier lines dropped]\n");
        }
        out
    }
}

fn short_label(code: &str) -> String {
    let one_line = code.lines().next().unwrap_or("").trim();
    let mut s: String = one_line.chars().take(LABEL_MAX).collect();
    if one_line.chars().count() > LABEL_MAX {
        s.push('…');
    }
    s
}

/// Kill a whole process group: TERM now, KILL after `grace` if it is still
/// alive. Safe to call twice.
fn kill_group(pid: u32, grace: Duration, exited: Arc<AtomicBool>) {
    if pid == 0 {
        return;
    }
    unsafe {
        libc::kill(-(pid as i32), libc::SIGTERM);
    }
    tokio::spawn(async move {
        tokio::time::sleep(grace).await;
        // Never escalate after the leader was reaped: the pgid may have been
        // recycled, and SIGKILL to a recycled group would hit a stranger.
        if exited.load(Ordering::SeqCst) {
            return;
        }
        unsafe {
            libc::kill(-(pid as i32), libc::SIGKILL);
        }
    });
}

/// Start a background shell job. Returns the job id.
pub fn start_bash(code: &str, cwd: &str, owner_session: &str) -> Result<String, String> {
    if owner_session.is_empty() {
        return Err("background jobs need a session owner".into());
    }
    let label = short_label(code);
    let code = code.to_string();
    let cwd = PathBuf::from(cwd);
    let ring = Arc::new(Mutex::new(Ring::new()));
    let pid_slot = Arc::new(Mutex::new(0u32));
    let registry = global();
    let (done_tx, done_rx) = oneshot::channel::<JobOutcome>();
    let cancelled = Arc::new(AtomicBool::new(false));
    let exited = Arc::new(AtomicBool::new(false));

    let ring_for_read = ring.clone();
    let pid_for_cancel = pid_slot.clone();
    let cancelled_cancel = cancelled.clone();
    let exited_for_cancel = exited.clone();

    let id = registry.start(JobStart {
        kind: JobKind::Bash,
        label: label.clone(),
        owner_session: owner_session.to_string(),
        run: Box::new(move |_ctx| {
            let mut child = Command::new("sh")
                .arg("-c")
                .arg(&code)
                .current_dir(&cwd)
                .stdin(Stdio::null())
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .process_group(0)
                .kill_on_drop(false)
                .spawn()
                .map_err(|e| format!("failed to start background command: {e}"))?;

            let pid = child.id().unwrap_or(0);
            *pid_slot.lock().unwrap() = pid;

            let stdout = child.stdout.take();
            let stderr = child.stderr.take();
            let r1 = ring.clone();
            let r2 = ring.clone();
            let h1 = tokio::spawn(async move {
                if let Some(s) = stdout {
                    let mut lines = BufReader::new(s).lines();
                    while let Ok(Some(line)) = lines.next_line().await {
                        r1.lock().unwrap().push(&line);
                    }
                }
            });
            let h2 = tokio::spawn(async move {
                if let Some(s) = stderr {
                    let mut lines = BufReader::new(s).lines();
                    while let Ok(Some(line)) = lines.next_line().await {
                        r2.lock().unwrap().push(&line);
                    }
                }
            });

            // Settlement: wait for the process AND both pipe readers, so the
            // job is only `completed` when its resources are truly released.
            let exited_settle = exited.clone();
            tokio::spawn(async move {
                let status = child.wait().await;
                exited_settle.store(true, Ordering::SeqCst);
                // A daemonized descendant can keep the pipes open forever.
                // Foreground code_run already has a drain grace; background
                // jobs need the same or they never settle.
                let _ = tokio::time::timeout(Duration::from_secs(5), async {
                    let _ = h1.await;
                    let _ = h2.await;
                })
                .await;
                let code = status.ok().and_then(|s| s.code());
                let killed = cancelled.load(Ordering::SeqCst);
                let outcome = JobOutcome {
                    status: if killed {
                        JobStatus::Killed
                    } else if code == Some(0) {
                        JobStatus::Completed
                    } else {
                        JobStatus::Failed
                    },
                    detail: Some(match code {
                        Some(c) => format!("exit code: {c}"),
                        None => "terminated by signal".into(),
                    }),
                };
                let _ = done_tx.send(outcome);
            });

            Ok(JobHooks {
                cancel: Box::new(move |_reason| {
                    cancelled_cancel.store(true, Ordering::SeqCst);
                    let pid = *pid_for_cancel.lock().unwrap();
                    kill_group(pid, Duration::from_secs(5), exited_for_cancel.clone());
                }),
                read_output: Some(Box::new(move || ring_for_read.lock().unwrap().take())),
                done: done_rx,
            })
        }),
    })?;

    Ok(id)
}

#[cfg(test)]
mod tests {
    use super::*;
    use oz_core::jobs::JobStatus;

    /// End-to-end through the real registry: output streams while running,
    /// cancel actually stops the process group, settlement is `killed`.
    #[tokio::test]
    async fn background_bash_streams_and_kills() {
        let owner = format!("test-{}", uuid::Uuid::new_v4());
        let id = start_bash("echo hello-bg; sleep 30", "/tmp", &owner).unwrap();
        let registry = global();

        let mut text = String::new();
        for _ in 0..40 {
            tokio::time::sleep(Duration::from_millis(100)).await;
            let (chunk, _) = registry.read(&id, &owner).unwrap();
            text.push_str(&chunk);
            if text.contains("hello-bg") {
                break;
            }
        }
        assert!(text.contains("hello-bg"), "missing output, got: {text:?}");

        let snap = registry.cancel(&id, &owner, Some("test".into())).unwrap();
        assert_eq!(snap.status, JobStatus::Stopping);
        let done = registry
            .wait(&id, &owner, Duration::from_secs(10))
            .await
            .unwrap();
        assert_eq!(done.status, JobStatus::Killed, "detail={:?}", done.detail);
    }
}
