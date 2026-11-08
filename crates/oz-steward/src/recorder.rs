//! Flight recorder: maps the runtime's `StreamEvent`s to immutable steps.
//!
//! This is intentionally an event *consumer*, not a hook inside
//! `agent_loop`. The loop already emits a typed stream at every tool
//! boundary; subscribing to it keeps the core loop unchanged and makes the
//! recorder testable with synthetic events.

use std::collections::HashMap;
use std::sync::Arc;

use oz_core_types::supervision::{
    now_ms, Phase, StepRecord, StepStatus, TicketStatus, ToolRecord, Verdict,
};
use oz_core_types::StreamEvent;
use sha2::{Digest, Sha256};
use uuid::Uuid;

use crate::error::Result;
use crate::rollback::FileJournal;
use crate::store::Store;

struct PendingTool {
    name: String,
    args_hash: String,
    targets: Vec<String>,
    started_ms: i64,
}

pub struct Recorder {
    store: Arc<Store>,
    ticket_id: String,
    session_id: String,
    turn: u32,
    phase: Phase,
    pending: HashMap<String, PendingTool>,
    artifacts: Vec<String>,
    tokens_in: u64,
    tokens_out: u64,
    journal: Option<Arc<std::sync::Mutex<FileJournal>>>,
}

impl Recorder {
    pub fn new(
        store: Arc<Store>,
        ticket_id: impl Into<String>,
        session_id: impl Into<String>,
    ) -> Self {
        Self {
            store,
            ticket_id: ticket_id.into(),
            session_id: session_id.into(),
            turn: 0,
            phase: Phase::Planning,
            pending: HashMap::new(),
            artifacts: Vec::new(),
            tokens_in: 0,
            tokens_out: 0,
            journal: None,
        }
    }

    /// Attach the workspace journal so write/edit/patch targets are backed up
    /// when their arguments become available, before the tool executes.
    pub fn with_journal(mut self, journal: Arc<std::sync::Mutex<FileJournal>>) -> Self {
        self.journal = Some(journal);
        self
    }

    pub fn ticket_id(&self) -> &str {
        &self.ticket_id
    }

    pub fn turn(&self) -> u32 {
        self.turn
    }

    pub fn store_heartbeat(&self) -> Result<()> {
        self.store.heartbeat(&self.ticket_id, "session_tick")
    }

