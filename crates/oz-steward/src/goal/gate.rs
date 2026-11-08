use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use serde::Serialize;
use tokio::process::Command;
use uuid::Uuid;

use super::spec::{GateResult, GoalGate};

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct GateOutcome {
    pub passed: bool,
    pub pending: bool,
    pub exit_code: Option<i32>,
    pub output: String,
    pub duration_ms: u64,
}

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct GateRunResult {
    pub gate: GoalGate,
    pub result: GateResult,
}

#[derive(Debug, Clone)]
pub struct GateRunner {
    workspace: PathBuf,
}

impl GateRunner {
    pub fn new(workspace: impl Into<PathBuf>) -> Self {
        Self {
            workspace: workspace.into(),
        }
    }

    pub async fn run_all(&self, gates: &[GoalGate]) -> Vec<GateRunResult> {
        let mut out = Vec::new();
        for (index, gate) in gates.iter().enumerate() {
            let outcome = self.run_gate(gate).await;
            out.push(GateRunResult {
                gate: gate.clone(),
                result: GateResult {
                    gate_index: index,
                    passed: outcome.passed,
                    pending: outcome.pending,
                    exit_code: outcome.exit_code,
                    output: left(&outcome.output, 4_000),
                    duration_ms: outcome.duration_ms,
                    evidence_id: format!("gate:{index}:{}", Uuid::new_v4()),
                },
            });
        }
        out
    }

    pub async fn run_gate(&self, gate: &GoalGate) -> GateOutcome {
        let started = Instant::now();
        match gate {
            GoalGate::Command {
                command,
                timeout_secs,
                retries,
            } => {
                let attempts = retries.saturating_add(1).min(5);
                let mut last = GateOutcome {
                    passed: false,
                    pending: false,
                    exit_code: None,
                    output: String::new(),
                    duration_ms: 0,
                };
                for _ in 0..attempts {
                    last = self.run_command(command, *timeout_secs).await;
                    if last.passed {
                        break;
                    }
                }
                last.duration_ms = started.elapsed().as_millis() as u64;
                last
            }
            GoalGate::FileAssertion {
                path,
                must_exist,
                contains,
            } => {
                let outcome = self.assert_file(path, *must_exist, contains.as_deref());
                let mut outcome = outcome;
                outcome.duration_ms = started.elapsed().as_millis() as u64;
                outcome
            }
            GoalGate::Human { prompt } => GateOutcome {
                passed: false,
                pending: true,
                exit_code: None,
                output: prompt.clone(),
                duration_ms: 0,
            },
            GoalGate::Review { rubric, .. } => GateOutcome {
                passed: false,
                pending: true,
                exit_code: None,
                output: format!("review required: {rubric}"),
                duration_ms: 0,
            },
            GoalGate::All { gates } => {
                let mut pending = false;
                let mut outputs = Vec::new();
                for child in gates {
                    let r = Box::pin(self.run_gate(child)).await;
                    outputs.push(r.output.clone());
                    if !r.passed && !r.pending {
                        return GateOutcome {
                            passed: false,
                            pending: false,
                            exit_code: r.exit_code,
                            output: outputs.join("\n"),
                            duration_ms: started.elapsed().as_millis() as u64,
                        };
                    }
                    if r.pending {
                        pending = true;
                    }
                }
                GateOutcome {
                    passed: !pending,
                    pending,
                    exit_code: Some(0),
                    output: outputs.join("\n"),
                    duration_ms: started.elapsed().as_millis() as u64,
                }
            }
            GoalGate::Any { gates } => {
                let mut pending = false;
                let mut outputs = Vec::new();
                for child in gates {
                    let r = Box::pin(self.run_gate(child)).await;
                    outputs.push(r.output.clone());
                    if r.passed {
                        return GateOutcome {
                            passed: true,
                            pending: false,
                            exit_code: r.exit_code,
                            output: outputs.join("\n"),
                            duration_ms: started.elapsed().as_millis() as u64,
                        };
                    }
                    if r.pending {
                        pending = true;
                    }
                }
                GateOutcome {
                    passed: false,
                    pending,
                    exit_code: Some(1),
                    output: outputs.join("\n"),
                    duration_ms: started.elapsed().as_millis() as u64,
                }
            }
        }
    }

