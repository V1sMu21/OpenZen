//! Audit log — records all tool calls for security audit trail.
//!
//! Each entry includes timestamp, session, tool name, argument summary, and result.
//!
//! P1-10 ①: the production entry point is the pair
//! [`set_audit_dir`] / [`log_tool_call`], called from the agent loop for every
//! dispatched tool call *after* the approval decision. The older [`AuditLog`]
//! type is kept for library users but had zero call sites — which is exactly
//! why "what did the agent actually do?" was unanswerable.
//!
//! Guarantees of the production path:
//! * **sanitized**: never file contents — a command keeps its first 200
//!   chars, a path is verbatim, a URL loses its query, everything else is
//!   reduced to its key list ([`digest_args`]);
//! * **rotated daily** (`audit-YYYY-MM-DD.jsonl`) with a 14-day retention, so
//!   the trail cannot grow without bound;
//! * **one `write_all` per line**, so concurrent writers cannot interleave;
//! * **inert until configured**: without [`set_audit_dir`] every call is a
//!   no-op, keeping unit tests and embedded users off the filesystem.

use std::fs::OpenOptions;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};

use chrono::Utc;
use serde::Serialize;

/// Directory the host wants audit logs in. `None` = auditing disabled.
static AUDIT_DIR: OnceLock<PathBuf> = OnceLock::new();
/// Retention window for rotated files.
const AUDIT_RETENTION_DAYS: i64 = 14;
/// Maximum length of a recorded command.
const MAX_COMMAND_CHARS: usize = 200;
/// Serializes appends inside this process.
static APPEND_LOCK: Mutex<()> = Mutex::new(());

/// Point the audit log at a directory (called once at host startup).
///
/// Idempotent: the first call wins, so a later call cannot silently redirect
/// an audit trail that is already being written.
pub fn set_audit_dir(dir: PathBuf) {
    let _ = AUDIT_DIR.set(dir);
}

/// Where the audit log is written (None = disabled). Test/diagnostic helper.
pub fn audit_dir() -> Option<&'static Path> {
    AUDIT_DIR.get().map(|p| p.as_path())
}

/// Sanitized one-line digest of a tool call's arguments. NEVER includes file
/// contents: a write's `content`, a command's payload beyond 200 chars, or a
/// URL's query string must not end up in the audit trail.
pub fn digest_args(tool: &str, args: &serde_json::Value) -> String {
    if tool == "code_run" {
        if let Some(code) = args.get("code").and_then(|v| v.as_str()) {
            let folded = code.replace('\n', "⏎").replace('\r', "");
            let head: String = folded.chars().take(MAX_COMMAND_CHARS).collect();
            return redact_secrets(&head);
        }
    }
    if let Some(path) = args.get("file_path").and_then(|v| v.as_str()) {
        // Keep the shape, drop the home directory: a path like
        // /Users/<name>/… is an identity leak in a log that lives for 14 days.
        return redact_home(path);
    }
    if let Some(url) = args.get("url").and_then(|v| v.as_str()) {
        // Query AND fragment: `#access_token=…` is just as sensitive as `?…`.
        let no_frag = match url.split_once('#') {
            Some((head, _)) => format!("{head}#<redacted>"),
            None => url.to_string(),
        };
        return match no_frag.split_once('?') {
            Some((head, _)) => format!("{head}?<redacted>"),
            None => no_frag,
        };
    }
    if let Some(pattern) = args.get("pattern").and_then(|v| v.as_str()) {
        let path = args.get("path").and_then(|v| v.as_str()).unwrap_or("");
        return if path.is_empty() {
            pattern.to_string()
        } else {
            format!("{pattern} @ {}", redact_home(path))
        };
    }
    match args.as_object() {
        Some(map) if !map.is_empty() => {
            let keys: Vec<&str> = map.keys().map(|k| k.as_str()).collect();
            format!("keys: {}", keys.join(","))
        }
        _ => String::new(),
    }
}

/// Mask the user's home directory in a path (`/Users/x/…` → `~/…`).
fn redact_home(path: &str) -> String {
    if let Some(home) = std::env::var_os("HOME").and_then(|h| h.into_string().ok()) {
        if !home.is_empty() && path.starts_with(&home) {
            return format!("~{}", &path[home.len()..]);
        }
    }
    if let Some(rest) = path.strip_prefix("/Users/") {
        if let Some((_user, tail)) = rest.split_once('/') {
            return format!("~/…/{tail}");
        }
    }
    path.to_string()
}

