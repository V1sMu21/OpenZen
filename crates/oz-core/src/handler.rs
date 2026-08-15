use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Instant;

use async_trait::async_trait;
use oz_core_types::{
    MockResponse, MockToolCall, StepOutcome, ToolContext, ToolResultItem,
};
use serde::Serialize;

// ── Loop-detection guard messages (injected into LLM conversation) ──
const GUARD_LOOP_ABAB: &str = "[GUARD] 检测到工具调用循环(A-B-A-B)，请更换策略！";
const GUARD_LOOP_ABCABC: &str = "[GUARD] 检测到工具调用循环(A-B-C-A-B-C)，请更换策略！";
const GUARD_REPEAT_5X: &str = "[GUARD] 工具 `{tool}` 连续调用5次，请确认进度或换方案！";

pub type ToolCalls = Vec<MockToolCall>;
pub type ToolResults = Vec<ToolResultItem>;

// ── Direction B: AgentState FSM ──

/// Explicit FSM states for the agent loop (Direction B).
/// Makes state transitions visible and auditable instead of implicit.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Default)]
pub enum AgentState {
    #[default]
    /// No LLM call in progress; about to start a turn.
    Idle,
    /// LLM is streaming a response (may include thinking tokens).
    /// Speculative pre-execution may be happening concurrently.
    Thinking,
    /// Tool calls are being dispatched and executed.
    ToolExecution,
    /// Processing results and preparing for the next turn.
    Responding,
    /// Loop finished; carries the exit reason.
    Done(String),
}

impl AgentState {
    pub fn is_terminal(&self) -> bool {
        matches!(self, AgentState::Done(_))
    }

    pub fn transition_to(&self, to: &AgentState) -> String {
        format!("FSM: {} → {}", self.label(), to.label())
    }

    /// Human-readable label for logging.
    pub fn label(&self) -> &str {
        match self {
            AgentState::Idle => "idle",
            AgentState::Thinking => "thinking",
            AgentState::ToolExecution => "tool_execution",
            AgentState::Responding => "responding",
            AgentState::Done(_) => "done",
        }
    }
}

/// Working memory — corresponds to Python GenericAgentHandler.working dict.
#[derive(Debug, Clone, Default)]
pub struct WorkingMemory {
    pub key_info: Option<String>,
    pub related_sop: Option<String>,
    pub passed_sessions: u32,
    pub in_plan_mode: Option<String>,
    pub sensorium: Sensorium,
    /// Current FSM state of the agent loop (Direction B).
    pub current_state: AgentState,
    /// Log of FSM transitions: (from, to, reason).
    pub state_transitions: Vec<(AgentState, AgentState, String)>,
    /// Todo list managed by todowrite/todoupdate tools.
    pub todos: Vec<oz_core_types::TodoItem>,
}

/// Sensorium: zero-latency self-awareness.
#[derive(Debug, Clone)]
pub struct Sensorium {
    pub start_time: std::time::Instant,
    pub calls: u64,
    pub tool_history: Vec<String>,
}

impl Default for Sensorium {
    fn default() -> Self {
        Sensorium::new()
    }
}

impl Sensorium {
    pub fn new() -> Self {
        Sensorium {
            start_time: Instant::now(),
            calls: 0,
            tool_history: Vec::new(),
        }
    }
    pub fn uptime_mins(&self) -> f64 {
        self.start_time.elapsed().as_secs_f64() / 60.0
    }
    pub fn record_tool(&mut self, name: &str) {
        self.calls += 1;
        self.tool_history.push(name.to_string());
        if self.tool_history.len() > 10 {
            self.tool_history.remove(0);
        }
    }
    pub fn detect_loop(&self) -> Option<String> {
        let h = &self.tool_history;
        let n = h.len();

        // Pattern 1: A-B-A-B (period-2 loop, last 6 calls)
        if n >= 6 && h[n-3..] == h[n-6..n-3] {
            let unique: std::collections::HashSet<&str> =
                h[n-6..].iter().map(|s| s.as_str()).collect();
            if unique.len() == 2 {
                return Some(GUARD_LOOP_ABAB.into());
            }
        }

        // Pattern 2: A-B-C-A-B-C (period-3 loop, last 9 calls)
        if n >= 9 && h[n-3..] == h[n-6..n-3] && h[n-6..n-3] == h[n-9..n-6] {
            let unique: std::collections::HashSet<&str> =
                h[n-9..].iter().map(|s| s.as_str()).collect();
            if unique.len() == 3 {
                return Some(GUARD_LOOP_ABCABC.into());
            }
        }

        // Pattern 3: single tool called 5+ consecutive times
        if n >= 5 {
            let last5: std::collections::HashSet<&str> =
                h[n-5..].iter().map(|s| s.as_str()).collect();
            if last5.len() == 1 {
                return Some(GUARD_REPEAT_5X.replace("{tool}", h.last().unwrap()));
            }
        }

        None
    }
}

