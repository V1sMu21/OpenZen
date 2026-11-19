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
            GoalGate::StrongCommand {
                command,
                timeout_secs,
            } => {
                if !crate::butler::spec::verify_command_shape_is_safe(command) {
                    return GateOutcome {
                        passed: false,
                        pending: true,
                        exit_code: None,
                        output: format!(
                            "verify command must be a single command (shell operators/redirects are refused): {command}"
                        ),
                        duration_ms: started.elapsed().as_millis() as u64,
                    };
                }
                if !crate::butler::spec::command_is_known_test_runner(command)
                    && !crate::butler::spec::custom_verify_commands_allowed()
                {
                    return GateOutcome {
                        passed: false,
                        pending: true,
                        exit_code: None,
                        output: format!(
                            "custom verify command requires approval (set OPENZEN_ALLOW_CUSTOM_VERIFY=1 or use a known test runner): {command}"
                        ),
                        duration_ms: started.elapsed().as_millis() as u64,
                    };
                }
                let mut out = self.run_command(command, *timeout_secs).await;
                out.duration_ms = started.elapsed().as_millis() as u64;
                out
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
            GoalGate::FileContent {
                path,
                contains_all,
                not_contains,
                min_bytes,
            } => {
                let mut outcome =
                    self.assert_file_content(path, contains_all, not_contains, *min_bytes);
                outcome.duration_ms = started.elapsed().as_millis() as u64;
                outcome
            }
            GoalGate::SourceCheck { path, min_sources } => {
                let mut outcome = self.assert_sources(path, *min_sources);
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

    /// Research evidence: every cited local path must exist. This is stronger
    /// than "the document mentions X" because it independently stats the
    /// filesystem; it is what lets a research task be `verified` without a
    /// build/test command.
    fn assert_sources(&self, path: &str, min_sources: usize) -> GateOutcome {
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
        let text = match std::fs::read_to_string(&target) {
            Ok(t) => t,
            Err(e) => {
                return GateOutcome {
                    passed: false,
                    pending: false,
                    exit_code: None,
                    output: format!("cannot read {path}: {e}"),
                    duration_ms: 0,
                }
            }
        };
        let self_path = target.canonicalize().ok();
        let mut verified: Vec<PathBuf> = Vec::new();
        let mut missing: Vec<String> = Vec::new();
        let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();
        for raw in cited_paths(&text) {
            if !seen.insert(raw.clone()) {
                continue;
            }
            let candidate = if let Some(rest) = raw.strip_prefix("~/") {
                std::env::var_os("HOME").map(|h| PathBuf::from(h).join(rest))
            } else if Path::new(&raw).is_absolute() {
                Some(PathBuf::from(&raw))
            } else {
                Some(self.workspace.join(&raw))
            };
            match candidate {
                Some(p) if p.is_file() => {
                    if let Ok(canon) = p.canonicalize() {
                        let s = canon.to_string_lossy();
                        let system = ["/etc/", "/usr/", "/bin/", "/sbin/", "/System/", "/Library/"]
                            .iter()
                            .any(|root| s.starts_with(root));
                        if !system
                            && Some(&canon) != self_path.as_ref()
                            && !verified.contains(&canon)
                        {
                            verified.push(canon);
                        }
                    }
                }
                Some(_) => missing.push(raw),
                None => missing.push(raw),
            }
        }
        let min = min_sources.max(1);
        if verified.len() < min {
            let mut out = format!(
                "{path}: only {} verified source(s), need at least {min}",
                verified.len()
            );
            if !missing.is_empty() {
                out.push_str("; missing: ");
                out.push_str(
                    &missing
                        .iter()
                        .take(5)
                        .cloned()
                        .collect::<Vec<_>>()
                        .join(", "),
                );
            }
            return GateOutcome {
                passed: false,
                pending: false,
                exit_code: Some(1),
                output: out,
                duration_ms: 0,
            };
        }
        GateOutcome {
            passed: true,
            pending: false,
            exit_code: Some(0),
            output: format!(
                "{path}: {} unique local source(s) verified (min {min})",
                verified.len()
            ),
            duration_ms: 0,
        }
    }

    fn assert_file_content(
        &self,
        path: &str,
        contains_all: &[String],
        not_contains: &[String],
        min_bytes: Option<u64>,
    ) -> GateOutcome {
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
        let text = match std::fs::read(&target) {
            Ok(bytes) => {
                if let Some(min) = min_bytes {
                    if (bytes.len() as u64) < min {
                        return GateOutcome {
                            passed: false,
                            pending: false,
                            exit_code: Some(1),
                            output: format!(
                                "{path} is {} bytes, expected at least {min}",
                                bytes.len()
                            ),
                            duration_ms: 0,
                        };
                    }
                }
                String::from_utf8_lossy(&bytes).to_string()
            }
            Err(e) => {
                return GateOutcome {
                    passed: false,
                    pending: false,
                    exit_code: None,
                    output: format!("cannot read {path}: {e}"),
                    duration_ms: 0,
                }
            }
        };
        if let Some(missing) = contains_all
            .iter()
            .find(|needle| !text.contains(needle.as_str()))
        {
            return GateOutcome {
                passed: false,
                pending: false,
                exit_code: Some(1),
                output: format!("{path} does not contain required text: {missing}"),
                duration_ms: 0,
            };
        }
        if let Some(found) = not_contains
            .iter()
            .find(|needle| text.contains(needle.as_str()))
        {
            return GateOutcome {
                passed: false,
                pending: false,
                exit_code: Some(1),
                output: format!("{path} still contains forbidden text: {found}"),
                duration_ms: 0,
            };
        }
        GateOutcome {
            passed: true,
            pending: false,
            exit_code: Some(0),
            output: format!("{path} meets structural assertions"),
            duration_ms: 0,
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

/// Extract local paths cited in a Markdown/plain document: backticked
/// spans, Markdown link targets, and bare absolute/workspace paths.
fn cited_paths(text: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut push = |raw: &str| {
        let mut t = raw
            .trim()
            .trim_matches([
                '`', '"', '\'', '(', ')', '[', ']', '<', '>', ',', '.', ';', ':', '，', '。', '；',
                '：',
            ])
            .to_string();
        // Citations often carry `path:166` or `path#L12`; the file is the
        // source, the line suffix is not part of its name.
        if let Some((base, line)) = t.rsplit_once(':') {
            if !line.is_empty() && line.chars().all(|c| c.is_ascii_digit()) {
                t = base.to_string();
            }
        }
        if let Some((base, _)) = t.split_once("#L") {
            t = base.to_string();
        }
        if t.is_empty() || t.starts_with("http://") || t.starts_with("https://") {
            return;
        }
        if t.starts_with('/')
            || t.starts_with("~/")
            || t.starts_with("temp/")
            || t.starts_with("docs/")
            || t.starts_with("projects/")
            || t.starts_with("output/")
            || t.starts_with("data/")
            || t.starts_with("artifacts/")
            || t.starts_with("reports/")
            || t.starts_with("src/")
            || t.starts_with("tests/")
            || (t.contains('.') && !t.contains(' '))
            || (t.contains('/') && !t.contains(' '))
        {
            out.push(t);
        }
    };
    let mut in_tick = false;
    for part in text.split('`') {
        if in_tick {
            push(part.trim());
        } else {
            let mut rest = part;
            while let Some(i) = rest.find("](") {
                let tail = &rest[i + 2..];
                let end = tail.find(')').unwrap_or(tail.len());
                push(&tail[..end]);
                rest = &tail[end..];
            }
            for token in part.split_whitespace() {
                if token.starts_with('/') || token.starts_with("~/") {
                    push(token);
                }
            }
        }
        in_tick = !in_tick;
    }
    out
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

    #[tokio::test]
    async fn source_check_verifies_existing_paths_and_rejects_missing_ones() {
        let dir = tempfile::tempdir().unwrap();
        let a = dir.path().join("a.txt");
        let b = dir.path().join("b.txt");
        let c = dir.path().join("c.txt");
        for p in [&a, &b, &c] {
            std::fs::write(p, "x").unwrap();
        }
        let doc = dir.path().join("sources.md");
        std::fs::write(
            &doc,
            format!(
                "sources: `{}`, `{}`, `{}`",
                a.display(),
                b.display(),
                c.display()
            ),
        )
        .unwrap();
        let runner = GateRunner::new(dir.path());
        let ok = runner
            .run_gate(&GoalGate::SourceCheck {
                path: "sources.md".into(),
                min_sources: 3,
            })
            .await;
        assert!(ok.passed, "{}", ok.output);
        assert!(ok.output.contains("3 unique"));

        std::fs::write(
            &doc,
            format!("only `{}` and `/definitely/not/here`", a.display()),
        )
        .unwrap();
        let bad = runner
            .run_gate(&GoalGate::SourceCheck {
                path: "sources.md".into(),
                min_sources: 3,
            })
            .await;
        assert!(!bad.passed);
        assert!(bad.output.contains("need at least 3"));

        std::fs::write(&doc, "`/etc/hosts` `/usr/bin/true` `sources.md`").unwrap();
        let gaming = runner
            .run_gate(&GoalGate::SourceCheck {
                path: "sources.md".into(),
                min_sources: 3,
            })
            .await;
        assert!(
            !gaming.passed,
            "system paths and the report itself must not count: {}",
            gaming.output
        );
    }
}
