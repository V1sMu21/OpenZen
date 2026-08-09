//! Goal-level workflow: self-review loop over turn-level agent runs (P2-9).
//!
//! The turn loop (`run_agent_loop`) executes one instruction and ends. This
//! module adds the "define → execute → verify → iterate" closure on top:
//! run the agent toward a high-level goal, then run a verification command
//! (e.g. `cargo test`); on failure the output is fed back into the next
//! attempt, until the goal passes or the attempt budget is exhausted.
//!
//! Compose-style multi-stage specs and concurrent goals are future work.

use std::path::Path;
use std::process::Stdio;
use std::time::Duration;

/// Verification step for a goal: a shell command whose exit code 0 means pass.
/// `retries` bounds in-round retries of THIS gate (Prime autonomous-gate-retries).
#[derive(Debug, Clone, Default)]
pub struct VerifySpec {
    pub command: String,
    pub timeout_secs: u64,
    pub retries: u32,
}

impl VerifySpec {
    pub fn new(command: impl Into<String>) -> Self {
        VerifySpec { command: command.into(), timeout_secs: 60, retries: 3 }
    }
}

/// A high-level goal with its verification gates and attempt budget.
#[derive(Debug, Clone, Default)]
pub struct GoalSpec {
    pub goal: String,
    /// Gates run in order; ALL must pass for the round to succeed.
    pub gates: Vec<VerifySpec>,
    /// Round budget — a failed round (after gate retries) counts once.
    pub max_attempts: u32,
}

/// Outcome of a goal loop.
#[derive(Debug, Clone)]
pub struct GoalOutcome {
    pub attempts: u32,
    pub success: bool,
    pub last_verify_output: String,
    /// Per-gate (command, passed) in execution order, for logs/UI.
    pub per_gate: Vec<(String, bool)>,
}

/// Run the verification command in `working_dir`. Returns (passed, output).
pub async fn run_verify(
    spec: &VerifySpec,
    working_dir: &str,
) -> (bool, String) {
    let dir = Path::new(working_dir);
    let out = match tokio::time::timeout(
        Duration::from_secs(spec.timeout_secs.max(1)),
        tokio::process::Command::new("sh")
            .arg("-c")
            .arg(&spec.command)
            .current_dir(dir)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .output(),
    )
    .await
    {
        Ok(Ok(out)) => out,
        _ => return (false, String::from("<verify timed out or failed to spawn>")),
    };
    let mut buf = out.stdout;
    buf.extend_from_slice(&out.stderr);
    let text = String::from_utf8_lossy(&buf).trim().to_string();
    (out.status.success(), text)
}

/// Iterate `run_agent_loop` toward `spec.goal`, verifying after each attempt.
///
/// Each failed attempt feeds the verify output back into the next attempt's
/// user message. Stops on first pass or when `max_attempts` is reached.
#[allow(clippy::too_many_arguments)]
pub async fn run_goal_loop<C>(
    client: &mut C,
    system_prompt: String,
    handler: &mut dyn crate::handler::Handler,
    tools: &[oz_core_types::ToolDefinition],
    ctx: &oz_core_types::ToolContext,
    config: &crate::handler::LoopConfig,
    stop_signal: &std::sync::atomic::AtomicBool,
    spec: &GoalSpec,
) -> GoalOutcome
where
    C: oz_core_types::LlmClient,
{
    let mut attempts: u32 = 0;
    let mut feedback = String::new();
    loop {
        attempts += 1;
        let mut user_input = format!("[Goal] {}\n", spec.goal);
        if !feedback.is_empty() {
            user_input.push_str("\n[Previous attempt failed — verification output]\n");
            user_input.push_str(&feedback);
            user_input.push_str("\nFix the issues and try again.\n");
        }
        let _outcome = crate::agent_loop::run_agent_loop(
            client,
            system_prompt.clone(),
            user_input,
            Vec::new(),
            handler,
            tools,
            ctx,
            config,
            stop_signal,
        )
        .await;

        if spec.gates.is_empty() {
            return GoalOutcome { attempts, success: true, last_verify_output: String::new(), per_gate: Vec::new() };
        }

        // Run gates in order; a gate may retry within the round (retries), and
        // the round fails at the first gate that exhausts its retries.
        let mut per_gate: Vec<(String, bool)> = Vec::with_capacity(spec.gates.len());
        let mut all_passed = true;
        let mut last_output = String::new();
        for gate in &spec.gates {
            let mut passed = false;
            for _ in 0..=gate.retries {
                let (p, output) = run_verify(gate, &config.working_dir).await;
                last_output = output;
                if p {
                    passed = true;
                    break;
                }
            }
            per_gate.push((gate.command.clone(), passed));
            if !passed {
                all_passed = false;
                break;
            }
        }

        if all_passed {
            return GoalOutcome { attempts, success: true, last_verify_output: last_output, per_gate };
        }
        feedback = truncate(&last_output, 4000);
        if attempts >= spec.max_attempts.max(1) {
            return GoalOutcome { attempts, success: false, last_verify_output: last_output, per_gate };
        }
    }
}