/// Circuit breaker — limits each tool to 20 calls per 60s window.
#[derive(Debug, Clone)]
pub struct Breaker {
    counts: HashMap<String, u32>,
    window_start: Instant,
}

impl Breaker {
    pub fn new() -> Self {
        Breaker {
            counts: HashMap::new(),
            window_start: Instant::now(),
        }
    }

    pub fn check(&mut self, tool_name: &str) -> bool {
        if tool_name == "respond" {
            return true;
        }
        if self.window_start.elapsed().as_secs_f64() > 60.0 {
            self.counts.clear();
            self.window_start = Instant::now();
        }
        let entry = self.counts.entry(tool_name.to_string()).or_insert(0);
        *entry += 1;
        *entry <= 20
    }
}

impl Default for Breaker {
    fn default() -> Self { Self::new() }
}

/// Handler trait — all tools register through this.
#[async_trait]
pub trait Handler: Send + Sync {
    fn working(&self) -> &WorkingMemory;
    fn working_mut(&mut self) -> &mut WorkingMemory;

    fn tool_before(&mut self, _tool_name: &str, _args: &serde_json::Value) {}
    fn tool_after(&mut self, _tool_name: &str, _args: &serde_json::Value, _result: Result<StepOutcome, oz_core_types::ToolError>) {}
    fn turn_end(
        &mut self,
        response: &MockResponse,
        tool_calls: &[MockToolCall],
        tool_results: &[ToolResultItem],
        turn: u32,
        next_prompt: String,
        exit_reason: Option<String>,
    ) -> String;

    /// Dispatch a tool call — async now for ToolRegistry integration.
    /// Takes `&self` so independent tool calls can be executed in parallel.
    async fn dispatch(
        &self,
        tool_name: &str,
        args: serde_json::Value,
        response: &MockResponse,
        index: u32,
        ctx: &ToolContext,
    ) -> Result<StepOutcome, oz_core_types::ToolError>;
}