/// Mask obvious credentials in a command line.
///
/// The first 200 characters of a `code_run` are the most useful audit data
/// there is, but they can also contain `export TOKEN=…` or `Bearer …`.
fn redact_secrets(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    // Once a sensitive marker appears, everything up to the end of the quoted
    // span (or the next `&&`/`;`/`|`) is masked: `-H 'Authorization: Bearer
    // tok123'` is one secret spread over three whitespace tokens.
    let mut masking = false;
    for token in s.split_inclusive([' ', '⏎']) {
        let lower = token.to_lowercase();
        let opens_secret = lower.contains("token=")
            || lower.contains("api_key=")
            || lower.contains("apikey=")
            || lower.contains("password=")
            || lower.contains("secret=")
            || lower.contains("bearer")
            || lower.contains("sk-")
            || lower.contains("authorization:");
        if opens_secret {
            masking = true;
        }
        if masking {
            out.push_str("<redacted> ");
            // A closing quote or a command separator ends the secret.
            if token.contains('\'')
                || token.contains('"')
                || token.contains("&&")
                || token.contains(';')
            {
                masking = false;
            }
            continue;
        }
        out.push_str(token);
    }
    out.trim_end().to_string()
}

/// Append one audit row. Silent no-op when [`set_audit_dir`] was never called.
pub fn log_tool_call(session_id: &str, tool: &str, args_digest: &str, decision: &str) {
    let Some(dir) = AUDIT_DIR.get() else {
        return;
    };
    let today = Utc::now().format("%Y-%m-%d").to_string();
    let path = dir.join(format!("audit-{today}.jsonl"));
    let row = serde_json::json!({
        "ts": Utc::now().to_rfc3339(),
        "session_id": session_id,
        "tool": tool,
        "args": args_digest,
        "decision": decision,
    });
    let mut buf = match serde_json::to_vec(&row) {
        Ok(b) => b,
        Err(_) => return,
    };
    buf.push(b'\n');
    let _guard = APPEND_LOCK.lock().unwrap_or_else(|p| p.into_inner());
    if std::fs::create_dir_all(dir).is_err() {
        return;
    }
    match OpenOptions::new().create(true).append(true).open(&path) {
        Ok(mut f) => {
            if let Err(e) = f.write_all(&buf) {
                tracing::warn!("[audit] write failed: {e}");
                return;
            }
            let _ = f.flush();
        }
        Err(e) => {
            tracing::warn!("[audit] cannot open {}: {e}", path.display());
            return;
        }
    }
    prune_old(dir);
}

/// Remove audit files older than the retention window.
fn prune_old(dir: &Path) {
    let cutoff = (Utc::now() - chrono::Duration::days(AUDIT_RETENTION_DAYS))
        .format("%Y-%m-%d")
        .to_string();
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let name = entry.file_name().to_string_lossy().to_string();
        let Some(stamp) = name
            .strip_prefix("audit-")
            .and_then(|s| s.strip_suffix(".jsonl"))
        else {
            continue;
        };
        if stamp < cutoff.as_str() {
            if let Err(e) = std::fs::remove_file(entry.path()) {
                tracing::warn!("[audit] failed to prune {}: {e}", entry.path().display());
            }
        }
    }
}

#[derive(Debug, Serialize)]
pub struct AuditEntry {
    pub timestamp: String,
    pub session_id: String,
    pub tool: String,
    pub args_summary: String,
    pub result: String,
    pub decision: String,
}

pub struct AuditLog {
    path: PathBuf,
    writer: Mutex<Option<std::fs::File>>,
}

impl AuditLog {
    pub fn new(path: PathBuf) -> Self {
        let writer = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&path)
            .ok();
        AuditLog {
            path,
            writer: Mutex::new(writer),
        }
    }

    pub fn record(&self, entry: AuditEntry) {
        if let Ok(mut guard) = self.writer.lock() {
            if let Some(ref mut f) = *guard {
                let line = serde_json::to_string(&entry).unwrap_or_default();
                let _ = writeln!(f, "{line}");
                return;
            }
        }
        // Fallback: trace only
        tracing::info!(
            "[audit] {tool} | session={sid} | {decision} | {summary}",
            tool = entry.tool,
            sid = entry.session_id,
            decision = entry.decision,
            summary = entry.args_summary,
        );
    }

    pub fn record_tool_call(
        &self,
        session_id: &str,
        tool: &str,
        args: &serde_json::Value,
        result: &str,
        decision: &str,
    ) {
        let args_summary = serde_json::to_string(args)
            .map(|s| {
                if s.len() > 200 {
                    // Truncate on a char boundary — byte 200 may fall inside a
                    // multi-byte UTF-8 char (e.g. CJK) in serialized args.
                    format!("{}...", &s[..s.floor_char_boundary(200)])
                } else {
                    s
                }
            })
            .unwrap_or_default();

        self.record(AuditEntry {
            timestamp: Utc::now().to_rfc3339(),
            session_id: session_id.to_string(),
            tool: tool.to_string(),
            args_summary,
            result: result.to_string(),
            decision: decision.to_string(),
        });
    }
}