fn truncate(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        s.to_string()
    } else {
        let mut out: String = s.chars().take(max).collect();
        out.push_str("\n… [truncated]");
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_run_verify_pass_and_fail() {
        let spec = VerifySpec::new("test -f /etc/hosts");
        let (passed, _out) = run_verify(&spec, "/tmp").await;
        assert!(passed);

        let spec = VerifySpec::new("test -f /nonexistent-xyz");
        let (passed, _out) = run_verify(&spec, "/tmp").await;
        assert!(!passed);
    }

    #[tokio::test]
    async fn test_run_verify_timeout() {
        let mut spec = VerifySpec::new("sleep 30");
        spec.timeout_secs = 1;
        let (passed, out) = run_verify(&spec, "/tmp").await;
        assert!(!passed);
        assert!(out.contains("timed out"));
    }

    #[test]
    fn test_truncate() {
        assert_eq!(truncate("abc", 10), "abc");
        let t = truncate("abcdefghij", 5);
        assert!(t.starts_with("abcde"));
        assert!(t.contains("truncated"));
    }

    // ── Goal loop end-to-end ──

    struct MockLlm {
        calls: usize,
    }

    #[async_trait::async_trait]
    impl oz_core_types::LlmClient for MockLlm {
        async fn chat(
            &mut self,
            _messages: &[oz_core_types::Message],
            _tools: &[oz_core_types::ToolDefinition],
        ) -> Result<oz_core_types::MockResponse, oz_core_types::LlmError> {
            self.calls += 1;
            Ok(oz_core_types::MockResponse::new("done"))
        }
    }

    struct StubHandler {
        working: crate::WorkingMemory,
    }

    impl StubHandler {
        fn new() -> Self {
            StubHandler { working: crate::WorkingMemory::default() }
        }
    }

    #[async_trait::async_trait]
    impl crate::handler::Handler for StubHandler {
        fn working(&self) -> &crate::WorkingMemory {
            &self.working
        }
        fn working_mut(&mut self) -> &mut crate::WorkingMemory {
            &mut self.working
        }
        fn turn_end(
            &mut self,
            _response: &oz_core_types::MockResponse,
            _tool_calls: &[oz_core_types::MockToolCall],
            _tool_results: &[oz_core_types::ToolResultItem],
            _turn: u32,
            _next_prompt: String,
            _exit_reason: Option<String>,
        ) -> String {
            String::new()
        }
        async fn dispatch(
            &self,
            _tool_name: &str,
            _args: serde_json::Value,
            _response: &oz_core_types::MockResponse,
            _index: u32,
            _ctx: &oz_core_types::ToolContext,
        ) -> Result<oz_core_types::StepOutcome, oz_core_types::ToolError> {
            Ok(oz_core_types::StepOutcome::success(serde_json::json!({})))
        }
    }

    #[tokio::test]
    async fn test_goal_loop_retries_until_verify_passes() {
        let dir = std::env::temp_dir().join("oz-goal-counter");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let counter = dir.join("counter");
        // Counter starts at 0: fails while < 2. With in-round retries (default
        // 3), the FIRST round passes once the counter reaches 2 — attempts == 1.
        let verify = VerifySpec::new(&format!(
            "n=$(cat {} 2>/dev/null || echo 0); n=$((n+1)); echo $n > {}; test $n -ge 2",
            counter.display(),
            counter.display()
        ));

        let mut llm = MockLlm { calls: 0 };
        let mut handler = StubHandler::new();
        let config = crate::handler::LoopConfig {
            working_dir: dir.to_string_lossy().to_string(),
            ..Default::default()
        };
        let ctx = oz_core_types::ToolContext::default();
        let stop = std::sync::atomic::AtomicBool::new(false);
        let spec = GoalSpec {
            goal: "make the counter reach 2".into(),
            gates: vec![verify],
            max_attempts: 3,
        };

        let outcome = run_goal_loop(
            &mut llm,
            String::from("system"),
            &mut handler,
            &[],
            &ctx,
            &config,
            &stop,
            &spec,
        )
        .await;
        assert!(outcome.success, "in-round gate retry must pass");
        assert_eq!(outcome.attempts, 1);
        assert_eq!(outcome.per_gate.len(), 1);
        assert!(outcome.per_gate[0].1, "gate must be marked passed");
    }

    #[tokio::test]
    async fn test_goal_loop_gives_up_after_max_attempts() {
        let dir = std::env::temp_dir().join("oz-goal-fail");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let mut verify = VerifySpec::new("exit 1");
        verify.retries = 0; // no in-round retry: exercise the round budget

        let mut llm = MockLlm { calls: 0 };
        let mut handler = StubHandler::new();
        let config = crate::handler::LoopConfig {
            working_dir: dir.to_string_lossy().to_string(),
            ..Default::default()
        };
        let ctx = oz_core_types::ToolContext::default();
        let stop = std::sync::atomic::AtomicBool::new(false);
        let spec = GoalSpec {
            goal: "impossible".into(),
            gates: vec![verify],
            max_attempts: 2,
        };

        let outcome = run_goal_loop(
            &mut llm,
            String::from("system"),
            &mut handler,
            &[],
            &ctx,
            &config,
            &stop,
            &spec,
        )
        .await;
        assert!(!outcome.success);
        assert_eq!(outcome.attempts, 2);
        assert_eq!(outcome.per_gate.len(), 1);
        assert!(!outcome.per_gate[0].1);
    }

    #[tokio::test]
    async fn test_goal_loop_multiple_gates_short_circuit() {
        let dir = std::env::temp_dir().join("oz-goal-gates");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        // Gate 1 passes, gate 2 always fails → round fails at gate 2, and
        // gate 3 must NOT run (short-circuit).
        let mut fail_gate = VerifySpec::new("exit 1");
        fail_gate.retries = 0;
        let spec = GoalSpec {
            goal: "multi-gate".into(),
            gates: vec![VerifySpec::new("true"), fail_gate, VerifySpec::new("true")],
            max_attempts: 1,
        };

        let mut llm = MockLlm { calls: 0 };
        let mut handler = StubHandler::new();
        let config = crate::handler::LoopConfig {
            working_dir: dir.to_string_lossy().to_string(),
            ..Default::default()
        };
        let ctx = oz_core_types::ToolContext::default();
        let stop = std::sync::atomic::AtomicBool::new(false);
        let outcome = run_goal_loop(
            &mut llm,
            String::from("system"),
            &mut handler,
            &[],
            &ctx,
            &config,
            &stop,
            &spec,
        )
        .await;
        assert!(!outcome.success);
        assert_eq!(outcome.per_gate.len(), 2, "gate 3 must be short-circuited");
        assert!(outcome.per_gate[0].1);
        assert!(!outcome.per_gate[1].1);
    }

    #[tokio::test]
    async fn test_goal_loop_no_gates_passes_immediately() {
        let dir = std::env::temp_dir().join("oz-goal-no-gates");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let spec = GoalSpec {
            goal: "no verification".into(),
            gates: vec![],
            max_attempts: 1,
        };

        let mut llm = MockLlm { calls: 0 };
        let mut handler = StubHandler::new();
        let config = crate::handler::LoopConfig {
            working_dir: dir.to_string_lossy().to_string(),
            ..Default::default()
        };
        let ctx = oz_core_types::ToolContext::default();
        let stop = std::sync::atomic::AtomicBool::new(false);
        let outcome = run_goal_loop(
            &mut llm,
            String::from("system"),
            &mut handler,
            &[],
            &ctx,
            &config,
            &stop,
            &spec,
        )
        .await;
        assert!(outcome.success);
        assert_eq!(outcome.attempts, 1);
        assert!(outcome.per_gate.is_empty());
    }
}