/// Loop configuration.
pub struct LoopConfig {
    pub max_turns: u32,
    pub verbose: bool,
    /// UI locale ("zh" | "en"); drives reply-language and summary-language.
    pub lang: String,
    /// Unified sender for all streaming events (replaces token_tx, thinking_tx,
    /// tool_call_tx, tool_result_tx).
    pub event_tx: Option<tokio::sync::mpsc::UnboundedSender<oz_core_types::StreamEvent>>,
    /// Context window size in characters (for compression).
    pub context_win: usize,
    /// Whether to enable context compression with summarization.
    pub enable_compression: bool,
    /// Path to checkpoint directory for resume (None = no resume).
    pub resume_from: Option<String>,
    /// Session identifier for checkpoint naming.
    pub session_id: String,
    /// Interval (in turns) between automatic checkpoint saves. 0 = disabled.
    pub checkpoint_interval: u32,
    /// Directory containing SOP (.md) files. If set, matching SOPs are
    /// injected into the system prompt at runtime.
    /// Deprecated: prefer `skill_mcp_dir` for unified knowledge management.
    pub sop_dir: Option<String>,
    /// Directory containing the .skill_mcp/ knowledge base.
    /// When set, skills, SOPs, and memory are loaded from this directory.
    pub skill_mcp_dir: Option<String>,
    /// When true, use the LLM to crystallize knowledge from completed sessions.
    pub enable_crystallization: bool,
    /// When true, periodically refine existing skills via LLM.
    pub enable_refinement: bool,
    /// Maximum number of tools to execute concurrently (0 = unlimited).
    pub max_concurrent_tools: usize,
    /// Per-tool execution timeout in seconds.
    pub tool_timeout_secs: u64,
    /// Channel for receiving user interventions mid-loop.
    /// Checked at the start of each turn.
    pub intervention_rx: Option<Arc<Mutex<std::collections::VecDeque<crate::checkpoint::InterventionEvent>>>>,
    /// Channel the `ask_user` tool blocks on for the user's reply.
    /// Lets the loop stay alive across the wait so the same run resumes
    /// with the user's answer as a tool_result.
    pub ask_user_rx: Option<Arc<Mutex<Option<String>>>>,
    /// Working directory for saving checkpoints and state.
    pub working_dir: String,
    /// Safety guard for checking tool calls (progressive trust + blocklist).
    pub safety_guard: Option<std::sync::Arc<oz_safety::SafetyGuard>>,
    /// Approval handler for requesting user confirmation on tool calls.
    /// When set, tools that need approval will pause the loop until the handler responds.
    pub approval_handler: Option<std::sync::Arc<dyn oz_safety::ApprovalHandler>>,
    /// Timeout in seconds for tool approval requests (default: 300).
    pub approval_timeout_secs: u64,
    /// Project root directory for checkpoints (overrides working_dir derivation).
    /// When set, checkpoints are stored at {checkpoint_dir}/{session_id}/.
    pub checkpoint_dir: Option<String>,
    /// Path to trust.json for this session's Project.
    /// When set, SafetyGuard uses this path instead of {working_dir}/openzen/trust.json.
    pub trust_path: Option<String>,
    /// Model name for compression summaries (e.g. "local-qwen").
    /// When set, compression uses this model instead of the main client.
    pub summary_model_name: Option<String>,
    /// API base URL for the summary model (only needed if different from main).
    pub summary_apibase: Option<String>,
    /// API key for the summary model.
    pub summary_apikey: Option<String>,
    /// Optional callback for operational logs (compression, stalls).
    /// Writes to the platform-specific log file so monitors can see it.
    /// None = logs go to stderr only.
    #[allow(clippy::type_complexity)]
    pub log_fn: Option<std::sync::Arc<dyn Fn(&str) + Send + Sync>>,
    /// LLM stream timeout in seconds. Local models need more time for
    /// prefill; cloud models are faster. Default: 300 (5 min).
    pub stream_timeout_secs: u64,
    /// Max consecutive LLM transport failures (timeout / stream error)
    /// before the loop gives up on the turn and exits with llm_error /
    /// llm_timeout. Local engines (omlx etc.) under contention can abort
    /// streams repeatedly; a higher budget keeps long tasks alive through
    /// transient wedges. Default: 3.
    pub llm_error_retries: u32,
    /// Directory for session rollout recording. When set, all stream events
    /// are appended to {rollout_dir}/rollout-*.jsonl for replay/debug.
    pub rollout_dir: Option<String>,
    /// Background memory distillation scheduler. When set, session
    /// transcripts are enqueued for async knowledge extraction instead of
    /// blocking the loop (U3).
    pub memory_scheduler: Option<std::sync::Arc<crate::memory_job::MemoryJobScheduler>>,
    /// Hook handler for SessionStart / PostToolUse automation. When set,
    /// hook events are fired at loop start and after file-writing tools.
    pub hooks: Option<std::sync::Arc<dyn crate::hooks::HookHandler>>,
    /// Collect compile diagnostics (cargo check / tsc) into the startup
    /// reminder block (P2-8). Off by default; runner opts in.
    pub include_diagnostics: bool,
    /// Master switch for the delivery-quality pipeline (spec anchor +
    /// [verify] assertions + independent review). Off = legacy behaviour.
    pub quality_gates: bool,
    /// Max fix rounds allowed when a [verify] assertion fails before the
    /// loop forces an exit (with a "not verified" note attached).
    pub assertion_max_rounds: u32,
    /// When true, run an independent multi-perspective review before exit
    /// for important tasks (write count >= review_min_tools).
    pub review_enabled: bool,
    /// Minimum number of file-writing tool calls for a task to be
    /// considered "important" enough for the independent review.
    pub review_min_tools: u32,
}