    /// Consume one event. Store failures are returned so the caller can log
    /// them; they must never abort the agent run.
    pub fn on_event(&mut self, event: &StreamEvent) -> Result<()> {
        match event {
            StreamEvent::StartStep {} => {
                self.turn = self.turn.saturating_add(1);
                self.phase = Phase::Acting;
            }
            StreamEvent::ToolInputStart { tool_call_id, name } => {
                self.pending.insert(
                    tool_call_id.clone(),
                    PendingTool {
                        name: name.clone(),
                        args_hash: String::new(),
                        targets: Vec::new(),
                        started_ms: now_ms(),
                    },
                );
            }
            StreamEvent::ToolInputAvailable {
                tool_call_id,
                name,
                args,
            } => {
                let entry = self
                    .pending
                    .entry(tool_call_id.clone())
                    .or_insert(PendingTool {
                        name: name.clone(),
                        args_hash: String::new(),
                        targets: Vec::new(),
                        started_ms: now_ms(),
                    });
                entry.name = name.clone();
                entry.args_hash = hash_str(args);
                entry.targets = extract_targets(args);
                if let Some(journal) = &self.journal {
                    if let Ok(mut journal) = journal.lock() {
                        let _ = journal.snapshot_for(&self.ticket_id, &entry.targets);
                    }
                }
            }
            StreamEvent::ToolOutputAvailable {
                tool_call_id,
                name,
                output,
            } => {
                let pending = self.pending.remove(tool_call_id).unwrap_or(PendingTool {
                    name: name.clone(),
                    args_hash: String::new(),
                    targets: Vec::new(),
                    started_ms: now_ms(),
                });
                let tool = ToolRecord {
                    name: pending.name,
                    args_hash: pending.args_hash,
                    output_hash: Some(hash_str(output)),
                    duration_ms: Some((now_ms() - pending.started_ms).max(0) as u64),
                    status: status_from_output(output),
                };
                let step = StepRecord {
                    id: Uuid::new_v4().to_string(),
                    ticket_id: self.ticket_id.clone(),
                    session_id: self.session_id.clone(),
                    turn: self.turn,
                    phase: self.phase,
                    at: now_ms(),
                    status: tool.status,
                    tool: Some(tool),
                    artifacts: std::mem::take(&mut self.artifacts),
                    targets: pending.targets,
                    plan_hash: None,
                    git_sha: None,
                    tokens_in: self.tokens_in,
                    tokens_out: self.tokens_out,
                };
                self.store.record_step(&step)?;
            }
            StreamEvent::ToolProgress { .. } => {
                self.store.heartbeat(&self.ticket_id, "tool_progress")?;
            }
            StreamEvent::OpenArtifact { artifact_path, .. } => {
                if !artifact_path.is_empty() {
                    self.artifacts.push(artifact_path.clone());
                }
            }
            StreamEvent::DataQualityGate { .. } => {
                self.phase = Phase::Verifying;
                self.store.set_phase(&self.ticket_id, Phase::Verifying)?;
            }
            StreamEvent::DataContextUsage {
                current_tokens,
                output_tokens,
                ..
            } => {
                self.tokens_in = *current_tokens;
                self.tokens_out = *output_tokens;
            }
            StreamEvent::FinishMessage { .. } => {
                self.phase = Phase::Done;
                // A run that ended without a verified gate is completed, not
                // achieved. Report/gate layers may promote it later.
                self.store.set_status(
                    &self.ticket_id,
                    TicketStatus::Completed,
                    Phase::Done,
                    None,
                )?;
                self.store.heartbeat(&self.ticket_id, "finish")?;
            }
            StreamEvent::Error { .. } => {
                // An LLM/tool error is recoverable: park the ticket as
                // blocked rather than writing a terminal failed state with no
                // report, so the user can retry.
                self.store.set_status(
                    &self.ticket_id,
                    TicketStatus::Blocked,
                    self.phase,
                    Some(Verdict::Blocked),
                )?;
            }
            _ => {}
        }
        Ok(())
    }

    /// Explicit terminal transition used by the report layer.
    pub fn finish(&mut self, status: TicketStatus) -> Result<()> {
        self.phase = Phase::Done;
        self.store
            .set_status(&self.ticket_id, status, Phase::Done, None)
    }
}

fn extract_targets(args: &str) -> Vec<String> {
    let Ok(value) = serde_json::from_str::<serde_json::Value>(args) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for key in ["file_path", "path", "target", "artifact_path"] {
        if let Some(s) = value.get(key).and_then(|v| v.as_str()) {
            if !s.is_empty() {
                out.push(s.to_string());
            }
        }
        if let Some(arr) = value.get(key).and_then(|v| v.as_array()) {
            for item in arr {
                if let Some(s) = item.as_str() {
                    if !s.is_empty() {
                        out.push(s.to_string());
                    }
                }
            }
        }
    }
    out
}

fn hash_str(s: &str) -> String {
    let digest = Sha256::digest(s.as_bytes());
    format!("{digest:x}")
}

