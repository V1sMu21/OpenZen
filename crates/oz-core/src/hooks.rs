//! Hook mechanism — user-configured automation around agent loop events.
//!
//! Hooks are declared in `~/.openzen/hooks.toml`:
//! ```toml
//! [[session_start]]
//! command = "git branch --show-current"
//!
//! [[post_tool_use]]
//! matcher = "write"
//! command = "npx prettier --write {file}"
//! ```
//!
//! Commands are user-authored policy (same trust level as `permissions.toml`),
//! so they run as-is. They never block the loop: PostToolUse hooks fire and
//! forget with a 5s timeout; SessionStart hooks wait at most 5s for output.

use std::path::Path;
use std::process::Stdio;
use std::sync::mpsc;
use std::time::Duration;

use serde::Deserialize;

const HOOK_TIMEOUT: Duration = Duration::from_secs(5);

/// Events that can trigger hooks.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HookEvent {
    /// Fired once at loop start; returned string is injected into the reminder.
    SessionStart,
    /// Fired after a tool call completes. `file` is set when the tool
    /// operates on a single file (write/edit/patch).
    PostToolUse { tool: String, file: Option<String> },
}

/// Hook handler: fires on events, optionally returning context to inject.
pub trait HookHandler: Send + Sync {
    fn fire(&self, evt: &HookEvent) -> Option<String>;
}

/// One `[[session_start]]` rule.
#[derive(Debug, Clone, Deserialize)]
struct SessionStartRule {
    command: String,
}

/// One `[[post_tool_use]]` rule.
#[derive(Debug, Clone, Deserialize)]
struct PostToolUseRule {
    matcher: String,
    command: String,
}

#[derive(Debug, Default, Deserialize)]
struct HooksFile {
    #[serde(default)]
    session_start: Vec<SessionStartRule>,
    #[serde(default)]
    post_tool_use: Vec<PostToolUseRule>,
}

/// TOML-backed hook handler loaded from `<data_dir>/hooks.toml`.
pub struct TomlHooks {
    session_start_commands: Vec<String>,
    post_tool_use: Vec<PostToolUseRule>,
    permissions: Option<oz_safety::Permissions>,
}

impl TomlHooks {
    /// Load from `<dir>/hooks.toml`; missing/unreadable → None (no hooks).
    pub fn load_from_dir(dir: &Path) -> Option<Self> {
        let data = std::fs::read_to_string(dir.join("hooks.toml")).ok()?;
        let permissions = oz_safety::Permissions::load_from_dir(dir);
        let mut hooks = Self::from_toml(&data)?;
        if !permissions.rules.is_empty() {
            hooks.permissions = Some(permissions);
        }
        Some(hooks)
    }

    /// Parse TOML content; on parse error → None.
    pub fn from_toml(data: &str) -> Option<Self> {
        let file: HooksFile = toml::from_str(data).ok()?;
        Some(TomlHooks {
            session_start_commands: file.session_start.into_iter().map(|r| r.command).collect(),
            post_tool_use: file.post_tool_use,
            permissions: None,
        })
    }

    fn run_session_start(&self) -> Option<String> {
        let mut outputs = Vec::new();
        for cmd in &self.session_start_commands {
            if let Some(out) = run_command_timeout(cmd, None) {
                outputs.push(out);
            }
        }
        if outputs.is_empty() {
            None
        } else {
            Some(outputs.join("\n"))
        }
    }

    fn run_post_tool_use(&self, tool: &str, file: Option<&str>) {
        for rule in &self.post_tool_use {
            if rule.matcher != tool {
                continue;
            }
            if self.is_blocked(tool, file) {
                tracing::warn!(
                    "[hooks] command blocked by permission policy: {}",
                    rule.command
                );
                continue;
            }
            run_command_detached(&rule.command, file);
        }
    }