impl Default for LoopConfig {
    fn default() -> Self {
        LoopConfig {
            max_turns: u32::MAX,
            verbose: true,
            lang: "zh".to_string(),
            event_tx: None,
            context_win: 8000,
            enable_compression: true,
            resume_from: None,
            session_id: String::new(),
            checkpoint_interval: 0,
            sop_dir: None,
            skill_mcp_dir: None,
            enable_crystallization: false,
            enable_refinement: false,
            max_concurrent_tools: 8,
            tool_timeout_secs: 30,
            intervention_rx: None,
            ask_user_rx: None,
            working_dir: String::from("."),
            safety_guard: None,
            approval_handler: None,
            approval_timeout_secs: 300,
            checkpoint_dir: None,
            trust_path: None,
            summary_model_name: None,
            summary_apibase: None,
            summary_apikey: None,
            log_fn: None,
            stream_timeout_secs: 300,
            llm_error_retries: 3,
            rollout_dir: None,
            memory_scheduler: None,
            hooks: None,
            include_diagnostics: false,
            quality_gates: true,
            assertion_max_rounds: 2,
            review_enabled: true,
            review_min_tools: 3,
        }
    }
}

impl std::fmt::Debug for LoopConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("LoopConfig")
            .field("max_turns", &self.max_turns)
            .field("verbose", &self.verbose)
            .field("event_tx", &self.event_tx.is_some())
            .finish()
    }
}

impl Clone for LoopConfig {
    fn clone(&self) -> Self {
        LoopConfig {
            max_turns: self.max_turns,
            verbose: self.verbose,
            lang: self.lang.clone(),
            event_tx: None,
            context_win: self.context_win,
            enable_compression: self.enable_compression,
            resume_from: self.resume_from.clone(),
            session_id: self.session_id.clone(),
            checkpoint_interval: self.checkpoint_interval,
            sop_dir: self.sop_dir.clone(),
            skill_mcp_dir: self.skill_mcp_dir.clone(),
            enable_crystallization: self.enable_crystallization,
            enable_refinement: self.enable_refinement,
            max_concurrent_tools: self.max_concurrent_tools,
            tool_timeout_secs: self.tool_timeout_secs,
            intervention_rx: self.intervention_rx.clone(),
            ask_user_rx: self.ask_user_rx.clone(),
            working_dir: self.working_dir.clone(),
            safety_guard: self.safety_guard.clone(),
            approval_handler: self.approval_handler.clone(),
            approval_timeout_secs: self.approval_timeout_secs,
            checkpoint_dir: self.checkpoint_dir.clone(),
            trust_path: self.trust_path.clone(),
            summary_model_name: self.summary_model_name.clone(),
            summary_apibase: self.summary_apibase.clone(),
            summary_apikey: self.summary_apikey.clone(),
            log_fn: self.log_fn.clone(),
            stream_timeout_secs: self.stream_timeout_secs,
            llm_error_retries: self.llm_error_retries,
            rollout_dir: self.rollout_dir.clone(),
            memory_scheduler: self.memory_scheduler.clone(),
            hooks: self.hooks.clone(),
            include_diagnostics: self.include_diagnostics,
            quality_gates: self.quality_gates,
            assertion_max_rounds: self.assertion_max_rounds,
            review_enabled: self.review_enabled,
            review_min_tools: self.review_min_tools,
        }
    }
}

/// Loop outcome returned on completion.
#[derive(Debug, Clone, Serialize)]
pub struct LoopOutcome {
    pub turn: u32,
    pub exit_reason: String,
    pub data: Option<serde_json::Value>,
}

#[cfg(test)]
mod tests {
    use super::*;

    // ---- WorkingMemory ----

    #[test]
    fn working_memory_default_fields_are_none_or_zero() {
        let wm = WorkingMemory::default();
        assert!(wm.key_info.is_none());
        assert!(wm.related_sop.is_none());
        assert_eq!(wm.passed_sessions, 0);
        assert!(wm.in_plan_mode.is_none());
        assert_eq!(wm.sensorium.calls, 0);
        assert!(wm.sensorium.tool_history.is_empty());
    }

    // ---- Sensorium ----

    #[test]
    fn sensorium_new_creates_clean_state() {
        let s = Sensorium::new();
        assert_eq!(s.calls, 0);
        assert!(s.tool_history.is_empty());
    }