fn status_from_output(output: &str) -> StepStatus {
    if let Ok(value) = serde_json::from_str::<serde_json::Value>(output) {
        if value.get("status").and_then(|v| v.as_str()) == Some("error") {
            return StepStatus::Failed;
        }
        if value.get("ok").and_then(|v| v.as_bool()) == Some(false) {
            return StepStatus::Failed;
        }
        if value.get("error").is_some() {
            return StepStatus::Failed;
        }
    }
    let trimmed = output.trim_start();
    if trimmed.starts_with("Error")
        || trimmed.starts_with("error:")
        || trimmed.starts_with("ToolError")
    {
        StepStatus::Failed
    } else {
        StepStatus::Succeeded
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use oz_core_types::supervision::TicketStatus;

    fn recorder() -> (Arc<Store>, Recorder) {
        let store = Arc::new(Store::open_memory().unwrap());
        store.ensure_ticket("t1", "sess", "title").unwrap();
        let rec = Recorder::new(store.clone(), "t1", "sess");
        (store, rec)
    }

    #[test]
    fn tool_lifecycle_becomes_one_step() {
        let (store, mut rec) = recorder();
        rec.on_event(&StreamEvent::StartStep {}).unwrap();
        rec.on_event(&StreamEvent::ToolInputStart {
            tool_call_id: "c1".into(),
            name: "write".into(),
        })
        .unwrap();
        rec.on_event(&StreamEvent::ToolInputAvailable {
            tool_call_id: "c1".into(),
            name: "write".into(),
            args: r#"{"file_path":"a"}"#.into(),
        })
        .unwrap();
        rec.on_event(&StreamEvent::ToolOutputAvailable {
            tool_call_id: "c1".into(),
            name: "write".into(),
            output: r#"{"status":"ok"}"#.into(),
        })
        .unwrap();
        let steps = store.steps("t1", 10).unwrap();
        assert_eq!(steps.len(), 1);
        let tool = steps[0].tool.as_ref().unwrap();
        assert_eq!(tool.name, "write");
        assert_eq!(tool.status, StepStatus::Succeeded);
        assert_eq!(tool.args_hash.len(), 64);
        assert_eq!(rec.turn(), 1);
    }

    #[test]
    fn error_output_is_failed() {
        let (store, mut rec) = recorder();
        rec.on_event(&StreamEvent::ToolOutputAvailable {
            tool_call_id: "c1".into(),
            name: "code_run".into(),
            output: r#"{"status":"error","error":"boom"}"#.into(),
        })
        .unwrap();
        let steps = store.steps("t1", 10).unwrap();
        assert_eq!(steps[0].status, StepStatus::Failed);
    }

    #[test]
    fn quality_gate_moves_to_verifying_and_error_blocks() {
        let (store, mut rec) = recorder();
        rec.on_event(&StreamEvent::DataQualityGate {
            stage: "assertions".into(),
        })
        .unwrap();
        assert_eq!(
            store.snapshot("t1").unwrap().unwrap().phase,
            Phase::Verifying
        );
        rec.on_event(&StreamEvent::Error {
            message: "gateway down".into(),
        })
        .unwrap();
        assert_eq!(
            store.snapshot("t1").unwrap().unwrap().status,
            TicketStatus::Blocked
        );
    }

    #[test]
    fn finish_message_marks_done_not_achieved() {
        let (store, mut rec) = recorder();
        rec.on_event(&StreamEvent::FinishMessage {
            stop_reason: "end_turn".into(),
        })
        .unwrap();
        let snap = store.snapshot("t1").unwrap().unwrap();
        assert_eq!(snap.phase, Phase::Done);
        assert_eq!(snap.status, TicketStatus::Completed);
    }

    #[test]
    fn artifacts_attach_to_next_tool_step() {
        let (store, mut rec) = recorder();
        rec.on_event(&StreamEvent::OpenArtifact {
            artifact_type: "file".into(),
            artifact_path: "src/a.rs".into(),
            artifact_label: "a".into(),
        })
        .unwrap();
        rec.on_event(&StreamEvent::ToolOutputAvailable {
            tool_call_id: "c1".into(),
            name: "write".into(),
            output: "ok".into(),
        })
        .unwrap();
        let steps = store.steps("t1", 10).unwrap();
        assert_eq!(steps[0].artifacts, vec!["src/a.rs".to_string()]);
    }
}