    /// Hooks inherit P1-4 permission policy: `{file}` is LLM-controlled, so a
    /// deny rule on (tool, file) must block the hook command too.
    fn is_blocked(&self, tool: &str, file: Option<&str>) -> bool {
        match (&self.permissions, file) {
            (Some(perms), Some(f)) => perms.check(tool, f) == oz_safety::Decision::Deny,
            _ => false,
        }
    }
}

impl HookHandler for TomlHooks {
    fn fire(&self, evt: &HookEvent) -> Option<String> {
        match evt {
            HookEvent::SessionStart => self.run_session_start(),
            HookEvent::PostToolUse { tool, file } => {
                self.run_post_tool_use(tool, file.as_deref());
                None
            }
        }
    }
}

/// Run a command synchronously, substituting `{file}`, bounded by HOOK_TIMEOUT.
/// Returns trimmed stdout on success, None on failure/timeout.
/// On timeout the child is killed so no orphan process survives the hook.
fn run_command_timeout(command: &str, file: Option<&str>) -> Option<String> {
    let (cmd, args) = split_command(command, file);
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        let mut child = std::process::Command::new(&cmd)
            .args(&args)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .ok();
        let deadline = std::time::Instant::now() + HOOK_TIMEOUT;
        let status = loop {
            let Some(c) = child.as_mut() else {
                break None;
            };
            match c.try_wait() {
                Ok(Some(_)) => {
                    let child = child.take().unwrap();
                    break child.wait_with_output().ok();
                }
                Ok(None) if std::time::Instant::now() >= deadline => {
                    let _ = c.kill();
                    let _ = c.wait();
                    break None;
                }
                Ok(None) => std::thread::sleep(std::time::Duration::from_millis(20)),
                Err(_) => break None,
            }
        };
        let _ = tx.send(status);
    });
    match rx.recv_timeout(HOOK_TIMEOUT + std::time::Duration::from_secs(1)) {
        Ok(Some(out)) if out.status.success() => {
            let s = String::from_utf8_lossy(&out.stdout).trim().to_string();
            if s.is_empty() {
                None
            } else {
                Some(s)
            }
        }
        Ok(Some(_)) => None,
        Ok(None) => {
            tracing::warn!("[hooks] command timed out after 5s: {command}");
            None
        }
        Err(_) => {
            tracing::warn!("[hooks] command timed out after 5s: {command}");
            None
        }
    }
}

/// Fire-and-forget: spawn the command detached with HOOK_TIMEOUT, logging failures.
fn run_command_detached(command: &str, file: Option<&str>) {
    let command = command.to_string();
    let (cmd, args) = split_command(&command, file);
    tokio::spawn(async move {
        match tokio::time::timeout(
            HOOK_TIMEOUT,
            tokio::process::Command::new(&cmd)
                .args(&args)
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .output(),
        )
        .await
        {
            Ok(Ok(out)) if out.status.success() => {}
            Ok(Ok(out)) => {
                let err = String::from_utf8_lossy(&out.stderr);
                tracing::warn!("[hooks] command failed ({}): {err}", out.status);
            }
            Ok(Err(e)) => tracing::warn!("[hooks] command failed to spawn: {e}"),
            Err(_) => tracing::warn!("[hooks] command timed out after 5s: {command}"),
        }
    });
}