    #[test]
    fn sensorium_record_tool_increments_calls_and_appends_history() {
        let mut s = Sensorium::new();
        s.record_tool("read_file");
        assert_eq!(s.calls, 1);
        assert_eq!(s.tool_history, vec!["read_file"]);

        s.record_tool("write_file");
        assert_eq!(s.calls, 2);
        assert_eq!(s.tool_history, vec!["read_file", "write_file"]);
    }

    #[test]
    fn sensorium_record_tool_keeps_max_10_in_history() {
        let mut s = Sensorium::new();
        for i in 0..12 {
            s.record_tool(&format!("tool_{}", i));
        }
        assert_eq!(s.tool_history.len(), 10);
        assert_eq!(s.tool_history[0], "tool_2");
        assert_eq!(s.tool_history[9], "tool_11");
    }

    #[test]
    fn sensorium_detect_loop_no_loop_with_less_than_6_items() {
        let mut s = Sensorium::new();
        for _ in 0..5 {
            s.record_tool("tool_a");
            s.record_tool("tool_b");
        }
        // Clear to leave < 6 items
        s.tool_history = vec!["tool_a".into(), "tool_b".into(), "tool_a".into()];
        assert!(s.detect_loop().is_none());
    }

    #[test]
    fn sensorium_detect_loop_abab_pattern() {
        let mut s = Sensorium::new();
        s.tool_history = vec![
            "a".into(), "a".into(), "b".into(),
            "a".into(), "a".into(), "b".into(),
        ];
        let result = s.detect_loop();
        assert!(result.is_some());
        assert!(result.unwrap().contains("A-B-A-B"));
    }

    #[test]
    fn sensorium_detect_loop_no_false_positive_with_four_unique() {
        let mut s = Sensorium::new();
        // Repeating pattern but 4 unique - should NOT trigger A-B-A-B
        s.tool_history = vec![
            "a".into(), "b".into(), "c".into(),
            "a".into(), "b".into(), "c".into(),
        ];
        let result = s.detect_loop();
        // unique in last 6 is {a,b,c}=3 != 2, so no A-B-A-B trigger
        assert!(result.is_none());
    }

    #[test]
    fn sensorium_detect_loop_same_tool_five_times() {
        let mut s = Sensorium::new();
        for _ in 0..5 {
            s.record_tool("same_tool");
        }
        let result = s.detect_loop();
        assert!(result.is_some());
        assert!(result.unwrap().contains("same_tool"));
    }

    // ---- Breaker ----

    #[test]
    fn breaker_new_creates_clean_state() {
        let b = Breaker::new();
        assert!(b.counts.is_empty());
    }

    #[test]
    fn breaker_check_respond_always_passes() {
        let mut b = Breaker::new();
        for _ in 0..100 {
            assert!(b.check("respond"));
        }
    }

    #[test]
    fn breaker_check_allows_20_calls_then_blocks() {
        let mut b = Breaker::new();
        for i in 1..=20 {
            assert!(b.check("my_tool"), "call {} should pass", i);
        }
        assert!(!b.check("my_tool"), "call 21 should be blocked");
    }

    #[test]
    fn breaker_check_tracks_tools_independently() {
        let mut b = Breaker::new();
        for _ in 0..20 {
            b.check("tool_a");
        }
        // tool_b should still pass since it's tracked separately
        assert!(b.check("tool_b"));
    }

    // ---- LoopConfig ----

    #[test]
    fn loop_config_default_values() {
        let config = LoopConfig::default();
        assert_eq!(config.max_turns, u32::MAX);
        assert!(config.verbose);
    }

    // ---- LoopOutcome ----

    #[test]
    fn loop_outcome_fields() {
        let outcome = LoopOutcome {
            turn: 5,
            exit_reason: "max_turns".into(),
            data: None,
        };
        assert_eq!(outcome.turn, 5);
        assert_eq!(outcome.exit_reason, "max_turns");
        assert!(outcome.data.is_none());

        let outcome2 = LoopOutcome {
            turn: 3,
            exit_reason: "tool_return".into(),
            data: Some(serde_json::json!({"key": "value"})),
        };
        assert!(outcome2.data.is_some());
    }
}
