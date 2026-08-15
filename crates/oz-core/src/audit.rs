//! Audit log — records all tool calls for security audit trail.
//!
//! Each entry includes timestamp, session, tool name, argument summary, and result.

use std::fs::OpenOptions;
use std::io::Write;
use std::path::PathBuf;
use std::sync::Mutex;

use chrono::Utc;
use serde::Serialize;

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
                    format!("{}...", &s[..200])
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