    async fn run_command(&self, command: &str, timeout_secs: u64) -> GateOutcome {
        let mut cmd = Command::new("sh");
        cmd.arg("-c")
            .arg(command)
            .current_dir(&self.workspace)
            .kill_on_drop(true);
        let started = Instant::now();
        match tokio::time::timeout(Duration::from_secs(timeout_secs.max(1)), cmd.output()).await {
            Ok(Ok(output)) => GateOutcome {
                passed: output.status.success(),
                pending: false,
                exit_code: output.status.code(),
                output: format!(
                    "{}{}",
                    String::from_utf8_lossy(&output.stdout),
                    String::from_utf8_lossy(&output.stderr)
                ),
                duration_ms: started.elapsed().as_millis() as u64,
            },
            Ok(Err(e)) => GateOutcome {
                passed: false,
                pending: false,
                exit_code: None,
                output: format!("spawn failed: {e}"),
                duration_ms: started.elapsed().as_millis() as u64,
            },
            Err(_) => GateOutcome {
                passed: false,
                pending: false,
                exit_code: None,
                output: format!("timeout after {timeout_secs}s"),
                duration_ms: started.elapsed().as_millis() as u64,
            },
        }
    }

    fn assert_file(&self, path: &str, must_exist: bool, contains: Option<&str>) -> GateOutcome {
        let Some(rel) = safe_relative(path) else {
            return GateOutcome {
                passed: false,
                pending: false,
                exit_code: None,
                output: format!("path escapes workspace: {path}"),
                duration_ms: 0,
            };
        };
        let target = self.workspace.join(rel);
        let exists = target.exists();
        if must_exist != exists {
            return GateOutcome {
                passed: false,
                pending: false,
                exit_code: None,
                output: if must_exist {
                    format!("missing file: {path}")
                } else {
                    format!("file should not exist: {path}")
                },
                duration_ms: 0,
            };
        }
        if let (Some(needle), true) = (contains, exists) {
            match std::fs::read_to_string(&target) {
                Ok(text) if text.contains(needle) => GateOutcome {
                    passed: true,
                    pending: false,
                    exit_code: Some(0),
                    output: format!("{path} contains expected text"),
                    duration_ms: 0,
                },
                Ok(_) => GateOutcome {
                    passed: false,
                    pending: false,
                    exit_code: Some(1),
                    output: format!("{path} does not contain expected text"),
                    duration_ms: 0,
                },
                Err(e) => GateOutcome {
                    passed: false,
                    pending: false,
                    exit_code: None,
                    output: format!("cannot read {path}: {e}"),
                    duration_ms: 0,
                },
            }
        } else {
            GateOutcome {
                passed: true,
                pending: false,
                exit_code: Some(0),
                output: format!("file assertion passed: {path}"),
                duration_ms: 0,
            }
        }
    }
}

fn safe_relative(raw: &str) -> Option<PathBuf> {
    let path = Path::new(raw);
    if path.is_absolute() {
        return None;
    }
    let mut out = PathBuf::new();
    for c in path.components() {
        match c {
            std::path::Component::Normal(p) => out.push(p),
            std::path::Component::CurDir => {}
            _ => return None,
        }
    }
    (!out.as_os_str().is_empty()).then_some(out)
}

fn left(s: &str, max: usize) -> String {
    s.chars().take(max).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[tokio::test]
    async fn command_gate_pass_and_fail() {
        let dir = tempdir().unwrap();
        let runner = GateRunner::new(dir.path());
        let pass = runner
            .run_gate(&GoalGate::Command {
                command: "echo ok".into(),
                timeout_secs: 5,
                retries: 0,
            })
            .await;
        assert!(pass.passed);
        let fail = runner
            .run_gate(&GoalGate::Command {
                command: "false".into(),
                timeout_secs: 5,
                retries: 0,
            })
            .await;
        assert!(!fail.passed);
    }

    #[tokio::test]
    async fn file_assertion_and_human_pending() {
        let dir = tempdir().unwrap();
        std::fs::write(dir.path().join("out.txt"), "hello").unwrap();
        let runner = GateRunner::new(dir.path());
        let ok = runner
            .run_gate(&GoalGate::FileAssertion {
                path: "out.txt".into(),
                must_exist: true,
                contains: Some("ell".into()),
            })
            .await;
        assert!(ok.passed);
        let human = runner
            .run_gate(&GoalGate::Human {
                prompt: "approve".into(),
            })
            .await;
        assert!(human.pending);
        assert!(!human.passed);
    }

    #[tokio::test]
    async fn all_and_any() {
        let dir = tempdir().unwrap();
        let runner = GateRunner::new(dir.path());
        let all = runner
            .run_gate(&GoalGate::All {
                gates: vec![
                    GoalGate::Command {
                        command: "true".into(),
                        timeout_secs: 5,
                        retries: 0,
                    },
                    GoalGate::Command {
                        command: "true".into(),
                        timeout_secs: 5,
                        retries: 0,
                    },
                ],
            })
            .await;
        assert!(all.passed);
        let any = runner
            .run_gate(&GoalGate::Any {
                gates: vec![
                    GoalGate::Command {
                        command: "false".into(),
                        timeout_secs: 5,
                        retries: 0,
                    },
                    GoalGate::Command {
                        command: "true".into(),
                        timeout_secs: 5,
                        retries: 0,
                    },
                ],
            })
            .await;
        assert!(any.passed);
    }
}