/// Split a shell command string into program + args, substituting `{file}`.
fn split_command(command: &str, file: Option<&str>) -> (String, Vec<String>) {
    let substituted = match file {
        Some(f) => command.replace("{file}", f),
        None => command.to_string(),
    };
    let mut parts = substituted.split_whitespace();
    let program = parts.next().unwrap_or("").to_string();
    let args: Vec<String> = parts.map(|s| s.to_string()).collect();
    (program, args)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_from_toml_parses_rules() {
        let toml = r#"
[[session_start]]
command = "git branch --show-current"

[[post_tool_use]]
matcher = "write"
command = "npx prettier --write {file}"
"#;
        let hooks = TomlHooks::from_toml(toml).expect("valid toml");
        assert_eq!(hooks.session_start_commands.len(), 1);
        assert_eq!(hooks.post_tool_use.len(), 1);
        assert_eq!(hooks.post_tool_use[0].matcher, "write");
    }

    #[test]
    fn test_from_toml_empty_is_none() {
        assert!(TomlHooks::from_toml("").is_some());
        assert!(TomlHooks::from_toml("not toml [[[").is_none());
    }

    #[test]
    fn test_load_from_dir_missing_is_none() {
        let dir = std::env::temp_dir().join("oz-core-hooks-missing");
        assert!(TomlHooks::load_from_dir(&dir).is_none());
    }

    #[test]
    fn test_split_command_substitutes_file() {
        let (cmd, args) = split_command("npx prettier --write {file}", Some("/tmp/a.rs"));
        assert_eq!(cmd, "npx");
        assert_eq!(args, vec!["prettier", "--write", "/tmp/a.rs"]);
    }

    #[test]
    fn test_split_command_no_file() {
        let (cmd, args) = split_command("git status --short", None);
        assert_eq!(cmd, "git");
        assert_eq!(args, vec!["status", "--short"]);
    }

    #[test]
    fn test_matcher_only_fires_matching_tool() {
        let hooks = TomlHooks::from_toml(
            r#"
[[post_tool_use]]
matcher = "write"
command = "echo hook-fired"
"#,
        )
        .unwrap();
        // Non-matching tool: no crash, no output.
        assert_eq!(
            hooks.fire(&HookEvent::PostToolUse {
                tool: "read".into(),
                file: None
            }),
            None
        );
    }

    #[test]
    fn test_deny_rule_blocks_hook_command() {
        let mut hooks = TomlHooks::from_toml(
            r#"
[[post_tool_use]]
matcher = "write"
command = "echo should-not-run"
"#,
        )
        .unwrap();
        hooks.permissions = Some(oz_safety::Permissions::from_toml(
            r#"
[[rules]]
tool = "write"
pattern = "/etc/**"
decision = "deny"
"#,
        ));
        assert!(hooks.is_blocked("write", Some("/etc/passwd")));
        assert!(!hooks.is_blocked("write", Some("/tmp/ok.txt")));
        assert!(!hooks.is_blocked("read", Some("/etc/passwd")));
        // Blocked hook fires but the command is skipped (no output to collect).
        assert_eq!(
            hooks.fire(&HookEvent::PostToolUse {
                tool: "write".into(),
                file: Some("/etc/passwd".into())
            }),
            None
        );
    }

    #[test]
    fn test_session_start_runs_command() {
        let hooks = TomlHooks::from_toml(
            r#"
[[session_start]]
command = "echo hello-from-hook"
"#,
        )
        .unwrap();
        let out = hooks.fire(&HookEvent::SessionStart);
        assert_eq!(out.as_deref(), Some("hello-from-hook"));
    }

    #[test]
    fn test_session_start_hanging_command_times_out() {
        // A command that never exits must be killed after HOOK_TIMEOUT and
        // yield None — no orphan process survives the hook.
        let start = std::time::Instant::now();
        let out = run_command_timeout("sleep 30", None);
        assert!(out.is_none());
        assert!(start.elapsed() < HOOK_TIMEOUT + std::time::Duration::from_secs(2));
    }

    #[tokio::test]
    async fn test_post_tool_use_detached_runs() {
        let hooks = TomlHooks::from_toml(
            r#"
[[post_tool_use]]
matcher = "write"
command = "touch /tmp/oz-hook-ran.txt"
"#,
        )
        .unwrap();
        let _ = std::fs::remove_file("/tmp/oz-hook-ran.txt");
        hooks.fire(&HookEvent::PostToolUse {
            tool: "write".into(),
            file: None,
        });
        // Give the detached task time to complete.
        for _ in 0..50 {
            if std::path::Path::new("/tmp/oz-hook-ran.txt").exists() {
                let _ = std::fs::remove_file("/tmp/oz-hook-ran.txt");
                return;
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
        panic!("detached hook command did not run within 5s");
    }
}