impl Clone for AuditLog {
    fn clone(&self) -> Self {
        AuditLog::new(self.path.clone())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_record_tool_call_multibyte_args_no_panic() {
        // Regression: serialized args over 200 bytes with CJK chars used to
        // panic on `&s[..200]` when byte 200 fell inside a multi-byte char.
        let log = AuditLog::new(std::path::PathBuf::from("/nonexistent/audit.jsonl"));
        let args = serde_json::json!({"cmd": "修复桌面端的问题 ".repeat(20)});
        log.record_tool_call("sess-1", "code_run", &args, "ok", "allowed");
    }

    #[test]
    fn test_record_tool_call_ascii_args_unchanged() {
        let log = AuditLog::new(std::path::PathBuf::from("/nonexistent/audit.jsonl"));
        let args = serde_json::json!({"cmd": "echo hi"});
        log.record_tool_call("sess-1", "code_run", &args, "ok", "allowed");
    }

    /// The digest must never leak file contents or query strings.
    #[test]
    fn digest_is_sanitized() {
        let d = digest_args(
            "code_run",
            &serde_json::json!({"code": "echo hello\nrm -rf /tmp/x"}),
        );
        assert!(d.starts_with("echo hello⏎rm -rf"), "newlines folded: {d}");
        assert!(!d.contains('\n'));

        let long = "x".repeat(500);
        let d2 = digest_args("code_run", &serde_json::json!({"code": long}));
        assert_eq!(d2.chars().count(), MAX_COMMAND_CHARS);

        let d3 = digest_args(
            "web_fetch",
            &serde_json::json!({"url": "https://x.test/a?token=SECRET"}),
        );
        assert!(!d3.contains("SECRET"), "query string leaked: {d3}");
        assert!(d3.starts_with("https://x.test/a"));

        let d4 = digest_args(
            "write",
            &serde_json::json!({"file_path": "/p/a.rs", "content": "SECRET BODY"}),
        );
        assert_eq!(d4, "/p/a.rs");
        assert!(!d4.contains("SECRET"));

        let d5 = digest_args(
            "skill_mcp_store",
            &serde_json::json!({"name": "n", "body": "SECRET"}),
        );
        assert!(
            d5.starts_with("keys: "),
            "unknown tools record keys only: {d5}"
        );
        assert!(!d5.contains("SECRET"));
    }

    /// Without `set_audit_dir` the module is a no-op (unit tests and library
    /// users must not write to disk).
    #[test]
    fn unset_dir_is_a_silent_no_op() {
        if audit_dir().is_some() {
            return; // another test configured it process-wide
        }
        log_tool_call("s", "read", "/tmp/x", "auto_allowed");
        assert!(audit_dir().is_none());
    }

    /// Review W2: the digest used to keep URL fragments and command secrets.
    #[test]
    fn digests_do_not_leak_credentials() {
        let home = std::env::var("HOME").unwrap_or_else(|_| "/Users/someone".into());
        let d = digest_args(
            "read",
            &serde_json::json!({ "file_path": format!("{home}/Documents/secret.txt") }),
        );
        assert!(!d.contains(&home), "home directory leaked: {d}");
        assert!(d.contains("secret.txt"), "the file name must survive: {d}");

        let d = digest_args(
            "web_fetch",
            &serde_json::json!({ "url": "https://x.dev/a#access_token=SECRET123" }),
        );
        assert!(!d.contains("SECRET123"), "fragment token leaked: {d}");

        let d = digest_args(
            "code_run",
            &serde_json::json!({ "code": "export API_KEY=sk-live-abcdef && curl -H 'Authorization: Bearer tok123' https://x" }),
        );
        assert!(!d.contains("sk-live-abcdef"), "api key leaked: {d}");
        assert!(!d.contains("tok123"), "bearer token leaked: {d}");
        assert!(d.contains("curl"), "the command shape must survive: {d}");
    }
}
