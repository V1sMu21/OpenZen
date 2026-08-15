//! Session rollout — JSONL event stream recording and replay.
//!
//! Records the full agent session as append-only JSONL (one event per line),
//! mirroring Codex's rollout format. Enables deterministic replay for
//! debugging and E2E tests without a live LLM.

use std::io::Write;
use std::path::{Path, PathBuf};

use oz_core_types::StreamEvent;

/// One recorded rollout event.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct RolloutEvent {
    pub timestamp: String,
    #[serde(flatten)]
    pub event: StreamEvent,
}

/// Session meta recorded at rollout start.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct RolloutMeta {
    pub session_id: String,
    pub cwd: String,
    pub model: String,
    pub git_sha: Option<String>,
    pub git_branch: Option<String>,
}

/// Appends events to a session rollout file.
pub struct RolloutRecorder {
    path: PathBuf,
    file: std::io::BufWriter<std::fs::File>,
}

impl RolloutRecorder {
    pub fn create(dir: &Path, meta: &RolloutMeta) -> std::io::Result<Self> {
        std::fs::create_dir_all(dir)?;
        let ts = chrono::Utc::now().format("%Y%m%d_%H%M%S");
        let id8: String = meta.session_id.chars().take(8).collect();
        let path = dir.join(format!("rollout-{ts}-{id8}.jsonl"));
        let file = std::fs::File::create(&path)?;
        let mut recorder = RolloutRecorder {
            path,
            file: std::io::BufWriter::new(file),
        };
        recorder.write(&StreamEvent::StartStep {})?;
        Ok(recorder)
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn write(&mut self, event: &StreamEvent) -> std::io::Result<()> {
        let rec = RolloutEvent {
            timestamp: chrono::Utc::now().to_rfc3339(),
            event: event.clone(),
        };
        let line = serde_json::to_string(&rec).map_err(std::io::Error::other)?;
        self.file.write_all(line.as_bytes())?;
        self.file.write_all(b"\n")?;
        self.file.flush()
    }
}

impl Drop for RolloutRecorder {
    fn drop(&mut self) {
        let _ = self.file.flush();
    }
}

/// Reads back a rollout file as a list of events.
pub fn read_rollout(path: &Path) -> std::io::Result<Vec<RolloutEvent>> {
    let content = std::fs::read_to_string(path)?;
    Ok(content
        .lines()
        .filter(|l| !l.trim().is_empty())
        .filter_map(|l| serde_json::from_str::<RolloutEvent>(l).ok())
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_roundtrip() {
        let dir = tempfile::tempdir().unwrap();
        let mut recorder = RolloutRecorder::create(
            dir.path(),
            &RolloutMeta {
                session_id: "abc12345".into(),
                cwd: "/tmp".into(),
                model: "test".into(),
                git_sha: None,
                git_branch: None,
            },
        )
        .unwrap();
        recorder
            .write(&StreamEvent::TextStart { id: "t1".into() })
            .unwrap();
        recorder
            .write(&StreamEvent::TextDelta {
                id: "t1".into(),
                text: "hi".into(),
            })
            .unwrap();
        recorder
            .write(&StreamEvent::TextEnd { id: "t1".into() })
            .unwrap();

        let events = read_rollout(recorder.path()).unwrap();
        assert_eq!(events.len(), 4); // StartStep + 3
        assert!(matches!(events[1].event, StreamEvent::TextStart { .. }));
    }
}
