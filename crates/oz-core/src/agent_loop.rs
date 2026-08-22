use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;

use oz_core_types::{
    ContentBlock, Message, MockResponse, Role, StepOutcome, StreamEvent, ToolContext,
    ToolDefinition, ToolError, ToolResultItem,
};

use crate::crystallizer::Crystallizer;
use crate::handler::{AgentState, Breaker, Handler, LoopConfig, LoopOutcome};
use crate::meter;
use crate::refiner::Refiner;
use crate::sop::SopStore;

const BREAKER_BLOCKED_MSG: &str = "[BREAKER] 工具 {tool} 调用过于频繁，已跳过本轮";
const DANGER_LOOP_MSG: &str =
    "\n[DANGER] 已连续执行第 {turn} 轮。禁止无效重试。若无有效进展，必须切换策略";

static BLOCK_ID_COUNTER: AtomicU64 = AtomicU64::new(0);

/// Tools eligible for speculative pre-execution: strictly read-only lookups.
/// Side-effectful tools (write/edit/patch/code_run…) must NEVER run ahead of
/// the complete-response + approval semantics (round3 P0-B), and a tool the
/// breaker is throttling must not sneak past it via the speculative lane.
const SPECULATIVE_READ_ONLY_TOOLS: &[&str] = &[
    "read",
    "grep",
    "glob",
    "ls",
    "web_search",
    "web_fetch",
    "skill_mcp_search",
    "skill_mcp_list",
];

/// Slot key for reply writers that cannot supply a tool_use_id (IM
/// bridges, legacy HTTP clients) — consumed only after the exact
/// question id misses (round3 P1-i).
pub(crate) const LEGACY_ASK_USER_KEY: &str = "__last__";

fn next_block_id(prefix: &str) -> String {
    let n = BLOCK_ID_COUNTER.fetch_add(1, Ordering::Relaxed);
    format!("{prefix}{n}")
}

/// Save a stop-path checkpoint without blocking the runtime thread: git
/// metadata and the full JSON write run on the blocking pool. Ensures all
/// stop paths (top of loop, LLM stream cancel, ask_user wait) persist
/// state for /resume.
#[allow(clippy::too_many_arguments)]
async fn save_stop_checkpoint_async(
    config: &LoopConfig,
    turn: u32,
    exit: &str,
    messages: &[Message],
    history_info: &[String],
    full_response: &str,
    full_thinking: &str,
    todos: &[oz_core_types::TodoItem],
) {
    if config.session_id.is_empty() {
        return;
    }
    let cp_dir = crate::checkpoint::checkpoint_dir(std::path::Path::new(&config.working_dir));
    let (git_sha, git_branch, git_origin_url) =
        crate::checkpoint::git_snapshot_async(std::path::Path::new(&config.working_dir)).await;
    let cp = crate::checkpoint::LoopCheckpoint {
        turn,
        timestamp: chrono::Utc::now().timestamp() as f64,
        messages: messages.to_vec(),
        history_info: history_info.to_vec(),
        full_response: full_response.to_string(),
        exit_reason: Some(exit.to_string()),
        session_id: Some(config.session_id.clone()),
        plan: crate::checkpoint::plan_from_todos(todos),
        todos: todos.to_vec(),
        interventions: vec![],
        full_thinking: Some(full_thinking.to_string()),
        git_sha,
        git_branch,
        git_origin_url,
    };
    crate::checkpoint::save_checkpoint_persist_async(&cp_dir, &config.session_id, cp).await;
}

/// Sleep for the exponential backoff delay of a failed LLM attempt,
/// aborting early when the stop signal fires (stop must stay responsive).
/// `consecutive` is the 1-based consecutive-error count.
async fn backoff_or_stop(stop_signal: &AtomicBool, consecutive: u32) {
    let delay = oz_llm::retry::compute_delay(consecutive.saturating_sub(1) as usize, None);
    let mut waited = 0.0_f64;
    while waited < delay {
        if stop_signal.load(Ordering::Relaxed) {
            return;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
        waited += 0.1;
    }
}

/// Transition the agent FSM state and log the change.
fn transition_state(handler: &mut dyn Handler, to: AgentState, reason: &str) {
    let from = handler.working_mut().current_state.clone();
    handler
        .working_mut()
        .state_transitions
        .push((from.clone(), to.clone(), reason.to_string()));
    handler.working_mut().current_state = to;
    tracing::debug!(
        "{} — {}",
        from.transition_to(&handler.working().current_state),
        reason
    );
}

/// A pending `ask_user` tool call waiting for the user's reply.
struct PendingAskUser {
    tool_use_id: String,
    /// Canonical SSE tool_call_id matching the frontend card key (tc_id),
    /// so ToolOutputAvailable after ask_user reply matches ToolInputStart.
    tool_call_id: String,
    tool_name: String,
    payload: serde_json::Value,
}

/// Metadata for a tool call in the parallel execution queue.
struct ToolCallMeta {
    ii: usize,
    tool_name: String,
    args: serde_json::Value,
    tid: String,
    /// Canonical SSE tool_call_id used for ALL events of this tool
    /// (ToolInputStart, ToolInputAvailable, ToolOutputAvailable).
    /// Pre-computed here so Phase 6 emits ToolOutputAvailable with the
    /// same id the frontend received in ToolInputStart — otherwise
    /// `findLast(toolCallId === event.tool_call_id)` returns nothing
    /// and the card stays in "Running" forever.
    tc_id: String,
    blocked: bool,
}

/// Process the outcome of a single tool dispatch, updating shared state.
/// Every outcome (success AND error) is recorded in `tool_results` so
/// the next LLM turn can pair it with the assistant's `tool_use`
/// block via a `ContentBlock::tool_result`.
///
/// On `INTERRUPT + HUMAN_INTERVENTION` (i.e. ask_user) we capture the
/// payload into `pending_ask_user` and return without recording a result
/// or setting `exit_reason`. The agent loop then waits on `ask_user_rx`
/// and synthesizes a `tool_result` for the same tool_use id, so the same
/// run continues after the user answers.
#[allow(clippy::too_many_arguments)]
fn process_tool_outcome(
    outcome: Result<StepOutcome, oz_core_types::ToolError>,
    tool_name: &str,
    tid: &str,
    tc_id: &str,
    ii: usize,
    tool_results: &mut Vec<ToolResultItem>,
    next_prompts: &mut Vec<String>,
    _full_response: &mut String,
    exit_reason: &mut Option<String>,
    _config: &LoopConfig,
    pending_ask_user: &mut Vec<PendingAskUser>,
) {
    match outcome {
        Ok(oc) => {
            if oc.data.get("status").and_then(|v| v.as_str()) == Some("INTERRUPT")
                && oc.data.get("intent").and_then(|v| v.as_str()) == Some("HUMAN_INTERVENTION")
            {
                pending_ask_user.push(PendingAskUser {
                    tool_use_id: tid.to_string(),
                    tool_call_id: tc_id.to_string(),
                    tool_name: tool_name.to_string(),
                    payload: oc.data.clone(),
                });
                return;
            }
            if oc.should_exit {
                *exit_reason = Some("EXITED".into());
                tool_results.push(ToolResultItem {
                    tool_use_id: tid.to_string(),
                    content: tool_result_content(&oc.data),
                    images: oc.images.clone(),
                });
                return;
            }
            tool_results.push(ToolResultItem {
                tool_use_id: tid.to_string(),
                content: tool_result_content(&oc.data),
                images: oc.images.clone(),
            });
            if let Some(np) = oc.next_prompt {
                if !(np == "\n" && ii > 0) || np.len() > 1 {
                    next_prompts.push(np);
                }
            }
        }
        Err(e) => {
            let err_msg = format!("[TOOL_ERROR] {}: {}", tool_name, e);
            tool_results.push(ToolResultItem {
                tool_use_id: tid.to_string(),
                content: err_msg.clone(),
                images: vec![],
            });
            next_prompts.push(err_msg);
        }
    }
}

/// Serialize a tool outcome into the tool_result content that enters the
/// LLM context, capped at 100K chars (head+tail). The SSE/persistence
/// layers already cap at the same budget, but the context path had no
/// cap at all: one `cat big.log` in code_run inline mode returned full
/// stdout into messages — context explosion plus an emergency-compression
/// LLM call to recover.
fn tool_result_content(data: &serde_json::Value) -> String {
    const MAX_TOOL_RESULT_CTX_CHARS: usize = 100_000;
    let serialized = serde_json::to_string(data).unwrap_or_default();
    if serialized.len() <= MAX_TOOL_RESULT_CTX_CHARS {
        return serialized;
    }
    smart_format(&serialized, MAX_TOOL_RESULT_CTX_CHARS)
}

/// Char-safe cap for tool output streamed to the UI. `String::truncate`
/// panics when the cut lands mid-UTF-8 character — CJK tool results larger
/// than the cap hit this on ~2/3 of offsets (same bug class fixed in
/// webui/mod.rs and wechat; this SSE path had been missed).
fn truncate_stream_output(s: &str, max_bytes: usize) -> String {
    if s.len() <= max_bytes {
        return s.to_string();
    }
    let mut end = max_bytes;
    while end > 0 && !s.is_char_boundary(end) {
        end -= 1;
    }
    format!(
        "{}\n...[truncated, original {} bytes, kept first {}]",
        &s[..end],
        s.len(),
        end
    )
}

/// Byte offset of the n-th char (clamped to len) — lets smart_format slice
/// without materializing a Vec<char> of the whole string (P2-l: 100K+ tool
/// results allocated ~400KB per call just to keep head+tail).
fn nth_char_offset(s: &str, n: usize) -> usize {
    s.char_indices().nth(n).map(|(i, _)| i).unwrap_or(s.len())
}

/// Truncate text to a readable length, showing start and end with "..." in between.
fn smart_format(text: &str, max_len: usize) -> String {
    let chars = text.chars().count();
    if chars <= max_len {
        return text.to_string();
    }
    if max_len == 0 {
        let half = 3.min(chars / 2);
        return format!(
            "{}...{}",
            &text[..nth_char_offset(text, half)],
            &text[nth_char_offset(text, chars - half)..]
        );
    }
    let half = max_len / 2;
    format!(
        "{}...{}",
        &text[..nth_char_offset(text, half)],
        &text[nth_char_offset(text, chars - half)..]
    )
}

/// Internal tag wrappers dropped by `strip_summary_tags`, compiled once.
static INTERNAL_TAG_PATTERNS: std::sync::LazyLock<Vec<regex::Regex>> =
    std::sync::LazyLock::new(|| {
        [
            r"(?is)<antThinking[^>]*>.*?</antThinking\s*>",
            r"(?is)<antThinking[^>]*/>",
            r"(?is)<thinking[^>]*>.*?</thinking\s*>",
            r"(?is)<thinking[^>]*/>",
            r"(?is)<tool_code[^>]*>.*?</tool_code\s*>",
            r"(?is)<tool_code[^>]*/>",
            r"(?is)<respond[^>]*>",
            r"(?is)</respond\s*>",
            r"(?is)<summary\s*>",
            r"(?is)</summary\s*>",
            r"(?is)<tool_use\s*>",
            r"(?is)</tool_use\s*>",
            r"(?is)<file_content\s*>",
            r"(?is)</file_content\s*>",
            r"(?is)</thinking\s*>",
        ]
        .iter()
        .filter_map(|p| regex::Regex::new(p).ok())
        .collect()
    });

/// Strip model-output tag wrappers from `text`, returning the cleaned
/// text and the extracted `<summary>` body. Other tags
/// (`<antThinking>`, `<thinking>`, `<tool_code>`, `<respond>`) are
/// dropped entirely — they are the model's internal scratch space
/// and must not reach the user. Summary content is preserved (it's
/// intended for the long-term working-memory record).
fn strip_summary_tags(text: &str) -> (String, String) {
    let mut cleaned = text.to_string();
    let mut summary = String::new();

    // Pull out <summary>…</summary> bodies first so the surrounding
    // tags don't get left behind.
    while let Some(start) = cleaned.find("<summary>") {
        if let Some(end) = cleaned[start..].find("</summary>") {
            let content = cleaned[start + 9..start + end].to_string();
            if !summary.is_empty() {
                summary.push('\n');
            }
            summary.push_str(content.trim());
            cleaned.replace_range(start..start + end + 10, "");
        } else {
            break;
        }
    }

    // Drop every other internal tag and its content. The stream
    // parser should have already routed these to the reasoning
    // channel, but the agent loop stores the raw LLM text in
    // `full_response` and may re-display it; we double-clean here so
    // historical sessions rendered from disk stay tag-free.
    for re in INTERNAL_TAG_PATTERNS.iter() {
        cleaned = re.replace_all(&cleaned, "").to_string();
    }

    (cleaned, summary)
}

/// Extract tool calls from a MockResponse.
pub fn extract_tool_calls(response: &MockResponse) -> Vec<oz_core_types::MockToolCall> {
    response.tool_calls.clone()
}

/// Build the `<system-reminder>` block carrying dynamic context that must NOT
/// live in the system prompt (so the system prompt stays byte-stable and
/// keeps the omlx prefix-cache chain intact). Injected as a prefix of the
/// first user message, mirroring Claude Code's dynamic-reminder pattern.
async fn build_system_reminder(working_dir: &str, session_id: &str) -> String {
    let (git_sha, git_branch, _origin) =
        crate::checkpoint::git_snapshot_async(std::path::Path::new(working_dir)).await;
    let date = chrono::Local::now().format("%Y-%m-%d");
    let mut block =
        format!("<system-reminder>\nToday's date: {date}\nWorking directory: {working_dir}\n");
    if let Some(branch) = git_branch {
        block.push_str(&format!("Git branch: {branch}\n"));
    }
    if let Some(sha) = git_sha {
        let short = &sha[..sha.len().min(7)];
        block.push_str(&format!("Git commit: {short}\n"));
    }
    if !session_id.is_empty() {
        block.push_str(&format!("Session: {session_id}\n"));
    }
    // Prior failure reflections (Reflexion): point the agent at them so it
    // can avoid repeating past mistakes.
    if crate::quality::reflection_log_exists(working_dir) {
        block.push_str(
            "Prior task failures: .openzen/reflections.jsonl (read with the `read` tool to avoid repeating past mistakes)\n",
        );
    }
    block.push_str("</system-reminder>");
    block
}

/// Wait (bounded) for the LLM summary from `spawn_summary`. Returns the
/// real summary when it arrives within `wait_secs`; otherwise returns the
/// `fallback` template. The caller injects a SINGLE final result — the
/// fallback is terminal and is never replaced by a late-arriving summary
/// on a later turn, because that second mutation would break the omlx
/// prefix-cache chain (one compression = exactly one prefix change).
async fn wait_for_summary(
    rx: Option<tokio::sync::oneshot::Receiver<String>>,
    fallback: String,
    wait_secs: u64,
) -> String {
    let Some(mut rx) = rx else {
        return fallback;
    };
    match tokio::time::timeout(std::time::Duration::from_secs(wait_secs), &mut rx).await {
        Ok(Ok(s)) if !s.is_empty() => s,
        _ => fallback,
    }
}

/// The main agent loop — matches Python agent_runner_loop() behavior.
///
/// ```python
/// def agent_runner_loop(client, system_prompt, user_input, handler, tools_schema, max_turns=40):
///     messages = [{"role":"system","content":system_prompt}, {"role":"user","content":user_input}]
///     for turn in range(max_turns):
///         response = client.chat(messages, tools)
///         for tc in tool_calls:
///             outcome = handler.dispatch(tool_name, args, response)
///             if outcome.should_exit: break
/// ```
#[allow(clippy::too_many_arguments)]
pub async fn run_agent_loop<C>(
    client: &mut C,
    system_prompt: String,
    user_input: String,
    additional_messages: Vec<Message>,
    handler: &mut dyn Handler,
    tools: &[ToolDefinition],
    ctx: &ToolContext,
    config: &LoopConfig,
    stop_signal: &AtomicBool,
) -> LoopOutcome
where
    C: oz_core_types::LlmClient,
{
    let mut messages = Vec::with_capacity(2 + additional_messages.len());
    messages.push(Message::system(&system_prompt));
    messages.extend(additional_messages);
    // Dynamic context (date/cwd/git) goes in a `<system-reminder>` prefix on
    // the first user message — never in the system prompt — so the system
    // block stays byte-stable for prefix caching.
    let reminder = build_system_reminder(&config.working_dir, &config.session_id).await;
    // P2-8: compile diagnostics ride inside the reminder block when enabled.
    let reminder = if config.include_diagnostics {
        match crate::diagnostics::collect_diagnostics_block(&config.working_dir).await {
            block if !block.is_empty() => format!("{reminder}\n{block}"),
            _ => reminder,
        }
    } else {
        reminder
    };
    // SessionStart hooks append extra context inside the reminder block,
    // keeping it byte-stable for prefix caching.
    let reminder = if let Some(ref hooks) = config.hooks {
        match hooks.fire(&crate::hooks::HookEvent::SessionStart) {
            Some(extra) if !extra.is_empty() => reminder.replace(
                "</system-reminder>",
                &format!("{extra}\n</system-reminder>"),
            ),
            _ => reminder,
        }
    } else {
        reminder
    };
    let user_input = if reminder.is_empty() {
        user_input
    } else {
        format!("{reminder}\n\n{user_input}")
    };
    messages.push(Message::user(&user_input));

    let mut turn: u32 = 0;
    let mut breaker = Breaker::new();
    let mut history_info: Vec<String> = Vec::new();
    // ── Delivery-quality pipeline state (spec anchor / assertions / review) ──
    // One-shot spec hint: only injected once per run so the agent cannot
    // loop on it; the assertion/review fix budgets bound the extra rounds.
    let mut spec_hint_sent = false;
    // P1: one-shot gentle plan nudge (non-blocking, ignore-able).
    let mut plan_hint_sent = false;
    let mut assertion_rounds: u32 = 0;
    let mut assertions_exhausted_checked = false;
    let mut review_rounds: u32 = 0;
    let mut quality_note: Option<String> = None;
    // P2: unresolved-suspicion closure — one-shot confirmation prompt.
    let mut suspicion_checked = false;
    // DQ1 one-shots + QA-4 contract captures.
    let mut auto_spec_attempted = false;
    let mut diff_selfcheck_done = false;
    let mut tdd_nudged = false;
    let mut contract_assertions: Option<(usize, usize)> = None; // (total, failed)
    let mut contract_review: Option<(bool, usize)> = None; // (pass, high count)
    // Consecutive LLM transport failures (timeout / stream error). Local
    // servers (omlx etc.) can wedge for minutes; instead of terminating
    // the whole long task, retry the same turn. Cap retries so a
    // persistently-broken backend still exits instead of looping forever.
    let mut consecutive_llm_errors: u32 = 0;
    let max_llm_error_retries: u32 = config.llm_error_retries;

    // Helper: logs to config.log_fn (→ openzen.log) if wired, else stderr.
    let agent_log = |msg: &str| {
        if let Some(ref f) = config.log_fn {
            f(msg);
        } else {
            eprintln!("[openzen] {msg}");
        }
    };
    let mut full_response = String::new();
    let mut full_thinking = String::new();
    let mut total_input_tokens: u64 = 0;
    let mut total_output_tokens: u64 = 0;
    let mut last_turn_input_tokens: u64 = 0;
    let mut last_turn_output_tokens: u64;
    // Pairs with last_turn_input_tokens: lets pre-exit/post-tool checks
    // project the real tokens ratio onto grown contexts (stale token
    // count alone cannot see single-turn context explosions).
    let mut last_turn_input_chars: u64;
    let mut exit_reason: Option<String> = None;
    let mut last_todo_summary_turn: u32 = 0;
    let mut last_todo_snapshot: String = String::new();
    let mut consecutive_intent_hits: u32 = 0;
    let mut consecutive_empty_turns: u32 = 0;

    // Session rollout recorder (U4): mirrors all stream events to a JSONL file.
    let (git_sha, git_branch, _git_origin) =
        crate::checkpoint::git_snapshot_async(std::path::Path::new(&config.working_dir)).await;
    let mut rollout: Option<crate::rollout::RolloutRecorder> =
        config.rollout_dir.as_ref().and_then(|dir| {
            let meta = crate::rollout::RolloutMeta {
                session_id: config.session_id.clone(),
                cwd: config.working_dir.clone(),
                model: "unknown".into(),
                git_sha,
                git_branch,
            };
            crate::rollout::RolloutRecorder::create(std::path::Path::new(dir), &meta).ok()
        });

    meter::record_session();

    let mut tool_sequence: Vec<(String, serde_json::Value)> = Vec::new();

    // ── Knowledge store: unified skill/SOP/memory injection ──
    // P1-d: prefer the AppState-level shared store (one process-wide instance,
    // mtime-gated incremental reload) over re-walking and re-parsing every
    // SKILL.md on each user message. Local construction stays as the
    // TUI/CLI/test fallback.
    type SharedSkillStore = std::sync::Arc<tokio::sync::Mutex<oz_skill_mcp::SkillMcpStore>>;
    let skill_mcp_store: Option<SharedSkillStore> = match &config.skill_mcp_store {
        Some(shared) => {
            if let Ok(mut guard) = shared.try_lock() {
                let _ = guard.reload_incremental();
            }
            Some(std::sync::Arc::clone(shared))
        }
        None => config.skill_mcp_dir.as_ref().map(|dir| {
            let ks = oz_skill_mcp::SkillMcpStore::new(
                &std::path::PathBuf::from(&config.working_dir),
                Some(std::path::PathBuf::from(dir)),
            );
            tracing::info!(
                "[openzen] Knowledge store: {} skills, {} SOPs from {}",
                ks.skill_count(),
                ks.sop_count(),
                dir
            );
            std::sync::Arc::new(tokio::sync::Mutex::new(ks))
        }),
    };
    if skill_mcp_store.is_none() {
        tracing::warn!("WARNING: skill_mcp_dir is None — NO SKILLS OR SOPS WILL BE LOADED. config.skill_mcp_dir={:?}", config.skill_mcp_dir);
    }

    // If skill_mcp_store is active, inject the compact skill/SOP index at
    // loop start (progressive disclosure: name+description only, ~100 tokens).
    // Full bodies are fetched on demand via skill_mcp_search.
    if let Some(ref store_arc) = skill_mcp_store {
        let skill_index = {
            let store = store_arc.lock().await;
            store.build_index()
        };
        if !skill_index.is_empty() {
            tracing::info!(
                "[openzen] Injected skill/SOP index ({} chars)",
                skill_index.len()
            );
            if let Some(system_msg) = messages.iter_mut().find(|m| m.role == Role::System) {
                system_msg.content.push(ContentBlock::text(&skill_index));
            } else {
                messages.insert(0, Message::system(&skill_index));
            }
        } else {
            tracing::info!("[openzen] SkillMcpStore active but no active skills/SOPs registered");
        }
    }

    // ── Legacy: sop_dir fallback (deprecated, use skill_mcp_dir) ──
    let mut sop_store = if skill_mcp_store.is_none() {
        config.sop_dir.as_ref().map(|dir| {
            let store = SopStore::new(std::path::PathBuf::from(dir));
            if !store.is_empty() && config.verbose {
                tracing::info!("Loaded {} SOP(s) from {}", store.len(), dir);
            }
            store
        })
    } else {
        None
    };

    if let Some(ref store) = sop_store {
        if !store.is_empty() {
            let sop_snippet = store.build_prompt_snippet(&user_input, 3);
            if !sop_snippet.is_empty() {
                messages.insert(0, Message::system(&sop_snippet));
            }
        }
    }

    // Try to resume from checkpoint if configured
    if let Some(ref resume_from) = config.resume_from {
        if let Some(cp) = crate::checkpoint::load_best_loop_checkpoint(
            &std::path::PathBuf::from(resume_from),
            &config.session_id,
        ) {
            tracing::info!(
                "Resuming from checkpoint at turn {} ({} messages, {} todos)",
                cp.turn,
                cp.messages.len(),
                cp.todos.len()
            );
            messages = cp.messages;
            turn = cp.turn;
            history_info = cp.history_info;
            full_response = cp.full_response;
            if !cp.todos.is_empty() {
                handler.working_mut().todos = cp.todos;
            }
            // Clear exit_reason — the user chose to resume, so don't
            // let the previous stop reason cause an immediate exit.
        }
    }

    // ── P1: Async compression summary service ──
    // Spawns LLM summary generation in background tasks via oneshot channels.
    // The agent loop fires a summary request without blocking, then collects
    // the result on a subsequent turn.
    let compression_service = crate::compress::CompressionService::new(
        config.summary_model_name.clone(),
        config.summary_apibase.clone(),
        config.summary_apikey.clone(),
        config.lang.clone(),
    );
    // Phase 1/2 micro-trims save only hundreds of tokens; stay silent below 1K.
    const COMPRESSION_NOTICE_MIN_TOKENS: usize = 1000;

    // Safety valve against verification false-negatives: count consecutive
    // verify failures per todo id. If the same todo fails verification twice
    // in a row, accept the completion with a warning instead of reverting it
    // forever — otherwise a persistently-failing verifier traps the agent in
    // an infinite todoupdate → revert → retry loop (observed with bare
    // filenames in todos whose files live in nested project subdirectories).
    let mut verify_fail_counts: std::collections::HashMap<String, u32> =
        std::collections::HashMap::new();

    'turn: while turn < config.max_turns {
        if stop_signal.load(Ordering::Relaxed) {
            tracing::info!("Stop signal received, saving checkpoint and exiting agent loop");
            save_stop_checkpoint_async(
                config,
                turn,
                "stopped_by_user",
                &messages,
                &history_info,
                &full_response,
                &full_thinking,
                &handler.working().todos,
            )
            .await;
            transition_state(
                handler,
                AgentState::Done("stopped_by_user".into()),
                "stop signal received",
            );
            return LoopOutcome {
                turn,
                exit_reason: "stopped_by_user".into(),
                data: Some(serde_json::json!({
                    "full_response": full_response.clone(),
                    "input_tokens_est": total_input_tokens,
                    "output_tokens_est": total_output_tokens,
                    "context_tokens_est": last_turn_input_tokens,
                })),
            };
        }

        // Check for user interventions before each turn
        if let Some(ref intervention_rx) = config.intervention_rx {
            let interventions = {
                let mut queue = intervention_rx
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner);
                let mut items = Vec::new();
                while let Some(evt) = queue.pop_front() {
                    items.push(evt);
                }
                items
            };
            for intervention in &interventions {
                tracing::info!(
                    "Applying intervention '{}': {}",
                    intervention.kind,
                    &intervention.content[..intervention.content.len().min(100)]
                );
                crate::checkpoint::apply_intervention(&mut messages, intervention);

                // Notify frontend to render an intervention card
                if let Some(ref tx) = config.event_tx {
                    let _ = tx.send(StreamEvent::UserIntervention {
                        content: intervention.content.clone(),
                    });
                }

                // If pause, save final checkpoint and stop
                if matches!(
                    intervention.kind,
                    crate::checkpoint::InterventionKind::Pause
                ) {
                    tracing::info!("Pause intervention received, saving checkpoint and stopping");
                    save_stop_checkpoint_async(
                        config,
                        turn,
                        "paused_by_user",
                        &messages,
                        &history_info,
                        &full_response,
                        &full_thinking,
                        &handler.working().todos,
                    )
                    .await;
                    transition_state(
                        handler,
                        AgentState::Done("paused_by_user".into()),
                        "pause intervention",
                    );
                    return LoopOutcome {
                        turn,
                        exit_reason: "paused_by_user".into(),
                        data: Some(serde_json::json!({"full_response": full_response.clone()})),
                    };
                }
            }
        }

        turn += 1;

        // Record turn boundary in rollout (U4).
        if let Some(ref mut r) = rollout {
            let ev = oz_core_types::StreamEvent::DataContextUsage {
                current_tokens: last_turn_input_tokens,
                output_tokens: total_output_tokens,
                context_window: config.context_win,
                turn,
                message_count: messages.len(),
                total_input_tokens,
                total_output_tokens,
            };
            let _ = r.write(&ev);
        }

        // Context compression before each LLM call.
        if config.enable_compression && config.context_win > 0 {
            let comp_config = crate::compress::CompressionConfig::default();
            let stats_before = crate::compress::measure_usage(&messages);
            let before_count = messages.len();

            // Phase 3 removes messages AFTER the system prompts (the
            // system block never leaves). The summary must cover exactly
            // the removed window, so the JSON slice starts past the
            // system messages instead of at index 0.
            let sys_count = messages
                .iter()
                .take_while(|m| m.role == oz_core_types::Role::System)
                .count();

            // Auto-calibrate token estimate. On the first turn after
            // /resume, last_turn_input_tokens is 0 — the LLM hasn't run
            // yet. Without correction, compression never triggers on the
            // turn that needs it most (82K+ tokens of restored context).
            // Use measure_usage chars-estimate as a floor, so we always
            // have a meaningful number.
            let mut est_tokens = last_turn_input_tokens as usize;
            if est_tokens == 0 && stats_before.total_chars > 0 {
                // No LLM token count yet — estimate from chars using the
                // chars/4 fallback. This is imprecise for Chinese text
                // but far better than 0 (which causes 82K-token contexts
                // to bypass compression entirely).
                est_tokens = stats_before.total_chars / 4;
            }
            let trigger_tokens = config.context_win * comp_config.trigger_pct as usize / 100;
            // hard_max_tokens ceiling. If token count exceeds the
            // emergency ceiling (170K by default), force compression
            // regardless of percentage. This catches the exact scenario
            // this bug fix addresses: 82K context but trigger at 204K
            // (80% of 256K) → compression never fires.
            let force_compress =
                est_tokens > trigger_tokens || est_tokens > comp_config.hard_max_tokens;
            if config.verbose {
                let msg = format!(
                    "compress check: est={est_tokens} trigger={trigger_tokens} hard_max={} ctx_win={} chars={} msgs={before_count} force={force_compress}",
                    comp_config.hard_max_tokens,
                    config.context_win,
                    stats_before.total_chars,
                );
                tracing::warn!("{msg}");
                agent_log(&msg);
            }

            // Snapshot lazily: serializing every message costs a full
            // deep JSON pass (~hundreds of KB near the ceiling) and is
            // only read when Phase 3 actually removes messages, which
            // happens exclusively on the force_compress path.
            let snapshot_before: Vec<serde_json::Value> = if force_compress {
                messages
                    .iter()
                    .filter_map(|m| serde_json::to_value(m).ok())
                    .collect()
            } else {
                Vec::new()
            };

            let saved = if force_compress {
                crate::compress::compress_messages(
                    &mut messages,
                    config.context_win,
                    &comp_config,
                    Some(est_tokens.max(1)),
                )
            } else {
                0
            };
            if saved > 0 {
                let stats_after = crate::compress::measure_usage(&messages);
                // Report on the LLM's REAL reported input total (system
                // prompt + tools + messages). The after-side projects the
                // same basis by the chars ratio — chars strictly shrank after
                // a successful compress, so before > after holds without cap.
                let before_tokens = est_tokens.max(1);
                let after_tokens = if stats_before.total_chars > 0 {
                    (before_tokens as f64 * stats_after.total_chars as f64
                        / stats_before.total_chars as f64)
                        .round() as usize
                } else {
                    0
                };
                let saved_tokens = before_tokens.saturating_sub(after_tokens);
                // Sync the per-turn estimate with the compressed context;
                // otherwise the next compress check reuses the stale
                // pre-compression count and force-fires every turn.
                last_turn_input_tokens = after_tokens as u64;
                if config.verbose {
                    let msg = format!("Context compression saved {saved_tokens} tokens ({before_tokens} → {after_tokens})");
                    tracing::warn!("{msg}");
                    agent_log(&msg);
                }
                if let Some(ref tx) = config.event_tx {
                    if saved_tokens >= COMPRESSION_NOTICE_MIN_TOKENS {
                        let _ = tx.send(oz_core_types::StreamEvent::DataCompressingContext {
                            before_tokens,
                            after_tokens,
                            saved_tokens,
                        });
                    }
                    let _ = tx.send(oz_core_types::StreamEvent::DataContextUsage {
                        current_tokens: after_tokens as u64,
                        output_tokens: 0,
                        context_window: config.context_win,
                        turn,
                        message_count: messages.len(),
                        total_input_tokens,
                        total_output_tokens,
                    });
                }

                let removed_count = before_count.saturating_sub(messages.len());

                let (summary_json, removed_label) = if removed_count > 0 {
                    let start = sys_count.min(snapshot_before.len());
                    let end = (sys_count + removed_count).min(snapshot_before.len());
                    (
                        &snapshot_before[start..end],
                        format!("{} messages removed", removed_count),
                    )
                } else {
                    // Only content trimming happened (Phase 1/2), no messages
                    // were actually dropped. Don't summarize messages that are
                    // still in the conversation — pass empty to avoid duplicating
                    // context with a summary alongside the originals.
                    (
                        &snapshot_before[..0],
                        format!(
                            "{} messages trimmed ({saved_tokens} tokens saved)",
                            before_count
                        ),
                    )
                };

                let template =
                    crate::compress::build_compression_summary(summary_json, &config.working_dir);
                let mut full_prompt = crate::compress::build_compression_prompt(summary_json);
                // Feed the FULL removed window to the summary model.
                // Truncating to `summary_max_prompt_chars` here used to drop
                // the middle ~90% of the removed context (head+tail only),
                // so the generated summary permanently lost the execution
                // history (file edits, rejected directions, verification
                // results) that later turns rely on. `spawn_summary` already
                // splits oversized prompts via `progressive_merge_summary`
                // (7K-char chunks, pairwise serial merges), which is slower
                // but keeps every byte visible to the summarizer. The wait
                // window (`summary_wait_secs`) must cover the merge cost:
                // measured ≈46s for a 170K-token window, hence 60s.

                // P1-h: tell the UI compression STARTED before blocking on
                // the summary model (up to 60s of otherwise-silent wait).
                // after_tokens=0 is the pending marker; the final event
                // below overwrites it with real numbers.
                if removed_count > 0 {
                    if let Some(ref tx) = config.event_tx {
                        let _ = tx.send(oz_core_types::StreamEvent::DataCompressingContext {
                            before_tokens: est_tokens,
                            after_tokens: 0,
                            saved_tokens: 0,
                        });
                    }
                }

                let previous = crate::compress::extract_compression_summaries(&mut messages);
                if !previous.is_empty() {
                    full_prompt = format!(
                        "[Prior context (merge into summary below)]:\n{previous}\n\n---\n\n{full_prompt}"
                    );
                }

                // ── P1: Fire LLM summary only when messages were
                // actually dropped. Pure trimming (Phase 1/2) keeps the
                // content in the conversation — summarizing it would
                // duplicate context for zero information gain.
                let summary_rx = if removed_count > 0 && compression_service.is_configured() {
                    let full_prompt_len = full_prompt.len();
                    let rx = compression_service.spawn_summary(full_prompt, template.clone());
                    tracing::info!(
                        "Fired LLM compression summary for {} (full prompt ~{} chars)",
                        removed_label,
                        full_prompt_len
                    );
                    Some(rx)
                } else {
                    if removed_count == 0 {
                        tracing::debug!(
                            "No messages removed — skipping LLM summary (nothing lost)"
                        );
                    } else {
                        tracing::warn!(
                            "No summary model configured (summary_model_name is None); \
                             skipping LLM summary for {}",
                            removed_label
                        );
                    }
                    None
                };

                // When messages were dropped, wait (bounded) for the
                // REAL summary so the main model's prefill runs against
                // it instead of the template. The template is the
                // terminal fallback on timeout — it is never replaced
                // by the late summary on a later turn, so each
                // compression changes the injected prefix exactly once.
                let summary_text = if removed_count > 0 {
                    wait_for_summary(summary_rx, template, comp_config.summary_wait_secs).await
                } else {
                    template
                };

                if !summary_text.is_empty() {
                    let inject_at = messages
                        .iter()
                        .position(|m| {
                            m.role == oz_core_types::Role::User
                                || m.role == oz_core_types::Role::Assistant
                        })
                        .unwrap_or(0);
                    messages.insert(
                        inject_at,
                        Message::system(format!("[Compression summary]: {summary_text}")),
                    );
                }
            }
        }

        // ── Direction B: FSM transition — Idle → Thinking ──
        transition_state(handler, AgentState::Thinking, "starting LLM call");

        // ── Direction A: Speculative pre-execution cache ──
        // Stores results of tool calls speculatively dispatched while the LLM
        // is still streaming. Each entry maps tool_call_id → outcome.
        let spec_cache: Arc<
            std::sync::Mutex<std::collections::HashMap<String, Result<StepOutcome, ToolError>>>,
        > = Arc::new(std::sync::Mutex::new(std::collections::HashMap::new()));

        let response = match &config.event_tx {
            Some(tx) => {
                let handler_ref: &dyn Handler = &*handler;
                let cache = spec_cache.clone();

                let cancel_signal = stop_signal;
                let cancel_fut = async move {
                    loop {
                        if cancel_signal.load(Ordering::Relaxed) {
                            break;
                        }
                        tokio::time::sleep(Duration::from_millis(100)).await;
                    }
                };
                tokio::pin!(cancel_fut);

                let timeout_secs = if config.stream_timeout_secs > 0 {
                    config.stream_timeout_secs
                } else {
                    300
                };

                // Outer retry loop: a wedged LLM transport (timeout or
                // stream error) retries the same turn up to
                // max_llm_error_retries instead of terminating the whole
                // long task. Each attempt rebuilds the stream future.
                // `pending_spec` collects guard-approved tool calls that
                // arrived while the model was still streaming; entries
                // from a failed attempt are dropped on retry so a tool the
                // final response never references is never executed.
                let mut pending_spec: Vec<(String, String, serde_json::Value)> = Vec::new();
                let result: Result<MockResponse, oz_core_types::LlmError> = loop {
                    let (spec_tx, mut spec_rx) = tokio::sync::mpsc::unbounded_channel();
                    let stream_fut =
                        client.stream_chat(&messages, tools, tx.clone(), Some(spec_tx));
                    tokio::pin!(stream_fut);

                    // Real stall detection lives inside the oz-llm parsers
                    // (per-chunk timeout: 60s cloud / 300s local). This outer
                    // bound is only a true-hang fallback — it must NOT fire
                    // while a slow model keeps making progress, so it is 4x
                    // the configured window with a 1h floor instead of a
                    // fixed total duration.
                    let hang_timeout_secs = timeout_secs.saturating_mul(4).max(3600);
                    let stream_hang_timeout =
                        tokio::time::sleep(Duration::from_secs(hang_timeout_secs));
                    tokio::pin!(stream_hang_timeout);

                    let attempt_result = loop {
                        tokio::select! {
                            _ = &mut cancel_fut => {
                                tracing::info!("Stop signal received during LLM stream, saving checkpoint and aborting");
                                save_stop_checkpoint_async(config, turn, "stopped_by_user", &messages, &history_info, &full_response, &full_thinking, &handler.working().todos).await;
                                transition_state(handler, AgentState::Done("stopped_by_user".into()), "stop signal during LLM stream");
                                return LoopOutcome {
                                    turn,
                                    exit_reason: "stopped_by_user".into(),
                                    data: Some(serde_json::json!({
                                        "full_response": full_response.clone(),
                                        "input_tokens_est": total_input_tokens,
                                        "output_tokens_est": total_output_tokens,
                                        "context_tokens_est": last_turn_input_tokens,
                                    })),
                                };
                            }
                            maybe_ready = spec_rx.recv() => {
                                if let Some(StreamEvent::ToolCallReady { id, name, args }) = maybe_ready {
                                    // Round3 P0-B: only strictly read-only tools with a
                                    // provider-issued id may pre-execute. An empty id would
                                    // never match Phase 2's cache lookup (keyed by tid) and
                                    // would execute twice; side-effectful or breaker-throttled
                                    // tools must wait for the full Phase 2 path.
                                    let spec_eligible = !id.is_empty()
                                        && SPECULATIVE_READ_ONLY_TOOLS.contains(&name.as_str());
                                    if name != "respond" && spec_eligible {
                                        let cache_id = id.clone();
                                        let tc_id = id;
                                        let _ = tx.send(StreamEvent::ToolInputStart {
                                            tool_call_id: tc_id.clone(),
                                            name: name.clone(),
                                        });
                                        let _ = tx.send(StreamEvent::ToolInputAvailable {
                                            tool_call_id: tc_id.clone(),
                                            name: name.clone(),
                                            args: args.clone(),
                                        });
                                        if let Ok(parsed) = serde_json::from_str::<serde_json::Value>(&args) {
                                            // Gate speculative execution with the
                                            // exact same guard Phase 2 applies —
                                            // tools needing approval or blocked are
                                            // left for Phase 2 instead of running
                                            // ahead of the user's decision.
                                            let guard_ok = match (&config.safety_guard, &config.approval_handler) {
                                                (Some(guard), Some(_)) => matches!(
                                                    guard.check(&name, &parsed),
                                                    oz_safety::TrustDecision::Allowed
                                                ),
                                                _ => true,
                                            };
                                            if guard_ok && breaker.check(&name) {
                                                pending_spec.push((cache_id, name, parsed));
                                            }
                                        }
                                    }
                                }
                            }
                            _ = &mut stream_hang_timeout => {
                                tracing::warn!("LLM stream produced no terminal event for {}s", hang_timeout_secs);
                                save_stop_checkpoint_async(config, turn, "llm_timeout", &messages, &history_info, &full_response, &full_thinking, &handler.working().todos).await;
                                break Err(oz_core_types::LlmError::StreamError(format!(
                                    "stream timed out after {hang_timeout_secs}s"
                                )));
                            }
                            stream_result = &mut stream_fut => break stream_result,
                        }
                    };
                    match attempt_result {
                        Ok(resp) => {
                            // Transport recovered — reset so one bad stretch
                            // mid-task can't kill a long 7x24 run later.
                            consecutive_llm_errors = 0;
                            break Ok(resp);
                        }
                        Err(e) => {
                            tracing::error!("LLM stream chat error: {e}");
                            consecutive_llm_errors += 1;
                            let is_timeout = matches!(
                                &e,
                                oz_core_types::LlmError::StreamError(m) if m.contains("timed out")
                            );
                            if consecutive_llm_errors > max_llm_error_retries {
                                let exit_reason = if is_timeout {
                                    "llm_timeout"
                                } else {
                                    "llm_error"
                                };
                                transition_state(
                                    handler,
                                    AgentState::Done(exit_reason.into()),
                                    "LLM stream error",
                                );
                                return LoopOutcome {
                                    turn,
                                    exit_reason: exit_reason.into(),
                                    data: Some(serde_json::json!({
                                        "error": format!("{e} ({} consecutive)", consecutive_llm_errors),
                                    })),
                                };
                            }
                            agent_log(&format!(
                                "LLM stream {} (attempt {consecutive_llm_errors}/{max_llm_error_retries}), retrying turn {turn}: {e}",
                                if is_timeout { "timeout" } else { "error" }
                            ));
                            // Drop tool calls queued by the failed attempt —
                            // the retry's response is the source of truth.
                            pending_spec.clear();
                            backoff_or_stop(stop_signal, consecutive_llm_errors).await;
                        }
                    }
                };
                let response = match result {
                    Ok(resp) => resp,
                    Err(e) => {
                        tracing::error!("LLM stream chat error: {e}");
                        transition_state(
                            handler,
                            AgentState::Done("llm_error".into()),
                            "LLM stream error",
                        );
                        return LoopOutcome {
                            turn,
                            exit_reason: "llm_error".into(),
                            data: Some(serde_json::json!({"error": e.to_string()})),
                        };
                    }
                };
                // Speculative pre-execution phase: dispatch the approved read-only
                // tool calls now that the stream finished, so Phase 2 finds
                // their results cached and skips re-execution. A dispatch that
                // misses its 5s budget is recorded as an error in the cache —
                // Phase 2 then reports the failure instead of running the same
                // side effect a second time. All dispatches run concurrently:
                // worst-case wall time is one 5s budget, not 5s × N.
                let empty = MockResponse::new("");
                let empty_ref = &empty;
                let dispatch_futs =
                    pending_spec
                        .into_iter()
                        .map(|(cache_id, name, parsed)| async move {
                            let outcome = match tokio::time::timeout(
                                Duration::from_secs(5),
                                handler_ref.dispatch(&name, parsed, empty_ref, 0, ctx),
                            )
                            .await
                            {
                                Ok(res) => res,
                                Err(_) => Err(ToolError::Custom(
                                    "speculative execution timed out; skipped".into(),
                                )),
                            };
                            (cache_id, outcome)
                        });
                for (cache_id, outcome) in futures::future::join_all(dispatch_futs).await {
                    cache
                        .lock()
                        .unwrap_or_else(|p| p.into_inner())
                        .insert(cache_id, outcome);
                }
                response
            }
            None => {
                // Same retry/backoff semantics as the streaming path: a
                // single transient failure used to terminate the whole
                // run here.
                let chat_resp = loop {
                    match client.chat(&messages, tools).await {
                        Ok(resp) => {
                            consecutive_llm_errors = 0;
                            break resp;
                        }
                        Err(e) => {
                            tracing::error!("LLM chat error: {e}");
                            consecutive_llm_errors += 1;
                            if consecutive_llm_errors > max_llm_error_retries {
                                transition_state(
                                    handler,
                                    AgentState::Done("llm_error".into()),
                                    "non-streaming LLM error",
                                );
                                return LoopOutcome {
                                    turn,
                                    exit_reason: "llm_error".into(),
                                    data: Some(serde_json::json!({
                                        "error": format!("{e} ({consecutive_llm_errors} consecutive)"),
                                    })),
                                };
                            }
                            tracing::warn!(
                                "LLM chat error (attempt {consecutive_llm_errors}/{max_llm_error_retries}), retrying turn {turn}: {e}"
                            );
                            backoff_or_stop(stop_signal, consecutive_llm_errors).await;
                        }
                    }
                };
                chat_resp
            }
        };

        if !response.thinking.is_empty() {
            if !full_thinking.is_empty() {
                full_thinking.push('\n');
            }
            full_thinking.push_str(&response.thinking);
        }
        let input_chars: usize = messages
            .iter()
            .map(|m| {
                m.content
                    .iter()
                    .map(|b| match b {
                        ContentBlock::Text { text, .. } => text.len(),
                        ContentBlock::ToolUse { name, input, .. } => {
                            name.len() + input.to_string().len()
                        }
                        ContentBlock::ToolResult { content, .. } => {
                            content.as_text().map(|t| t.len()).unwrap_or(0)
                        }
                        ContentBlock::Thinking { thinking, .. } => thinking.len(),
                        ContentBlock::ImageUrl { url, .. } => url.len(),
                    })
                    .sum::<usize>()
            })
            .sum();
        // Prefer real token usage from the LLM provider; fall back to
        // the chars/4 estimate when the provider didn't report usage
        // (e.g. native sessions that wrap a different API).
        if let Some(usage) = &response.usage {
            total_input_tokens += usage.input_tokens;
            total_output_tokens += usage.output_tokens;
            // omlx reports SESSION-CUMULATIVE input_tokens: after
            // client-side compression a ~17K-char payload still reports
            // ~180K tokens (10+ tokens/char is impossible), so the raw
            // value never drops and force-compression fires every turn.
            // Clamp the per-turn estimate against the actual payload
            // size (4 chars/token is a loose upper bound for zh/en mix;
            // real is 0.25-1.5 tokens/char) so a stale cumulative report
            // can't pin est at ~180K forever.
            let chars_bound = (input_chars.saturating_mul(4)).max(2048) as u64;
            last_turn_input_tokens = usage.input_tokens.min(chars_bound);
            last_turn_output_tokens = usage.output_tokens;
        } else {
            let est_input = input_chars as u64 / 4;
            total_input_tokens += est_input;
            let est_output = (response.content.len() as u64 + response.thinking.len() as u64) / 4;
            total_output_tokens += est_output;
            last_turn_input_tokens = est_input;
            last_turn_output_tokens = est_output;
        }
        last_turn_input_chars = input_chars as u64;

        let (clean_content, _model_summary) = strip_summary_tags(&response.content);

        // full_response must hold ONLY the latest turn's visible text.
        // Earlier turns' text is already streamed to the UI as its own
        // text part at its own position in the timeline; accumulating
        // every turn made the final bubble text contain all intermediate
        // replies (duplicated next to the timeline parts).
        if !clean_content.is_empty() {
            full_response = clean_content.clone();
        }
        meter::record_tokens(total_input_tokens, total_output_tokens);

        // Emit real-time context usage so the frontend can update the
        // context-window progress bar after every turn instead of waiting
        // until the entire task finishes.
        if let Some(ref tx) = config.event_tx {
            let _ = tx.send(StreamEvent::DataContextUsage {
                current_tokens: last_turn_input_tokens,
                output_tokens: last_turn_output_tokens,
                context_window: config.context_win,
                turn,
                message_count: messages.len(),
                total_input_tokens,
                total_output_tokens,
            });
        }

        let tool_calls = extract_tool_calls(&response);

        // Reset stall counters when LLM actually calls tools.
        if tool_calls.iter().any(|tc| tc.name != "respond") {
            consecutive_intent_hits = 0;
            consecutive_empty_turns = 0;
        } else {
            consecutive_empty_turns += 1;
        }

        // ── Direction B: FSM transition — Thinking → ToolExecution or Responding ──
        if tool_calls.iter().any(|tc| tc.name != "respond") {
            transition_state(handler, AgentState::ToolExecution, "tool calls detected");
        } else {
            transition_state(handler, AgentState::Responding, "text-only response");
        }

        let tool_calls_iter: Vec<oz_core_types::MockToolCall> = if tool_calls.is_empty() {
            let intent_markers = [
                "I'll read",
                "I will check",
                "let me look",
                "let me search",
                "I'll open",
                "I need to",
                "I should",
                "let's first",
                "我来读",
                "我来看",
                "让我查",
                "我先看看",
                "我先找一下",
                "让我读",
                "我来搜索",
                "让我打开",
            ];
            let has_intent = intent_markers.iter().any(|m| clean_content.contains(m));
            if has_intent && turn < config.max_turns.saturating_sub(1) {
                consecutive_intent_hits += 1;
                // If the LLM keeps expressing intent without calling tools,
                // its context is likely broken — nudge it back instead of
                // compressing (compression is reserved for context > 170K).
                if consecutive_intent_hits >= 3 {
                    tracing::warn!("{consecutive_intent_hits} consecutive intent-only responses — injecting tool-call hint");
                    consecutive_intent_hits = 0;
                }
                let hint = if ctx.lang == "zh" {
                    "[系统提示] 你表达了使用工具的意图但未实际调用。请调用对应的工具函数（如 read、grep、ls、web_search）。"
                } else {
                    "[SYSTEM] You indicated intent to use a tool but didn't call one. Please call the actual tool function now (e.g., read, grep, ls, web_search)."
                };
                // Inject hint for the next turn — push to messages so the model sees it.
                messages.push(Message::user(hint));
                // Fallback for progressive disclosure: the model expressed
                // intent but did not call skill_mcp_search. Local models
                // sometimes never discover the tool; degrade to injecting the
                // matched skill/SOP bodies once (first intent-only turn only)
                // so the intent can proceed.
                if consecutive_intent_hits == 1 {
                    if let Some(ref store_arc) = skill_mcp_store {
                        let matched = {
                            let store = store_arc.lock().await;
                            store
                                .build_context(
                                    &user_input,
                                    std::path::Path::new(&config.working_dir),
                                    None,
                                )
                                .await
                        };
                        if !matched.is_empty() {
                            let mut blocks = Vec::new();
                            blocks.push(ContentBlock::text(format!(
                                "<system-reminder>Detected intent without tool calls — matched skill/SOP context injected below:</system-reminder>\n{matched}"
                            )));
                            messages.push(Message::user_with_blocks(blocks));
                            tracing::info!(
                                "Degraded to full skill/SOP context ({} chars) after intent-only turn",
                                matched.len()
                            );
                        }
                    }
                }
                // Return empty vec — no respond → no exit → loop continues naturally.
                vec![]
            } else {
                vec![oz_core_types::MockToolCall::new(
                    "respond",
                    serde_json::json!({"response": clean_content}),
                )]
            }
        } else {
            // When the LLM explicitly called respond, its `response`
            // argument is the canonical final text. The streamed
            // `response.content` was already appended above; replace
            // it (not append) with the tool arg to avoid showing the
            // reply twice. Models that only stream (no respond call)
            // keep the streamed text as-is.
            let mut respond_override: Option<String> = None;
            for tc in &tool_calls {
                if tc.name == "respond" {
                    if let Some(resp_val) = tc.arguments.get("response").and_then(|v| v.as_str()) {
                        if !resp_val.is_empty() {
                            let (clean_resp, _resp_summary) = strip_summary_tags(resp_val);
                            respond_override = Some(clean_resp);
                        }
                    } else {
                        tracing::warn!(
                            "respond call found but no 'response' field in arguments: {:?}",
                            tc.arguments
                        );
                    }
                }
            }
            if let Some(override_text) = respond_override {
                // The respond tool's `response` argument is the canonical
                // final text. full_response holds only the latest turn's
                // streamed text (we no longer accumulate across turns), so
                // replace it wholesale instead of trimming a last segment.
                full_response = override_text;
            }
            tool_calls
        };

        let mut tool_results: Vec<ToolResultItem> = Vec::new();
        let mut next_prompts: Vec<String> = Vec::new();
        let mut pending_ask_user: Vec<PendingAskUser> = Vec::new();
        // P1 plan approval: plan submitted by submit_plan, awaiting the
        // user's approve/modify decision (consumed in the ask_user pause).
        let mut pending_plan: Option<(String, Vec<String>)> = None;

        // ── Fast path: text-only response, skip parallel tool machinery ──
        let is_text_only = tool_calls_iter.iter().all(|tc| tc.name == "respond");

        // tool_meta is declared here so Todo tracking (after the if/else)
        // can access tool args by name.
        let mut tool_meta: Vec<ToolCallMeta> = Vec::new();

        if is_text_only {
            // Only respond to handle — no parallel execution needed
            for m in &tool_calls_iter {
                let outcome = handler
                    .dispatch(&m.name, m.arguments.clone(), &response, 0, ctx)
                    .await;
                handler.tool_after(&m.name, &m.arguments, outcome.clone());
                process_tool_outcome(
                    outcome,
                    &m.name,
                    &m.id,
                    &m.id,
                    0,
                    &mut tool_results,
                    &mut next_prompts,
                    &mut full_response,
                    &mut exit_reason,
                    config,
                    &mut pending_ask_user,
                );
            }
        } else {
            // ── Parallel tool execution (Phases 1-6) ──
            // Tools already satisfied by the speculative cache skip the
            // breaker tick here — their budget was consumed once when they
            // were queued during the stream (double-counting would halve
            // the effective read-tool budget for speculated calls).
            let spec_cached_ids: std::collections::HashSet<String> = {
                let c = spec_cache.lock().unwrap_or_else(|p| p.into_inner());
                c.keys().cloned().collect()
            };

            // Collect pre-processing info for each tool call
            tool_meta = tool_calls_iter
                .iter()
                .enumerate()
                .map(|(ii, tc)| {
                    let tool_name = tc.name.clone();
                    let args = tc.arguments.clone();
                    let tid = tc.id.clone();
                    // Pre-compute the canonical SSE tc_id so Start, Available, and
                    // OutputAvailable all carry the same value.
                    let tc_id = if tid.is_empty() {
                        next_block_id("tc")
                    } else {
                        tid.clone()
                    };

                    handler.working_mut().sensorium.record_tool(&tool_name);
                    // `respond` is a synthetic wrapper around the LLM's text reply; its args
                    // are the full response body, which would flood the log and leak into
                    // terminal scrollback. Skip the verbose dump for it.
                    if config.verbose && tool_name != "respond" {
                        let args_pretty = serde_json::to_string_pretty(&args).unwrap_or_default();
                        tracing::debug!("Tool: `{tool_name}` args:\n```text\n{args_pretty}\n```");
                    }

                    // Check breaker (skip for spec-cached calls — see above)
                    let already_cached = !tid.is_empty() && spec_cached_ids.contains(&tid);
                    let blocked =
                        tool_name != "respond" && !already_cached && !breaker.check(&tool_name);

                    ToolCallMeta {
                        ii,
                        tool_name,
                        args,
                        tid,
                        tc_id,
                        blocked,
                    }
                })
                .collect();

            // Phase 1: Call tool_before for all executable tools (sequential)
            for m in &tool_meta {
                if !m.blocked && m.tool_name != "respond" {
                    handler.tool_before(&m.tool_name, &m.args);
                    meter::record_tool_call();
                }
            }

            // Phase 2: Execute all non-blocked tools in parallel
            // dispatch() takes &self so we can share the handler reference
            // Each tool gets a timeout; concurrency is limited by Semaphore;
            // if any tool triggers should_exit, remaining tasks are cancelled.
            // The per-run semaphore is topped up by a process-wide cap:
            // concurrent sessions (desktop + IM bridge) would otherwise
            // multiply the limit (8 x N tool subprocesses, unbounded).
            let cancel_flag = Arc::new(AtomicBool::new(false));
            static GLOBAL_TOOL_SEMAPHORE: std::sync::OnceLock<tokio::sync::Semaphore> =
                std::sync::OnceLock::new();
            let global_sem = GLOBAL_TOOL_SEMAPHORE.get_or_init(|| tokio::sync::Semaphore::new(16));
            let semaphore = if config.max_concurrent_tools > 0 {
                Some(Arc::new(tokio::sync::Semaphore::new(
                    config.max_concurrent_tools,
                )))
            } else {
                None
            };

            let parallel_results = {
                let handler_ref: &dyn Handler = &*handler;
                let cancel = cancel_flag.clone();
                let sem = semaphore.clone();
                let global_sem: &tokio::sync::Semaphore = global_sem;

                let spec_cache_for_phase2 = spec_cache.clone();
                let futures: Vec<_> = tool_meta
                    .iter()
                    .filter(|m| !m.blocked && m.tool_name != "respond")
                    .map(|m| {
                        let ii = m.ii;
                        let tool_name = m.tool_name.clone();
                        let args = m.args.clone();
                        let tid = m.tid.clone();
                        tool_sequence.push((tool_name.clone(), args.clone()));
                        if let Some(ref tx) = config.event_tx {
                            let args_str = serde_json::to_string(&args).unwrap_or_default();
                            let tc_id = m.tc_id.clone();
                            let _ = tx.send(StreamEvent::ToolInputStart {
                                tool_call_id: tc_id.clone(),
                                name: tool_name.clone(),
                            });
                            let _ = tx.send(StreamEvent::ToolInputAvailable {
                                tool_call_id: tc_id,
                                name: tool_name.clone(),
                                args: args_str,
                            });
                        }
                        let cancel = cancel.clone();
                        let sem = sem.clone();
                        let resp = &response;
                        let cx = ctx;
                        let cfg = config;
                        let cache = spec_cache_for_phase2.clone();

                        async move {
                            // Check speculative execution cache first (Direction A)
                            if !tid.is_empty() {
                                let cached = cache.lock().unwrap_or_else(std::sync::PoisonError::into_inner).remove(&tid);
                                if let Some(cached_outcome) = cached {
                                    // Tool was already speculatively executed; skip re-dispatch
                                    if let Ok(ref outcome) = cached_outcome {
                                        if outcome.should_exit {
                                            cancel.store(true, Ordering::Relaxed);
                                        }
                                    }
                                    return (ii, tool_name, cached_outcome);
                                }
                            }

                            // Early cancellation check (another tool already requested exit)
                            if cancel.load(Ordering::Relaxed) {
                                return (ii, tool_name, Err(ToolError::Custom("cancelled".into())));
                            }

                            // Acquire concurrency permits (blocks if at
                            // capacity): per-run first, then the global cap
                            // so concurrent sessions share one budget.
                            let _global_permit = match global_sem.acquire().await {
                                Ok(p) => p,
                                Err(_) => {
                                    tracing::warn!(
                                        "Global tool semaphore closed, skipping {}",
                                        tool_name
                                    );
                                    return (
                                        ii,
                                        tool_name.clone(),
                                        Err(ToolError::Custom("semaphore closed".into())),
                                    );
                                }
                            };
                            let _permit = match &sem {
                                Some(s) => match s.acquire().await {
                                    Ok(p) => Some(p),
                                    Err(_) => {
                                        tracing::warn!("Semaphore closed, skipping {}", tool_name);
                                        return (
                                            ii,
                                            tool_name.clone(),
                                            Err(ToolError::Custom("semaphore closed".into())),
                                        );
                                    }
                                },
                                None => None,
                            };

                            // Check cancellation again after acquiring permit
                            if cancel.load(Ordering::Relaxed) {
                                return (
                                    ii,
                                    tool_name.clone(),
                                    Err(ToolError::Custom("cancelled".into())),
                                );
                            }

                            // Safety guard check — progressive trust + blocklist
                            if let (Some(ref guard), Some(ref approval)) =
                                (&cfg.safety_guard, &cfg.approval_handler)
                            {
                                let decision = guard.check(&tool_name, &args);
                                match decision {
                                    oz_safety::TrustDecision::Blocked(msg) => {
                                        tracing::warn!("[safety] blocked {tool_name}: {msg}");
                                        return (
                                            ii,
                                            tool_name.clone(),
                                            Err(ToolError::Custom(format!("blocked: {msg}"))),
                                        );
                                    }
                                    oz_safety::TrustDecision::NeedsApproval(info) => {
                                        tracing::info!(
                                            "[safety] requesting approval for {}/{}",
                                            info.tool_name,
                                            info.pattern
                                        );
                                        let req = oz_safety::ApprovalRequest {
                                            session_id: cfg.session_id.clone(),
                                            tool_name: info.tool_name.clone(),
                                            pattern: info.pattern.clone(),
                                            arguments: args.clone(),
                                            info,
                                        };
                                        let timeout = std::time::Duration::from_secs(
                                            cfg.approval_timeout_secs,
                                        );
                                        // Race the approval wait against the user
                                        // stop signal — a pending approval must not
                                        // leave Stop dead for up to 300s.
                                        let approval_fut = approval.request_approval(req, timeout);
                                        tokio::pin!(approval_fut);
                                        let stop_sig = stop_signal;
                                        let stop_wait = async move {
                                            loop {
                                                if stop_sig.load(Ordering::Relaxed) {
                                                    break;
                                                }
                                                tokio::time::sleep(Duration::from_millis(100))
                                                    .await;
                                            }
                                        };
                                        tokio::pin!(stop_wait);
                                        let approval_result = tokio::select! {
                                            d = &mut approval_fut => d,
                                            _ = &mut stop_wait => {
                                                tracing::info!(
                                                    "[safety] stop signal during approval wait for {tool_name}"
                                                );
                                                return (
                                                    ii,
                                                    tool_name.clone(),
                                                    Err(ToolError::Custom("stopped".into())),
                                                );
                                            }
                                        };
                                        match approval_result {
                                            Ok(oz_safety::ApprovalDecision::Allow) => {
                                                tracing::debug!("[safety] approved {tool_name}");
                                            }
                                            Ok(oz_safety::ApprovalDecision::TrustSession) => {
                                                tracing::info!(
                                                    "[safety] session-trusted {tool_name}"
                                                );
                                                guard.record_approval(&tool_name, &args);
                                            }
                                            Ok(oz_safety::ApprovalDecision::TrustWorkspace) => {
                                                tracing::info!(
                                                    "[safety] workspace-trusted {tool_name}"
                                                );
                                                guard.record_approval(&tool_name, &args);
                                            }
                                            Ok(oz_safety::ApprovalDecision::Deny) => {
                                                tracing::info!("[safety] denied {tool_name}");
                                                return (
                                                    ii,
                                                    tool_name.clone(),
                                                    Err(ToolError::Custom(
                                                        "user denied operation".into(),
                                                    )),
                                                );
                                            }
                                            Ok(oz_safety::ApprovalDecision::BlockForever) => {
                                                tracing::warn!(
                                                    "[safety] permanently blocked {tool_name}"
                                                );
                                                guard.block(&tool_name, &args);
                                                return (
                                                    ii,
                                                    tool_name.clone(),
                                                    Err(ToolError::Custom(
                                                        "user permanently blocked operation".into(),
                                                    )),
                                                );
                                            }
                                            Err(e) => {
                                                tracing::warn!(
                                                    "[safety] approval error for {tool_name}: {e}"
                                                );
                                                return (
                                                    ii,
                                                    tool_name.clone(),
                                                    Err(ToolError::Custom(format!(
                                                        "approval failed: {e}"
                                                    ))),
                                                );
                                            }
                                        }
                                    }
                                    oz_safety::TrustDecision::Allowed => {
                                        // Silent pass — trusted or safe tool
                                    }
                                }
                            }

                            let dispatch_fut =
                                handler_ref.dispatch(&tool_name, args, resp, ii as u32, cx);
                            // Race the dispatch against both the tool timeout AND a
                            // user stop-signal poll, so cancelling the session
                            // actually interrupts an in-flight tool instead of
                            // waiting for its 30s timeout to fire.
                            let stop_poll = cancel.clone();
                            let stop_wait = async move {
                                loop {
                                    if stop_poll.load(Ordering::Relaxed) {
                                        break;
                                    }
                                    tokio::time::sleep(Duration::from_millis(100)).await;
                                }
                            };
                            tokio::pin!(stop_wait);
                            let result = tokio::select! {
                                biased;
                                _ = &mut stop_wait => {
                                    tracing::info!("Tool {} interrupted by user stop", tool_name);
                                    Err(ToolError::Custom("interrupted by user stop".into()))
                                }
                                timed = tokio::time::timeout(
                                    Duration::from_secs(cfg.tool_timeout_secs),
                                    dispatch_fut,
                                ) => match timed {
                                    Ok(outcome) => outcome,
                                    Err(_elapsed) => Err(ToolError::Custom(format!(
                                        "tool '{}' timed out after {}s",
                                        tool_name, cfg.tool_timeout_secs,
                                    ))),
                                }
                            };

                            // If this tool requested exit, signal others to cancel
                            if let Ok(ref outcome) = result {
                                if outcome.should_exit {
                                    cancel.store(true, Ordering::Relaxed);
                                }
                            }

                            (ii, tool_name.clone(), result)
                        }
                    })
                    .collect();
                futures::future::join_all(futures).await
            };

            // Phase 3: Call tool_after for all results (sequential)
            for (ii, ref tool_name, ref outcome) in &parallel_results {
                if let Some(meta) = tool_meta.iter().find(|m| m.ii == *ii) {
                    handler.tool_after(tool_name, &meta.args, outcome.clone());
                    // PostToolUse hooks fire only for successful file-writing
                    // tools, with the target file path for {file} substitution.
                    if outcome.is_ok() && matches!(tool_name.as_str(), "write" | "edit" | "patch") {
                        if let Some(ref hooks) = config.hooks {
                            let file = meta
                                .args
                                .get("file_path")
                                .and_then(|v| v.as_str())
                                .map(|s| s.to_string());
                            hooks.fire(&crate::hooks::HookEvent::PostToolUse {
                                tool: tool_name.clone(),
                                file,
                            });
                        }
                    }
                }
            }

            // Phase 4: Handle blocked (breaker) tools
            for m in &tool_meta {
                if m.blocked {
                    let msg = BREAKER_BLOCKED_MSG.replace("{tool}", &m.tool_name);
                    next_prompts.push(msg.clone());
                    if !m.tid.is_empty() {
                        tool_results.push(ToolResultItem {
                            tool_use_id: m.tid.clone(),
                            content: msg,
                            images: vec![],
                        });
                    }
                }
            }

            // Phase 5: Handle respond (always sequential, single)
            // respond events are NOT sent to frontends — internal mechanism only.
            // We dispatch and record the side effect, but we intentionally do NOT
            // call process_tool_outcome for respond here. Doing so would set
            // exit_reason="EXITED" and break the loop, even when the LLM called
            // respond alongside other tools in a multi-step task. The "I'm done"
            // signal is only honoured in the is_text_only branch above, where
            // respond is the sole tool call.
            for m in &tool_meta {
                if m.tool_name == "respond" && !m.blocked {
                    let outcome = handler
                        .dispatch(&m.tool_name, m.args.clone(), &response, m.ii as u32, ctx)
                        .await;
                    handler.tool_after(&m.tool_name, &m.args, outcome.clone());
                }
            }

            // Phase 6: Process parallel results in order.
            // Two-pass: send every ToolOutputAvailable event first so the UI never sees
            // a stuck "Running..." card, then apply outcomes (which may set exit_reason).
            // Third (inline): extract todowrite todo_ids so the Todo tracking section
            // uses the same UUID-based IDs that the tool returns to the LLM — without
            // this, the agent loop's sequential IDs (todo_1, todo_2) never match the
            // UUID-based IDs the LLM passes to todoupdate, and status updates silently fail.
            let mut todo_write_ids: std::collections::HashMap<usize, String> =
                std::collections::HashMap::new();
            for (_ii, tool_name, ref outcome) in &parallel_results {
                if tool_name == "todowrite" {
                    if let Ok(oc) = outcome {
                        if let Some(id) = oc.data.get("todo_id").and_then(|v| v.as_str()) {
                            todo_write_ids.insert(*_ii, id.to_string());
                        }
                    }
                }
                if let Some(ref tx) = config.event_tx {
                    let result_str = match outcome {
                        Ok(oc) => {
                            let data = &oc.data;
                            // For todoupdate, ensure content is in the result
                            // even if the LLM didn't pass it — look it up from
                            // working memory so the frontend card shows text.
                            let enriched = if tool_name == "todoupdate" {
                                let mut d = data.clone();
                                if d.get("content")
                                    .and_then(|v| v.as_str())
                                    .is_none_or(|s| s.is_empty())
                                {
                                    if let Some(id) = d.get("todo_id").and_then(|v| v.as_str()) {
                                        let wm = handler.working();
                                        if let Some(t) = wm.todos.iter().find(|t| t.id == id) {
                                            d["content"] =
                                                serde_json::Value::String(t.content.clone());
                                        }
                                    }
                                }
                                serde_json::to_string(&d).unwrap_or_default()
                            } else {
                                serde_json::to_string(data).unwrap_or_default()
                            };
                            enriched
                        }
                        Err(e) => format!("{{\"error\":\"{}\"}}", e),
                    };
                    let tc_id = tool_meta
                        .iter()
                        .find(|m| m.ii == *_ii)
                        .map(|m| m.tc_id.clone())
                        .unwrap_or_else(|| next_block_id("tc"));
                    const MAX_TOOL_OUTPUT_IN_STREAM: usize = 32 * 1024;
                    let output_for_stream =
                        truncate_stream_output(&result_str, MAX_TOOL_OUTPUT_IN_STREAM);
                    let _ = tx.send(StreamEvent::ToolOutputAvailable {
                        tool_call_id: tc_id,
                        name: tool_name.clone(),
                        output: output_for_stream,
                    });

                    // open_side_panel → tell the frontend to open the sidebar artifact
                    if tool_name == "open_side_panel" {
                        if let Ok(oc) = outcome {
                            if oc.data.get("status").and_then(|v| v.as_str()) == Some("OPENED") {
                                let artifact_type = oc
                                    .data
                                    .get("artifact_type")
                                    .and_then(|v| v.as_str())
                                    .unwrap_or("")
                                    .to_string();
                                let artifact_path = oc
                                    .data
                                    .get("artifact_path")
                                    .and_then(|v| v.as_str())
                                    .unwrap_or("")
                                    .to_string();
                                let artifact_label = oc
                                    .data
                                    .get("artifact_label")
                                    .and_then(|v| v.as_str())
                                    .unwrap_or("")
                                    .to_string();
                                if !artifact_type.is_empty() && !artifact_path.is_empty() {
                                    let _ = tx.send(StreamEvent::OpenArtifact {
                                        artifact_type,
                                        artifact_path,
                                        artifact_label,
                                    });
                                }
                            }
                        }
                    }
                }
            }
            for (ii, tool_name, outcome) in &parallel_results {
                let meta = &tool_meta[*ii];
                process_tool_outcome(
                    outcome.clone(),
                    tool_name,
                    &meta.tid,
                    &meta.tc_id,
                    *ii,
                    &mut tool_results,
                    &mut next_prompts,
                    &mut full_response,
                    &mut exit_reason,
                    config,
                    &mut pending_ask_user,
                );
            }

            // ── Todo tracking ──
            {
                let mut dirty = false;
                let wm = handler.working_mut();
                for m in &tool_meta {
                    if m.tool_name == "todowrite" {
                        let content = m
                            .args
                            .get("content")
                            .and_then(|v| v.as_str())
                            .unwrap_or("")
                            .to_string();
                        let priority = m
                            .args
                            .get("priority")
                            .and_then(|v| v.as_str())
                            .unwrap_or("medium")
                            .to_string();
                        let id = todo_write_ids.get(&m.ii).cloned().unwrap_or_else(|| {
                            format!(
                                "todo_{}",
                                uuid::Uuid::new_v4()
                                    .to_string()
                                    .split('-')
                                    .next()
                                    .unwrap_or("0")
                            )
                        });
                        if !content.is_empty() {
                            let normalized = content.trim().to_lowercase();
                            let is_duplicate = wm
                                .todos
                                .iter()
                                .any(|t| t.content.trim().to_lowercase() == normalized);
                            if !is_duplicate {
                                wm.todos.push(oz_core_types::TodoItem {
                                    id,
                                    content,
                                    status: "pending".into(),
                                    priority,
                                    order: wm.todos.len(),
                                    in_progress_since_turn: None,
                                });
                                dirty = true;
                            }
                        }
                    } else if m.tool_name == "submit_plan" {
                        // P1 plan state machine with HUMAN APPROVAL: the plan is
                        // staged and the user is asked to approve/modify it via
                        // the ask_user dialog; todos are created only after
                        // approval (in the ask_user pause section below).
                        let goal = m
                            .args
                            .get("goal")
                            .and_then(|v| v.as_str())
                            .unwrap_or("")
                            .to_string();
                        let steps: Vec<String> = m
                            .args
                            .get("steps")
                            .and_then(|v| v.as_array())
                            .map(|arr| {
                                arr.iter()
                                    .filter_map(|v| v.as_str().map(|s| s.to_string()))
                                    .filter(|s| !s.trim().is_empty())
                                    .collect()
                            })
                            .unwrap_or_default();
                        if !goal.is_empty() && !steps.is_empty() {
                            pending_plan = Some((goal.clone(), steps.clone()));
                            pending_ask_user.push(PendingAskUser {
                                tool_use_id: m.tid.clone(),
                                tool_call_id: m.tc_id.clone(),
                                tool_name: "submit_plan".to_string(),
                                payload: serde_json::json!({
                                    "data": {
                                        "question": if ctx.lang == "zh" {
                                            format!(
                                                "Agent 提交了执行计划（{} 步）：{}。确认开始执行吗？",
                                                steps.len(),
                                                goal
                                            )
                                        } else {
                                            format!(
                                                "Agent submitted a plan ({} steps): {}. Approve to start?",
                                                steps.len(),
                                                goal
                                            )
                                        },
                                        "candidates": if ctx.lang == "zh" {
                                            ["确认，开始执行", "修改计划"]
                                        } else {
                                            ["Approve", "Modify plan"]
                                        },
                                    }
                                }),
                            });
                        }
                    } else if m.tool_name == "todoupdate" {
                        let id = m
                            .args
                            .get("id")
                            .and_then(|v| v.as_str())
                            .unwrap_or("")
                            .to_string();
                        let status = m
                            .args
                            .get("status")
                            .and_then(|v| v.as_str())
                            .unwrap_or("in_progress")
                            .to_string();
                        if !id.is_empty() {
                            if let Some(t) = wm.todos.iter_mut().find(|t| t.id == id) {
                                if status == "completed" {
                                    match crate::verifier::verify_todo_item(
                                        &t.content,
                                        &config.working_dir,
                                    )
                                    .await
                                    {
                                        crate::verifier::VerifyResult::Failed(reason) => {
                                            let fail_count =
                                                verify_fail_counts.entry(id.clone()).or_insert(0);
                                            *fail_count += 1;
                                            if *fail_count >= 2 {
                                                // Safety valve: after repeated verification failures,
                                                // accept the completion (with a warning) instead of
                                                // reverting forever — a false-negative verifier would
                                                // otherwise trap the agent in an infinite
                                                // todoupdate → revert → retry loop.
                                                t.in_progress_since_turn = None;
                                                t.status = "completed".to_string();
                                                let msg = if ctx.lang == "zh" {
                                                    format!(
                                                    "[验证放行] \"{}\" 标记完成但验证连续失败（{}）。已接受完成状态，如确有问题请后续修复。",
                                                    t.content, reason
                                                )
                                                } else {
                                                    format!(
                                                    "[VERIFY OVERRIDE] \"{}\" marked complete but verification failed repeatedly ({}). Accepted as done; fix later if needed.",
                                                    t.content, reason
                                                )
                                                };
                                                next_prompts.push(msg);
                                                dirty = true;
                                            } else {
                                                t.status = "in_progress".to_string();
                                                let msg = if ctx.lang == "zh" {
                                                    format!(
                                                    "[验证失败] \"{}\" 标记完成但验证失败：{}。请修复后重试。",
                                                    t.content, reason
                                                )
                                                } else {
                                                    format!(
                                                    "[VERIFY FAILED] \"{}\" was marked complete but verification failed: {}. Fix and re-verify.",
                                                    t.content, reason
                                                )
                                                };
                                                next_prompts.push(msg);
                                                dirty = true;
                                            }
                                        }
                                        crate::verifier::VerifyResult::Passed
                                        | crate::verifier::VerifyResult::SoftPass => {
                                            verify_fail_counts.remove(&id);
                                            t.in_progress_since_turn = None;
                                            t.status = status;
                                            dirty = true;
                                        }
                                    }
                                } else {
                                    verify_fail_counts.remove(&id);
                                    let is_in_progress = status == "in_progress";
                                    t.status = status;
                                    dirty = true;
                                    if is_in_progress {
                                        t.in_progress_since_turn = Some(turn);
                                    } else {
                                        t.in_progress_since_turn = None;
                                    }
                                }
                            } else {
                                let existing: Vec<String> =
                                    wm.todos.iter().map(|t| t.id.clone()).collect();
                                let msg = if ctx.lang == "zh" {
                                    format!(
                                    "[todoupdate] 未找到 id={id} 的待办项。当前列表: [{}]。请使用 todowrite 返回的正确 todo_id。",
                                    existing.join(", ")
                                )
                                } else {
                                    format!(
                                    "[todoupdate] No todo found with id={id}. Current IDs: [{}]. Use the exact todo_id returned by todowrite.",
                                    existing.join(", ")
                                )
                                };
                                next_prompts.push(msg);
                            }
                        }
                    }
                }
                if dirty {
                    let items = wm.todos.clone();
                    let total = items.len();
                    let current = items.iter().filter(|t| t.status == "completed").count();
                    if let Some(ref tx) = config.event_tx {
                        let _ = tx.send(oz_core_types::StreamEvent::DataTodoUpdate {
                            items,
                            current,
                            total,
                        });
                    }
                }

                // ── Status summary: inform the LLM about current todo state ──
                // Injects only when the todo list changed or every 5 turns.
                // Lists every item with status + stall info for in_progress items.
                // No automatic promotion, completion, or blocking — the LLM decides.
                {
                    let current_snapshot: String = wm
                        .todos
                        .iter()
                        .map(|t| format!("{}|{}", t.id, t.status))
                        .collect::<Vec<_>>()
                        .join(",");
                    let changed = current_snapshot != last_todo_snapshot;
                    let periodic = turn.saturating_sub(last_todo_summary_turn) >= 5;
                    if changed || periodic {
                        last_todo_snapshot = current_snapshot;
                        last_todo_summary_turn = turn;
                        let items: Vec<String> = wm
                            .todos
                            .iter()
                            .map(|t| {
                                let stall = if t.status == "in_progress" {
                                    t.in_progress_since_turn
                                        .map(|s| format!(" ({} turns)", turn.saturating_sub(s)))
                                        .unwrap_or_default()
                                } else {
                                    String::new()
                                };
                                let status_label = match t.status.as_str() {
                                    "completed" => {
                                        if ctx.lang == "zh" {
                                            "✅完成"
                                        } else {
                                            "✅done"
                                        }
                                    }
                                    "in_progress" => {
                                        if ctx.lang == "zh" {
                                            "⏳进行中"
                                        } else {
                                            "⏳in-progress"
                                        }
                                    }
                                    "pending" => {
                                        if ctx.lang == "zh" {
                                            "📋待处理"
                                        } else {
                                            "📋pending"
                                        }
                                    }
                                    "cancelled" => {
                                        if ctx.lang == "zh" {
                                            "❌已取消"
                                        } else {
                                            "❌cancelled"
                                        }
                                    }
                                    _ => &t.status,
                                };
                                format!("  [{}] {} {}{}", t.id, status_label, t.content, stall)
                            })
                            .collect();
                        let (completed, total) = (
                            wm.todos.iter().filter(|t| t.status == "completed").count(),
                            wm.todos.len(),
                        );
                        let hint = if ctx.lang == "zh" {
                            format!(
                            "[待办状态] {}/{}\n{}\n\n请根据进度自行调用 todoupdate 更新状态。全部完成后调用 respond 退出。",
                            completed, total, items.join("\n")
                        )
                        } else {
                            format!(
                            "[TODO STATUS] {}/{}\n{}\n\nUpdate status via todoupdate as tasks progress. Call respond when ALL done.",
                            completed, total, items.join("\n")
                        )
                        };
                        next_prompts.push(hint);
                    }
                }

                // Auto-complete disabled: file operations do not mean a task is
                // finished. The agent must explicitly call todoupdate(id, "completed")
                // when each todo is verified complete.
                if dirty {
                    let items = wm.todos.clone();
                    let total = items.len();
                    let current = items.iter().filter(|t| t.status == "completed").count();
                    if let Some(ref tx) = config.event_tx {
                        let _ = tx.send(oz_core_types::StreamEvent::DataTodoUpdate {
                            items,
                            current,
                            total,
                        });
                    }
                }
            }
        } // end else (non-fast-path: parallel tool execution)

        // ── In-turn quick verification (P2-10) ──
        // After a write/edit turn, run a fast check (cargo check for Rust
        // workspaces) and feed failures back immediately — environment as
        // ground truth, not just exit-time acceptance.
        if config.quality_gates && next_prompts.is_empty() {
            let tool_names: Vec<String> = tool_meta.iter().map(|m| m.tool_name.clone()).collect();
            if let Some(check_fb) = crate::quality::quick_verify_after_write(
                &tool_names,
                &config.working_dir,
                &config.lang,
            )
            .await
            {
                next_prompts.push(check_fb);
                if exit_reason.is_some() {
                    exit_reason = None;
                    transition_state(
                        handler,
                        AgentState::Thinking,
                        "quick check: write needs fix",
                    );
                }
            }
        }

        // ── ask_user pause ──────────────────────────────────────
        // The user's reply is a tool_result for the same tool_use id,
        // not a brand-new user message — the LLM resumes the same run.
        // Multiple ask_user calls in one turn are answered in order, so
        // every tool_use gets its own tool_result instead of the last
        // one silently overwriting the rest.
        while !pending_ask_user.is_empty() {
            let pending = pending_ask_user.remove(0);
            // Broadcast the prompt BEFORE waiting so the UI shows the
            // dialog as soon as the tool finishes, not only after reply.
            if let Some(ref tx) = config.event_tx {
                let q_payload = serde_json::json!({
                    "tool_use_id": pending.tool_use_id,
                    "tool_name": pending.tool_name,
                    "payload": pending.payload,
                });
                let _ = tx.send(StreamEvent::AskUserPending {
                    data: serde_json::to_string(&q_payload).unwrap_or_default(),
                });
            }

            // Wait for the user's reply. When ask_user_rx isn't wired
            // (TUI / CLI) we fall back to reading from stdin so the
            // existing TUI ask_user flow still works.
            // Timeout (5 min) prevents infinite blocking if the frontend
            // disconnects without responding — the agent loop continues
            // with an empty reply rather than wedging the session.
            const ASK_USER_TIMEOUT_SECS: u64 = 300;
            let user_reply: String = if let Some(rx_arc) = &config.ask_user_rx {
                // P1-i: replies are keyed by tool_use_id. Discard a stale
                // reply for THIS question, then wait on exactly this key —
                // a late answer to an earlier (timed-out) question can no
                // longer be eaten by the next one. Writers that cannot
                // supply an id land under `__last__`, consumed only after
                // our own key misses.
                {
                    let mut guard = rx_arc.lock().unwrap_or_else(|p| p.into_inner());
                    guard.remove(&pending.tool_use_id);
                }
                let rx = std::sync::Arc::clone(rx_arc);
                let qid = pending.tool_use_id.clone();
                let legacy_key = LEGACY_ASK_USER_KEY.to_string();
                let wait_fut = async move {
                    loop {
                        let reply = {
                            let mut guard = rx.lock().unwrap_or_else(|p| p.into_inner());
                            guard.remove(&qid).or_else(|| guard.remove(&legacy_key))
                        };
                        if let Some(reply) = reply {
                            return Some(reply);
                        }
                        if stop_signal.load(Ordering::Relaxed) {
                            return None;
                        }
                        tokio::time::sleep(Duration::from_millis(100)).await;
                    }
                };
                match tokio::time::timeout(Duration::from_secs(ASK_USER_TIMEOUT_SECS), wait_fut)
                    .await
                {
                    Ok(Some(reply)) => reply,
                    Ok(None) => {
                        exit_reason.replace("stopped_by_user".to_string());
                        String::new()
                    }
                    Err(_elapsed) => {
                        tracing::warn!(
                            "ask_user timed out after {}s — continuing with empty reply",
                            ASK_USER_TIMEOUT_SECS
                        );
                        String::new()
                    }
                }
            } else {
                use std::io::{self, BufRead, Write};
                eprintln!("\n[ask_user] {}", pending.payload["data"]["question"]);
                if let Some(cands) = pending.payload["data"]["candidates"].as_array() {
                    for (i, c) in cands.iter().enumerate() {
                        eprintln!("  {}. {}", i + 1, c.as_str().unwrap_or(""));
                    }
                }
                eprint!("> ");
                let _ = io::stderr().flush();
                let mut line = String::new();
                let stdin = io::stdin();
                stdin.lock().read_line(&mut line).unwrap_or_default();
                line.trim().to_string()
            };

            if exit_reason.is_some() {
                save_stop_checkpoint_async(
                    config,
                    turn,
                    "stopped_by_user",
                    &messages,
                    &history_info,
                    &full_response,
                    &full_thinking,
                    &handler.working().todos,
                )
                .await;
                transition_state(
                    handler,
                    AgentState::Done(exit_reason.clone().unwrap()),
                    "stopped while waiting for ask_user",
                );
                break 'turn;
            }

            // submit_plan approval gate: approve → create todos + plan
            // marker; modify → feed the user's feedback back for a re-plan.
            // Empty reply (timeout / dismiss) counts as approval so the
            // task is never blocked by a missing decision.
            if pending.tool_name == "submit_plan" {
                if let Some((goal, steps)) = pending_plan.take() {
                    let approved = user_reply.trim().is_empty()
                        || ["确认", "开始", "同意", "approve", "yes", "ok", "y", "执行"]
                            .iter()
                            .any(|k| user_reply.to_lowercase().contains(k));
                    if approved {
                        let wm = handler.working_mut();
                        if wm.in_plan_mode.is_none() {
                            wm.in_plan_mode = Some(goal.clone());
                        }
                        for content in &steps {
                            let normalized = content.trim().to_lowercase();
                            let is_duplicate = wm
                                .todos
                                .iter()
                                .any(|t| t.content.trim().to_lowercase() == normalized);
                            if !is_duplicate {
                                let id = format!(
                                    "todo_{}",
                                    uuid::Uuid::new_v4()
                                        .to_string()
                                        .split('-')
                                        .next()
                                        .unwrap_or("0")
                                );
                                wm.todos.push(oz_core_types::TodoItem {
                                    id,
                                    content: content.clone(),
                                    status: "pending".into(),
                                    priority: "medium".into(),
                                    order: wm.todos.len(),
                                    in_progress_since_turn: None,
                                });
                            }
                        }
                        next_prompts.push(if ctx.lang == "zh" {
                            "计划已确认。按步骤执行，每步用 todoupdate 标记状态，清单全 completed 后 respond。".to_string()
                        } else {
                            "Plan approved. Execute the steps, mark each with todoupdate, and respond only when ALL are completed.".to_string()
                        });
                    } else {
                        next_prompts.push(if ctx.lang == "zh" {
                            format!(
                                "用户未批准计划，反馈：\"{}\"。请根据反馈修改计划（重新调用 submit_plan）或调整步骤后继续。",
                                truncate_feedback(&user_reply)
                            )
                        } else {
                            format!(
                                "The user did not approve the plan. Feedback: \"{}\". Revise the plan (call submit_plan again) or adjust the steps and continue.",
                                truncate_feedback(&user_reply)
                            )
                        });
                    }
                }
                // submit_plan replies are consumed as plan feedback; no
                // USER_REPLIED tool result is needed (the plan is not a
                // tool the model's turn depends on).
            } else {
                // Re-emit ToolOutputAvailable so the frontend flips the
                // AskUser card from "Running" to "Done" before the LLM
                // resumes streaming.
                tool_results.push(ToolResultItem {
                    tool_use_id: pending.tool_use_id.clone(),
                    content: serde_json::json!({
                        "status": "USER_REPLIED",
                        "user_reply": user_reply,
                    })
                    .to_string(),
                    images: vec![],
                });
                if let Some(ref tx) = config.event_tx {
                    let _ = tx.send(StreamEvent::ToolOutputAvailable {
                        tool_call_id: pending.tool_call_id.clone(),
                        name: pending.tool_name.clone(),
                        output: serde_json::json!({
                            "status": "USER_REPLIED",
                            "user_reply": user_reply,
                        })
                        .to_string(),
                    });
                }

                // Required so the post-tool `next_prompts.is_empty()` check
                // below does not short-circuit with CURRENT_TASK_DONE — the LLM
                // must be told the reply is input, not a final answer.
                next_prompts.push(format!(
                    "The user has answered the ask_user question with: \"{user_reply}\". \
                 Continue the original task using this input — do NOT treat the reply as a \
                 final answer. If the task still requires tool calls (e.g. skill_mcp_search, \
                 respond), make them now. Only call the `respond` tool (or output a plain text \
                 reply) when the original task is actually complete."
                ));
            }
        }

        if exit_reason.is_some() {
            // ── Checklist Gate 1: pending checklist items ──
            let (pending_count, total_count, remaining_with_status, todos_empty) = {
                let todos = &handler.working().todos;
                let pending = todos
                    .iter()
                    .filter(|t| t.status == "pending" || t.status == "in_progress")
                    .count();
                let remaining: Vec<(String, String, String)> = todos
                    .iter()
                    .filter(|t| t.status != "completed")
                    .map(|t| (t.id.clone(), t.content.clone(), t.status.clone()))
                    .collect();
                (pending, todos.len(), remaining, todos.is_empty())
            };
            if pending_count > 0 {
                let items_fmt: Vec<String> = remaining_with_status
                    .iter()
                    .enumerate()
                    .map(|(i, (id, content, status))| {
                        let status_label = match status.as_str() {
                            "in_progress" => {
                                if ctx.lang == "zh" {
                                    "⏳进行中"
                                } else {
                                    "⏳in-progress"
                                }
                            }
                            _ => {
                                if ctx.lang == "zh" {
                                    "📋待处理"
                                } else {
                                    "📋pending"
                                }
                            }
                        };
                        format!("  {}. [{}] {} {}", i + 1, id, status_label, content)
                    })
                    .collect();
                let hint = if ctx.lang == "zh" {
                    format!(
                        "[CHECKLIST] {}/{} 项未完成。请逐项调用 todoupdate(id, status)：\n{}\n\nstatus 可用值：completed / cancelled。完成后再次调用 respond 即可退出。",
                        pending_count, total_count,
                        items_fmt.join("\n")
                    )
                } else {
                    format!(
                        "[CHECKLIST] {}/{} items incomplete. Call todoupdate(id, status) for each:\n{}\n\nValid status: completed / cancelled. Call respond when ALL done.",
                        pending_count, total_count,
                        items_fmt.join("\n")
                    )
                };
                next_prompts.push(hint);
                exit_reason = None;
                transition_state(
                    handler,
                    AgentState::Thinking,
                    "checklist gate: pending items remain",
                );
            }

            // ── Checklist Gate 2: complex ops without checklist ──
            if todos_empty && exit_reason.is_some() && turn >= 2 {
                let write_count = tool_sequence
                    .iter()
                    .filter(|(name, _)| {
                        matches!(
                            name.as_str(),
                            "write" | "file_write" | "edit" | "file_edit" | "patch" | "file_patch"
                        )
                    })
                    .count();
                let run_count = tool_sequence
                    .iter()
                    .filter(|(name, _)| name == "code_run")
                    .count();
                let is_complex = write_count >= 2 || (write_count >= 1 && run_count >= 1);
                if is_complex {
                    let hint = if ctx.lang == "zh" {
                        format!(
                            "[PROTOCOL] 执行了 {} 次写操作和 {} 次命令，但无任务清单。\
                             复杂任务需 `todowrite` 分解。请先创建清单再继续。",
                            write_count, run_count
                        )
                    } else {
                        format!(
                            "[PROTOCOL] {} write(s) and {} command(s) without a checklist. \
                             Complex tasks require `todowrite`. Create a checklist first.",
                            write_count, run_count
                        )
                    };
                    next_prompts.push(hint);
                    exit_reason = None;
                    transition_state(
                        handler,
                        AgentState::Thinking,
                        "checklist gate: complex operations without checklist",
                    );
                }
            }

            // ── Spec anchor (one-shot): writes without a task spec ──
            // Guides the agent to create task_spec.md (with [verify]
            // assertions) before finishing; only fires once per run so it
            // can never loop.
            if !spec_hint_sent
                && crate::quality::has_write_operations(&tool_sequence)
                && crate::quality::read_spec(&config.working_dir).is_none()
            {
                spec_hint_sent = true;
                next_prompts.push(crate::quality::spec_anchor_hint(&config.lang));
                exit_reason = None;
                transition_state(
                    handler,
                    AgentState::Thinking,
                    "spec anchor: writes without task_spec.md",
                );
            }

            // ── Plan gentle reminder (P1): writes without a plan ──
            // One-shot, NON-blocking (never clears exit_reason) — mirrors
            // ZCode's "gentle reminder - ignore if not applicable" nudge.
            // Fires only while the run continues (a finishing turn discards
            // next_prompts), so simple tasks are never held up by it.
            if !plan_hint_sent
                && turn >= 2
                && crate::quality::has_write_operations(&tool_sequence)
                && handler.working().in_plan_mode.is_none()
                && handler.working().todos.is_empty()
            {
                plan_hint_sent = true;
                let hint = if config.lang == "zh" {
                    "[PLAN] 你已进行文件写入，但尚未提交执行计划（submit_plan）也未建立待办清单。\
                     复杂任务建议先 submit_plan(goal, steps) 建立可验证步骤再继续执行。\
                     （温和提示——简单任务可直接忽略。）"
                        .to_string()
                } else {
                    "[PLAN] You have written files but have not submitted an execution plan \
                     (submit_plan) nor created todos. For complex tasks, consider \
                     submit_plan(goal, steps) with verifiable steps first. \
                     (Gentle reminder — ignore if not applicable for simple tasks.)"
                        .to_string()
                };
                next_prompts.push(hint);
            }

            // DQ2 QB-2: opt-in gentle test-first nudge — never blocks exit,
            // mirrors the plan-reminder semantics above.
            if config.tdd_gate_enabled
                && !tdd_nudged
                && turn >= 2
                && crate::quality::has_write_operations(&tool_sequence)
                && !tool_sequence.iter().any(|(n, a)| {
                    n == "code_run" && a.to_string().to_lowercase().contains("test")
                })
            {
                tdd_nudged = true;
                next_prompts.push(if config.lang == "zh" {
                    "[TDD] 本次改动尚未见测试运行。建议为关键路径补一条最小测试并跑通（温和提示——可忽略）。"
                        .to_string()
                } else {
                    "[TDD] No test execution seen for this change set yet. Consider adding one \
                     minimal passing test for the critical path (gentle reminder — ignorable)."
                        .to_string()
                });
            }

            // ── Gate C (P2): unresolved-suspicion closure ──
            // Surface recent uncertainty ("飞船可能朝左" / "seems off") before
            // exit so the agent confirms or fixes it instead of carrying an
            // open suspicion into delivery. One-shot per run.
            if exit_reason.is_some() && config.quality_gates && !suspicion_checked {
                suspicion_checked = true;
                if let Some(hint) = crate::quality::find_unresolved_suspicion(
                    &messages,
                    &tool_sequence,
                    &config.lang,
                ) {
                    next_prompts.push(hint);
                    exit_reason = None;
                    transition_state(
                        handler,
                        AgentState::Thinking,
                        "quality gate: unresolved suspicion needs closure",
                    );
                }
            }

            // ── Delivery-quality gates (P2-10) ──
            if exit_reason.is_some() && config.quality_gates {
                let write_count = crate::quality::collect_deliverables(&tool_sequence).len();

                // DQ1 QA-1: writes happened but no task_spec.md exists —
                // synthesize minimal executable assertions from the user's own
                // request text via the summary model, then run Gate A on them
                // like an agent-authored spec. One-shot, fail-open.
                if !auto_spec_attempted
                    && write_count > 0
                    && crate::quality::read_spec(&config.working_dir).is_none()
                {
                    auto_spec_attempted = true;
                    if compression_service.is_configured() {
                        let deliverables =
                            crate::quality::collect_deliverables(&tool_sequence);
                        let prompt = crate::quality::build_spec_synthesis_prompt(
                            &user_input,
                            &deliverables,
                            &config.lang,
                        );
                        let rx = compression_service.spawn_summary(prompt.clone(), String::new());
                        let raw = wait_for_summary(Some(rx), String::new(), 30).await;
                        let lines = crate::quality::extract_verify_lines(&raw);
                        match crate::quality::write_auto_spec(&config.working_dir, &lines) {
                            Some(path) => {
                                tracing::info!(
                                    "[quality] auto-synthesized {} assertions -> {}",
                                    lines.len(),
                                    path.display()
                                );
                                crate::quality::log_quality_event(
                                    &config.working_dir,
                                    "spec_synthesized",
                                    &format!("{} assertion(s) derived from user request", lines.len()),
                                );
                            }
                            None => tracing::warn!(
                                "[quality] auto spec synthesis produced no usable assertions (fail-open)"
                            ),
                        }
                    }
                }

                let spec_text = crate::quality::read_spec(&config.working_dir);

                // Gate A: [verify] assertions from task_spec.md — executable
                // acceptance criteria the agent pre-registered at task start.
                // Failures feed back with real output; fix rounds are bounded
                // by assertion_max_rounds, then we exit with a note.
                if let Some(spec) = &spec_text {
                    if assertion_rounds < config.assertion_max_rounds {
                        let failures =
                            crate::quality::run_assertion_gate(spec, &config.working_dir).await;
                        contract_assertions =
                            Some((crate::quality::load_assertions(spec).len(), failures.len()));
                        if !failures.is_empty() {
                            assertion_rounds += 1;
                            next_prompts.push(crate::quality::format_assertion_feedback(
                                &failures,
                                &config.lang,
                                assertion_rounds,
                            ));
                            // Reflexion: record the failure for later tasks.
                            crate::quality::log_reflection(
                                &config.working_dir,
                                "assertion_failed",
                                &format!("{} failure(s): {}", failures.len(), failures[0].command),
                            );
                            exit_reason = None;
                            transition_state(
                                handler,
                                AgentState::Thinking,
                                "quality gate: verify assertion failed",
                            );
                        }
                    } else if !assertions_exhausted_checked {
                        // Budget exhausted: check once more for the exit note
                        // so the final reply honestly states unverified state.
                        assertions_exhausted_checked = true;
                        let failures =
                            crate::quality::run_assertion_gate(spec, &config.working_dir).await;
                        contract_assertions =
                            Some((crate::quality::load_assertions(spec).len(), failures.len()));
                        if !failures.is_empty() {
                            quality_note = Some(if config.lang == "zh" {
                                format!("验收断言未通过（{} 条），修复预算已耗尽", failures.len())
                            } else {
                                format!(
                                    "acceptance assertions failed ({}), fix budget exhausted",
                                    failures.len()
                                )
                            });
                        }
                    }
                }

                // Gate B: independent multi-perspective review (important
                // tasks only). Clean context: spec + deliverables + final
                // reply — never the implementation transcript. Two rounds
                // max (initial review + one re-review after fixes).
                if exit_reason.is_some()
                    && config.review_enabled
                    && write_count >= config.review_min_tools as usize
                    && review_rounds < 2
                {
                    review_rounds += 1;
                    let deliverables = crate::quality::collect_deliverables(&tool_sequence);
                    // Fall back to the original task text (first 1500 chars)
                    // when no spec was written — reviewing against an empty
                    // spec defeats the purpose of the cross-check.
                    let spec_for_review = spec_text
                        .clone()
                        .unwrap_or_else(|| user_input.chars().take(1500).collect::<String>());
                    let reply_for_review = full_response.clone();
                    let mut review_prompt = crate::quality::build_review_prompt(
                        &spec_for_review,
                        &deliverables,
                        &reply_for_review,
                        &config.lang,
                    );
                    // QA-2: adversarial pass for high-risk change sets.
                    if crate::quality::is_high_risk_write(&tool_sequence) {
                        review_prompt.push_str(if config.lang == "zh" {
                            crate::quality::RED_TEAM_SUFFIX_ZH
                        } else {
                            crate::quality::RED_TEAM_SUFFIX_EN
                        });
                    }
                    // QA-2 reviewer selection: summary model first (a different
                    // model removes self-review blind spots); main client only
                    // as a bounded fallback.
                    let verdict_opt = if config.review_use_summary
                        && compression_service.is_configured()
                    {
                        let rx =
                            compression_service.spawn_summary(review_prompt, String::new());
                        let raw = wait_for_summary(Some(rx), String::new(), 90).await;
                        crate::quality::review_verdict_from_raw(&raw)
                    } else {
                        crate::quality::review_prompt_via_client(client, &review_prompt).await
                    };
                    match verdict_opt {
                        Some(v) => {
                            let high_count =
                                v.issues.iter().filter(|i| i.severity == "high").count();
                            contract_review = Some((v.pass, high_count));
                            if v.pass {
                                // QC-1: success-side evidence.
                                crate::quality::log_quality_event(
                                    &config.working_dir,
                                    "review_passed",
                                    &format!("high={high_count}"),
                                );
                            } else if review_rounds < 2 {
                                crate::quality::log_reflection(
                                    &config.working_dir,
                                    "review_failed",
                                    &format!("{high_count} high issue(s)"),
                                );
                                next_prompts.push(crate::quality::format_review_feedback(
                                    &v.issues,
                                    &config.lang,
                                ));
                                exit_reason = None;
                                transition_state(
                                    handler,
                                    AgentState::Thinking,
                                    "quality gate: review issues found",
                                );
                            } else {
                                quality_note = Some(if config.lang == "zh" {
                                    "独立评审未通过，修复预算已耗尽".to_string()
                                } else {
                                    "independent review failed, fix budget exhausted"
                                        .to_string()
                                });
                            }
                        }
                        None => {}
                    }
                }
            }

            // DQ1 QA-3: oversized uncommitted diff gets ONE self-check round
            // against the spec before the loop may end.
            if exit_reason.is_some()
                && config.quality_gates
                && !diff_selfcheck_done
                && crate::quality::has_write_operations(&tool_sequence)
            {
                diff_selfcheck_done = true;
                if let Some(ds) =
                    crate::quality::git_diff_stat_summary(&config.working_dir).await
                {
                    if ds.files_changed > crate::quality::DIFF_FILES_MAX
                        || ds.churn > crate::quality::DIFF_CHURN_MAX
                    {
                        tracing::info!(
                            "[quality] diff self-check: {} files / {} churn exceeds threshold",
                            ds.files_changed,
                            ds.churn
                        );
                        next_prompts.push(crate::quality::build_diff_selfcheck_prompt(
                            &ds.excerpt,
                            &config.lang,
                        ));
                        exit_reason = None;
                        transition_state(handler, AgentState::Thinking, "diff self-check");
                    }
                }
            }

            if exit_reason.is_some() {
                // ── P0: Emergency compression before exit ──
                // When the agent is about to exit (via respond or LLM done),
                // compress context BEFORE breaking so that on resume/reopen
                // the context is manageable. Without this, accumulated tool
                // results and messages persist at 300K+ tokens, making every
                // subsequent LLM call slow and the stop button unresponsive.
                if config.enable_compression && config.context_win > 0 {
                    let comp_config = crate::compress::CompressionConfig::default();
                    let pre_exit_stats = crate::compress::measure_usage(&messages);
                    // Project tokens forward with the real chars→tokens
                    // ratio from the last LLM call, so tool results that
                    // grew the context after that call are counted. Falls
                    // back to chars/4 when no LLM usage is known yet.
                    let est_tokens = if last_turn_input_tokens > 0 && last_turn_input_chars > 0 {
                        (pre_exit_stats.total_chars as f64
                            * (last_turn_input_tokens as f64 / last_turn_input_chars as f64))
                            as usize
                    } else {
                        pre_exit_stats.total_chars / 4
                    };
                    if est_tokens > comp_config.hard_max_tokens {
                        tracing::warn!(
                            "Pre-exit compression: {} chars / {} msgs → compressing before agent exits",
                            pre_exit_stats.total_chars, messages.len()
                        );
                        let snapshot: Vec<_> = messages
                            .iter()
                            .filter_map(|m| serde_json::to_value(m).ok())
                            .collect();
                        let emergency_win = if comp_config.trigger_pct > 0 {
                            comp_config.hard_max_tokens * 100 / comp_config.trigger_pct as usize
                        } else {
                            comp_config.hard_max_tokens
                        };
                        let _saved = crate::compress::compress_messages(
                            &mut messages,
                            emergency_win,
                            &comp_config,
                            Some(est_tokens.max(1)),
                        );
                        let template = crate::compress::build_compression_summary(
                            &snapshot,
                            &config.working_dir,
                        );
                        let full_prompt = crate::compress::build_compression_prompt(&snapshot);
                        // P1-h pending marker (after_tokens=0) — see pre-call site.
                        if let Some(ref tx) = config.event_tx {
                            let _ = tx.send(oz_core_types::StreamEvent::DataCompressingContext {
                                before_tokens: est_tokens,
                                after_tokens: 0,
                                saved_tokens: 0,
                            });
                        }
                        // Wait (bounded) for the LLM summary so a later
                        // resume sees the real summary; the template is the
                        // terminal fallback on timeout (never replaced later).
                        let summary_text = if compression_service.is_configured() {
                            let rx =
                                compression_service.spawn_summary(full_prompt, template.clone());
                            wait_for_summary(Some(rx), template, comp_config.summary_wait_secs)
                                .await
                        } else {
                            template
                        };
                        if !summary_text.is_empty() {
                            let inject_at = messages
                                .iter()
                                .position(|m| {
                                    m.role == oz_core_types::Role::User
                                        || m.role == oz_core_types::Role::Assistant
                                })
                                .unwrap_or(0);
                            messages.insert(
                                inject_at,
                                Message::system(format!("[Compression summary]: {summary_text}")),
                            );
                        }
                    }
                }
                transition_state(
                    handler,
                    AgentState::Done(exit_reason.clone().unwrap()),
                    "loop exit condition met",
                );
                break;
            }
        }

        // ── Direction B: FSM transition — back to Idle for next turn ──
        transition_state(handler, AgentState::Idle, "turn complete, ready for next");

        // Save loop checkpoint periodically — borrowed, zero-clone (P1-f).
        if config.checkpoint_interval > 0
            && turn.is_multiple_of(config.checkpoint_interval)
            && !config.session_id.is_empty()
        {
            let cp_dir =
                std::path::PathBuf::from(config.checkpoint_dir.as_deref().unwrap_or("checkpoints"));
            let (git_sha, git_branch, git_origin_url) =
                crate::checkpoint::git_snapshot_async(std::path::Path::new(&config.working_dir))
                    .await;
            let session_opt = Some(config.session_id.as_str());
            let interventions: [crate::checkpoint::InterventionEvent; 0] = [];
            let plan = crate::checkpoint::plan_from_todos(&handler.working().todos);
            let cp = crate::checkpoint::LoopCheckpointRef {
                turn,
                timestamp: chrono::Utc::now().timestamp() as f64,
                messages: &messages,
                history_info: &history_info,
                full_response: &full_response,
                exit_reason: &exit_reason,
                session_id: session_opt,
                plan: &plan,
                todos: &handler.working().todos,
                interventions: &interventions,
                full_thinking: Some(full_thinking.as_str()),
                git_sha: git_sha.as_deref(),
                git_branch: git_branch.as_deref(),
                git_origin_url: git_origin_url.as_deref(),
            };
            crate::checkpoint::save_loop_checkpoint_borrowed_async(&cp_dir, &config.session_id, cp)
                .await;
        }

        let guard_msg = handler.working().sensorium.detect_loop();
        if let Some(msg) = guard_msg {
            next_prompts.push(msg);
        }

        if let Some(ref reason) = exit_reason {
            let _ = reason;
            // Exit the loop
            let summary = build_summary_from_response(&response, &tool_calls_iter);
            let _ = summary;
            break;
        }

        // ── P0: Stall guard — force-exit when the LLM produces no
        // tool calls across consecutive turns. Auto-generated prompts
        // (todo hints, intent warnings) are injected by the agent loop
        // itself and do NOT count as forward progress. Only actual LLM
        // tool calls reset this counter (handled above at ~line 825).
        // Without this guard, an agent with pending todos cycles
        // indefinitely: intent-only response → todo hint → reset →
        // intent-only → todo hint → reset …
        // Wall-clock timeout (extends per-LLM-call timeout):
        // if the agent has been running for >30 minutes without
        // meaningful tool output, force-exit with a save point.
        if consecutive_empty_turns >= 10 {
            tracing::warn!("{consecutive_empty_turns} consecutive turns with no tool calls — LLM appears stuck, exiting");
            save_stop_checkpoint_async(
                config,
                turn,
                "llm_stuck",
                &messages,
                &history_info,
                &full_response,
                &full_thinking,
                &handler.working().todos,
            )
            .await;
            transition_state(
                handler,
                AgentState::Done("llm_stuck".into()),
                "consecutive empty turns",
            );
            return LoopOutcome {
                turn,
                exit_reason: "llm_stuck".into(),
                data: Some(serde_json::json!({
                    "full_response": full_response.clone(),
                    "input_tokens_est": total_input_tokens,
                    "output_tokens_est": total_output_tokens,
                    "context_tokens_est": last_turn_input_tokens,
                })),
            };
        }

        if next_prompts.is_empty() && exit_reason.is_none() {
            let pending_todos = handler
                .working()
                .todos
                .iter()
                .filter(|t| t.status == "pending" || t.status == "in_progress")
                .count();
            if pending_todos > 0 {
                let remaining: Vec<String> = handler
                    .working()
                    .todos
                    .iter()
                    .filter(|t| t.status != "completed")
                    .map(|t| format!("  [{}] {}", t.id, t.content))
                    .collect();
                let hint = if ctx.lang == "zh" {
                    format!(
                        "还有 {} 项待办未完成，请继续：\n{}\n\n完成后调用 respond 结束任务。",
                        pending_todos,
                        remaining.join("\n")
                    )
                } else {
                    format!(
                        "{} pending todo(s), continue working:\n{}\n\nCall respond when all are done.",
                        pending_todos, remaining.join("\n")
                    )
                };
                next_prompts.push(hint);
            } else {
                transition_state(
                    handler,
                    AgentState::Done("CURRENT_TASK_DONE".into()),
                    "no next prompts and no exit reason",
                );
                break;
            }
        }

        // Anti-runaway warning every 10 turns on CONTINUING turns only —
        // placed after the emptiness check so a clean finish is never held
        // hostage by it (this used to be computed-then-discarded dead code).
        if turn.is_multiple_of(10) {
            next_prompts.push(DANGER_LOOP_MSG.replace("{turn}", &turn.to_string()));
        }

        let combined_next = next_prompts.join("\n");
        let next_prompt_str = handler.turn_end(
            &response,
            &tool_calls_iter,
            &tool_results,
            turn,
            combined_next,
            exit_reason.clone(),
        );

        history_info.push(format!(
            "[Agent] {}",
            smart_format(
                &build_summary_from_response(&response, &tool_calls_iter),
                80
            )
        ));

        // Append the assistant turn and the tool-result turn instead
        // of replacing `messages`. Replacing was the root cause of the
        // multi-turn amnesia bug: after one tool call the LLM lost the
        // system prompt, the original user request, and every prior
        // turn, so it kept answering with "I'm here, what can I help?".
        let mut assistant_blocks: Vec<ContentBlock> = Vec::new();
        if !clean_content.is_empty() {
            assistant_blocks.push(ContentBlock::text(&clean_content));
        }
        for tc in &tool_calls_iter {
            // `respond` is the synthetic wrapper that captures the LLM's
            // text reply when it didn't call any real tool; pushing it
            // as a real tool_use breaks the conversation protocol.
            if tc.name == "respond" {
                continue;
            }
            if !tc.id.is_empty() {
                assistant_blocks.push(ContentBlock::tool_use(
                    &tc.id,
                    &tc.name,
                    tc.arguments.clone(),
                ));
            }
        }
        if assistant_blocks.is_empty() {
            assistant_blocks.push(ContentBlock::text(""));
        }
        messages.push(Message::assistant_with_blocks(assistant_blocks));

        let mut user_blocks: Vec<ContentBlock> = Vec::new();
        for tr in &tool_results {
            user_blocks.push(ContentBlock::tool_result(&tr.tool_use_id, &tr.content));
            for img in &tr.images {
                user_blocks.push(ContentBlock::ImageUrl {
                    url: img.url.clone(),
                    media_type: Some(img.media_type.clone()),
                });
            }
        }
        if !next_prompt_str.is_empty() {
            user_blocks.push(ContentBlock::text(&next_prompt_str));
        }
        if !user_blocks.is_empty() {
            messages.push(Message::user_with_blocks(user_blocks));
        }

        // ── P0: Post-tool emergency compression check ──
        // After tool results are appended, the context may have grown
        // significantly within a single turn. If it exceeds hard_max_tokens,
        // force aggressive compression immediately to prevent the next
        // LLM call from choking on a huge prefill.
        if config.enable_compression && config.context_win > 0 {
            let comp_config = crate::compress::CompressionConfig::default();
            let post_tool_stats = crate::compress::measure_usage(&messages);
            // Project tokens forward with the real chars→tokens ratio
            // from the last LLM call, so tool results appended this turn
            // are counted. Falls back to chars/4 when no LLM usage known.
            let est_tokens = if last_turn_input_tokens > 0 && last_turn_input_chars > 0 {
                (post_tool_stats.total_chars as f64
                    * (last_turn_input_tokens as f64 / last_turn_input_chars as f64))
                    as usize
            } else {
                post_tool_stats.total_chars / 4
            };
            let emergency = est_tokens > comp_config.hard_max_tokens;

            if emergency {
                tracing::warn!(
                    "EMERGENCY: post-tool context {est_tokens} est-tokens / {} chars / {} msgs exceeds ceiling — forcing aggressive compression",
                    post_tool_stats.total_chars, messages.len()
                );
                let before_chars = post_tool_stats.total_chars;
                let before_count = messages.len();
                let snapshot: Vec<_> = messages
                    .iter()
                    .filter_map(|m| serde_json::to_value(m).ok())
                    .collect();
                // P0: The emergency flag means we've exceeded hard_max_tokens
                // (80K by default), but compress_messages targets
                // context_win * trigger_pct% (e.g. 256K * 80% = 205K). At
                // 210K context, that removes only ~5K tokens — effectively
                // a no-op. Compute an effective window so the target IS
                // hard_max_tokens, forcing real reduction.
                let emergency_win = if comp_config.trigger_pct > 0 {
                    comp_config.hard_max_tokens * 100 / comp_config.trigger_pct as usize
                } else {
                    comp_config.hard_max_tokens
                };
                let _saved = crate::compress::compress_messages(
                    &mut messages,
                    emergency_win,
                    &comp_config,
                    Some(est_tokens),
                );
                let template =
                    crate::compress::build_compression_summary(&snapshot, &config.working_dir);
                let full_prompt = crate::compress::build_compression_prompt(&snapshot);
                // P1-h pending marker (after_tokens=0) — see pre-call site.
                if let Some(ref tx) = config.event_tx {
                    let _ = tx.send(oz_core_types::StreamEvent::DataCompressingContext {
                        before_tokens: est_tokens,
                        after_tokens: 0,
                        saved_tokens: 0,
                    });
                }
                // Wait (bounded) for the LLM summary via the summary
                // model; the template is the terminal fallback on timeout
                // (never replaced later — one compression changes the
                // injected prefix exactly once).
                let summary_text = if compression_service.is_configured() {
                    let rx = compression_service.spawn_summary(full_prompt, template.clone());
                    wait_for_summary(Some(rx), template, comp_config.summary_wait_secs).await
                } else {
                    template
                };
                let after_stats = crate::compress::measure_usage(&messages);
                let after_chars = after_stats.total_chars;
                let after_tokens = if before_chars > 0 {
                    (after_chars as f64 / before_chars as f64 * est_tokens as f64) as usize
                } else {
                    0
                };
                let saved_tokens = est_tokens.saturating_sub(after_tokens);
                // Keep the per-turn estimate consistent with the compressed
                // context so the next turn is not force-compressed again.
                last_turn_input_tokens = after_tokens as u64;
                if config.verbose {
                    tracing::debug!(
                        "Post-tool emergency compression: {before_count}→{} msgs, {est_tokens}→{after_tokens} tokens",
                        messages.len()
                    );
                }
                if let Some(ref tx) = config.event_tx {
                    if saved_tokens >= COMPRESSION_NOTICE_MIN_TOKENS {
                        let _ = tx.send(oz_core_types::StreamEvent::DataCompressingContext {
                            before_tokens: est_tokens,
                            after_tokens,
                            saved_tokens,
                        });
                    }
                    let _ = tx.send(oz_core_types::StreamEvent::DataContextUsage {
                        current_tokens: after_tokens as u64,
                        output_tokens: 0,
                        context_window: config.context_win,
                        turn,
                        message_count: messages.len(),
                        total_input_tokens,
                        total_output_tokens,
                    });
                }
                if !summary_text.is_empty() {
                    let inject_at = messages
                        .iter()
                        .position(|m| {
                            m.role == oz_core_types::Role::User
                                || m.role == oz_core_types::Role::Assistant
                        })
                        .unwrap_or(0);
                    messages.insert(
                        inject_at,
                        Message::system(format!("[Compression summary]: {summary_text}")),
                    );
                }
            }
        }
    }
    // Honesty: when the turn budget ran out (loop exited via the while
    // condition, not via respond), report max_turns_exhausted instead of
    // silently claiming "CURRENT_TASK_DONE". Only fall back to
    // CURRENT_TASK_DONE for genuinely clean loop-end states.
    let final_reason = exit_reason.clone().unwrap_or_else(|| {
        if turn >= config.max_turns {
            "max_turns_exhausted".to_string()
        } else {
            "CURRENT_TASK_DONE".to_string()
        }
    });

    // Reflexion: record abnormal exits so later tasks can avoid the mode.
    if matches!(
        final_reason.as_str(),
        "llm_stuck" | "llm_error" | "llm_timeout" | "max_turns_exhausted"
    ) {
        crate::quality::log_reflection(
            &config.working_dir,
            &final_reason,
            &format!("task ended abnormally after {} turn(s)", turn),
        );
        // L1-a: distill an actionable lesson from abnormal exits — future
        // similar runs recall it through the harness-lessons channel.
        let exit_lesson = match final_reason.as_str() {
            "llm_stuck" => Some("任务多轮无工具调用而停滞：把需求拆成更小的可验证 todo，先跑通最小闭环再扩展。"),
            "max_turns_exhausted" => Some("轮次耗尽：减少一次性大改动，分批交付并尽早验证每一步。"),
            "llm_error" | "llm_timeout" => Some("传输不稳导致中断：重要节点及时 checkpoint，恢复后从断点继续而非重做。"),
            _ => None,
        };
        if let Some(lesson) = exit_lesson {
            crate::quality::log_quality_event(&config.working_dir, "lesson", lesson);
        }
    }
    // QC-1: success-side evidence — clean deliveries also become data so
    // weekly aggregation can show pass rates, not only failures.
    if final_reason == "EXITED" && crate::quality::has_write_operations(&tool_sequence) {
        crate::quality::log_quality_event(
            &config.working_dir,
            "delivery_success",
            &format!(
                "turns={turn} tools={} deliverables={}",
                tool_sequence.len(),
                crate::quality::collect_deliverables(&tool_sequence).len()
            ),
        );
    }
    if !tool_sequence.is_empty() && final_reason == "EXITED" {
        let safe_name: String = user_input
            .chars()
            .take(40)
            .map(|c| {
                if c.is_alphanumeric() || c == '_' || c == '-' {
                    c
                } else {
                    '_'
                }
            })
            .collect();
        let sess_id = if config.session_id.is_empty() {
            None
        } else {
            Some(config.session_id.clone())
        };

        // Priority: skill_mcp_store > sop_store (legacy)
        if config.enable_crystallization {
            if let Some(ref store_arc) = skill_mcp_store {
                let mut store = store_arc.lock().await;
                let _ = store.crystallise_sop(
                    &safe_name,
                    &smart_format(&user_input, 100),
                    &tool_sequence,
                    sess_id,
                );
                if config.verbose {
                    tracing::info!(
                        "Crystallised SOP via SkillMcpStore from {} tool calls",
                        tool_sequence.len()
                    );
                }
            } else if let Some(ref mut store) = sop_store {
                store.crystallise(
                    &safe_name,
                    &smart_format(&user_input, 100),
                    &tool_sequence,
                    sess_id,
                );
                if config.verbose {
                    tracing::info!("Crystallised SOP from {} tool calls", tool_sequence.len());
                }
            }
        }
    }

    // Session transcript: built once for the background distillation queue.
    let transcript = messages
        .iter()
        .filter_map(|m| {
            if matches!(
                m.role,
                oz_core_types::Role::User | oz_core_types::Role::Assistant
            ) {
                let text: Vec<&str> = m
                    .content
                    .iter()
                    .filter_map(|b| match b {
                        oz_core_types::ContentBlock::Text { text, .. } => Some(text.as_str()),
                        _ => None,
                    })
                    .collect();
                if text.is_empty() {
                    None
                } else {
                    Some(text.join(" "))
                }
            } else {
                None
            }
        })
        .collect::<Vec<_>>()
        .join("\n");
    let transcript = transcript.trim();

    // Background memory distillation (M5): enqueue the transcript for async
    // knowledge extraction instead of blocking the loop. The distiller is
    // chosen by the caller (ERME semantic store or legacy skill/MCP), and
    // the worker retries with backoff under a lease. Falls back to the
    // LLM-driven Crystallizer below when no scheduler is configured.
    if let Some(ref scheduler) = config.memory_scheduler {
        if !transcript.is_empty() {
            scheduler
                .submit(config.session_id.clone(), transcript.to_string())
                .await;
            if config.verbose {
                tracing::info!(
                    "Enqueued session '{}' for memory distillation",
                    config.session_id
                );
            }
        }
    }

    // LLM-driven crystallization & refinement (when skill_mcp_store is active)
    if let Some(ref store_arc) = skill_mcp_store {
        let mut store = store_arc.lock().await;
        if config.enable_crystallization && !tool_sequence.is_empty() && final_reason == "EXITED" {
            match Crystallizer::crystallize(
                client,
                &mut store,
                &user_input,
                &messages,
                &tool_sequence,
                if config.session_id.is_empty() {
                    None
                } else {
                    Some(config.session_id.clone())
                },
            )
            .await
            {
                Ok(results) => {
                    for r in &results {
                        match r {
                            crate::crystallizer::CrystallizeResult::SkillCreated { name } => {
                                tracing::info!("Crystallized skill: {}", name)
                            }
                            crate::crystallizer::CrystallizeResult::SopCreated { name } => {
                                tracing::info!("Crystallized SOP: {}", name)
                            }
                            crate::crystallizer::CrystallizeResult::FactAdded { content } => {
                                tracing::info!("Crystallized fact: {}", content)
                            }
                            crate::crystallizer::CrystallizeResult::Nothing => {}
                        }
                    }
                }
                Err(e) => tracing::warn!("Crystallization failed: {}", e),
            }
        }

        if config.enable_refinement && store.skill_count() > 0 {
            match Refiner::refine_all_skills(client, &mut store).await {
                Ok(results) => {
                    for r in &results {
                        if let crate::refiner::RefineResult::Refined {
                            name,
                            old_version,
                            new_version,
                        } = r
                        {
                            tracing::info!(
                                "Refined skill '{}': v{} → v{}",
                                name,
                                old_version,
                                new_version
                            );
                        }
                    }
                }
                Err(e) => tracing::warn!("Skill refinement failed: {}", e),
            }
        }
    }

    // ── Direction B: ensure final Done state ──
    if !handler.working().current_state.is_terminal() {
        transition_state(
            handler,
            AgentState::Done(final_reason.clone()),
            "loop ended",
        );
    }

    let tool_seq_json: Vec<serde_json::Value> = tool_sequence
        .iter()
        .map(|(name, args)| serde_json::json!({"name": name, "arguments": args}))
        .collect();

    // Bug-1 fallback: local Qwen3 / MiniMax / GLM / Step / Claude
    // (via proxy) sometimes emit the visible reply inside the
    // reasoning channel — no matching text block ever opens. Promote
    // thinking-as-response so the user always sees a reply.
    let trimmed_response = full_response.trim();
    let trimmed_thinking = full_thinking.trim();
    let final_full_response = if trimmed_response.is_empty() && trimmed_thinking.len() >= 20 {
        tracing::warn!(
            "[agent_loop] empty response, promoting {}-char thinking as the user reply",
            trimmed_thinking.len()
        );
        full_thinking.clone()
    } else {
        full_response
    };

    // Attach the quality-gate note (assertions / review failed after the
    // fix budget was exhausted) so the user sees an honest delivery state.
    let final_full_response = if let Some(note) = &quality_note {
        format!("{final_full_response}\n\n> ⚠️ {note}")
    } else {
        final_full_response
    };

    // DQ1 QA-4: three-section delivery contract — done / verified / left
    // open. Honesty becomes protocol rather than goodwill.
    let final_full_response = if config.quality_gates
        && crate::quality::has_write_operations(&tool_sequence)
    {
        let completed_head = smart_format(final_full_response.trim(), 120);
        let mut verified: Vec<String> = Vec::new();
        if let Some((total, failed)) = contract_assertions {
            verified.push(if config.lang == "zh" {
                format!("断言 {}/{} 通过", total.saturating_sub(failed), total)
            } else {
                format!("{}/{} assertions passed", total.saturating_sub(failed), total)
            });
        }
        if let Some((passed, high)) = contract_review {
            verified.push(if passed {
                if config.lang == "zh" {
                    "独立评审通过".to_string()
                } else {
                    "independent review passed".to_string()
                }
            } else if config.lang == "zh" {
                format!("独立评审提出 high 问题 ×{high}")
            } else {
                format!("independent review raised {high} high issue(s)")
            });
        }
        if verified.is_empty() && contract_assertions.is_none() {
            verified.push(if config.lang == "zh" {
                "未运行验收断言（无 spec）".to_string()
            } else {
                "no acceptance assertions ran (no spec)".to_string()
            });
        }
        let pending_left = handler
            .working()
            .todos
            .iter()
            .filter(|t| t.status == "pending" || t.status == "in_progress")
            .count();
        let leftover = quality_note.clone().or_else(|| {
            (pending_left > 0).then(|| {
                if config.lang == "zh" {
                    format!("{pending_left} 项待办未完成")
                } else {
                    format!("{pending_left} pending todo(s)")
                }
            })
        });
        let verification = (!verified.is_empty()).then(|| verified.join("；"));
        final_full_response
            + &crate::quality::format_delivery_contract(
                &config.lang,
                Some(&completed_head),
                verification.as_deref(),
                leftover.as_deref(),
            )
    } else {
        final_full_response
    };

    LoopOutcome {
        turn,
        exit_reason: final_reason,
        data: Some(serde_json::json!({
            "full_response": final_full_response,
            "full_thinking": full_thinking,
            "input_tokens_est": total_input_tokens,
            "output_tokens_est": total_output_tokens,
            "context_tokens_est": last_turn_input_tokens,
            "tool_sequence": tool_seq_json,
        })),
    }
}

/// Bound user feedback text injected into next_prompts.
fn truncate_feedback(s: &str) -> String {
    const MAX: usize = 300;
    if s.chars().count() <= MAX {
        s.to_string()
    } else {
        format!("{}…", s.chars().take(MAX).collect::<String>())
    }
}

fn build_summary_from_response(
    response: &MockResponse,
    tool_calls: &[oz_core_types::MockToolCall],
) -> String {
    // Extract <summary> tag from content
    if let Some(pos) = response.content.find("<summary>") {
        if let Some(end) = response.content[pos..].find("</summary>") {
            return response.content[pos + 9..pos + end].trim().to_string();
        }
    }
    if !tool_calls.is_empty() {
        let tc = &tool_calls[0];
        let clean_args: serde_json::Value = match &tc.arguments {
            serde_json::Value::Object(obj) => {
                let filtered: serde_json::Map<String, serde_json::Value> = obj
                    .iter()
                    .filter(|(k, _)| !k.starts_with('_'))
                    .map(|(k, v)| (k.clone(), v.clone()))
                    .collect();
                serde_json::Value::Object(filtered)
            }
            other => other.clone(),
        };
        format!(
            "调用工具{}, args: {}",
            tc.name,
            serde_json::to_string(&clean_args).unwrap_or_default()
        )
    } else {
        "直接回答了用户问题".into()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::handler::WorkingMemory;
    use async_trait::async_trait;
    use oz_core_types::{MockToolCall, StepOutcome, ToolResultItem};
    use std::sync::atomic::AtomicBool;

    #[test]
    fn extract_tool_calls_returns_clone() {
        let calls = vec![
            oz_core_types::MockToolCall::new("read_file", serde_json::json!({"path": "/tmp/test"})),
            oz_core_types::MockToolCall::new("write_file", serde_json::json!({"path": "/tmp/out"})),
        ];
        let response = MockResponse::with_tools("done", calls.clone());
        let result = extract_tool_calls(&response);
        assert_eq!(result.len(), 2);
        assert_eq!(result[0].name, "read_file");
        assert_eq!(result[1].name, "write_file");
    }

    #[test]
    fn extract_tool_calls_empty() {
        let response = MockResponse::new("just a response");
        let result = extract_tool_calls(&response);
        assert!(result.is_empty());
    }

    #[test]
    fn extract_tool_calls_independent_clone() {
        let calls = vec![oz_core_types::MockToolCall::new(
            "test",
            serde_json::json!({}),
        )];
        let response = MockResponse::with_tools("ok", calls);
        let result = extract_tool_calls(&response);
        assert_eq!(result[0].name, response.tool_calls[0].name);
    }

    #[test]
    fn smart_format_short_returns_as_is() {
        assert_eq!(smart_format("hello", 10), "hello");
    }

    #[test]
    fn smart_format_equal_to_max_len_returns_as_is() {
        assert_eq!(smart_format("abcde", 5), "abcde");
    }

    #[test]
    fn smart_format_truncates_with_dots_in_middle() {
        assert_eq!(smart_format("abcdefghij", 6), "abc...hij");
    }

    #[test]
    fn smart_format_very_long_truncates_correctly() {
        let result = smart_format("0123456789ABCDEFGHIJKLMNOPQRSTUVWXYZ", 10);
        assert_eq!(result.len(), 13);
        assert!(result.contains("..."));
    }

    #[test]
    fn smart_format_empty_string_returns_empty() {
        assert_eq!(smart_format("", 5), "");
    }

    #[test]
    fn smart_format_zero_max_len_with_nonempty_input() {
        let result = smart_format("nonempty", 0);
        assert!(result.len() > 0);
        assert!(result.contains("..."));
    }

    #[test]
    fn smart_format_odd_max_len() {
        let result = smart_format("testing123", 5);
        assert_eq!(result.len(), 7);
    }

    #[test]
    fn truncate_stream_output_ascii_under_cap() {
        assert_eq!(truncate_stream_output("hello", 32), "hello");
        assert_eq!(truncate_stream_output("", 10), "");
    }

    #[test]
    fn truncate_stream_output_ascii_over_cap() {
        let out = truncate_stream_output("x".repeat(40).as_str(), 32);
        assert!(out.starts_with(&"x".repeat(32)));
        assert!(out.contains("original 40 bytes"));
        assert!(out.contains("kept first 32"));
    }

    // Regression (round3 P0-A): String::truncate panicked when the byte cap
    // landed inside a multi-byte CJK character. 3-byte chars × cap 9 forces
    // the old code to cut at offset 9-1/2 → panic; the char-safe path must
    // instead back off to a boundary and keep valid UTF-8.
    #[test]
    fn truncate_stream_output_cjk_char_boundary() {
        let s = "中".repeat(8); // 24 bytes, all 3-byte chars
        let out = truncate_stream_output(&s, 10);
        assert!(out.is_char_boundary(0));
        let kept = out.split('\n').next().unwrap();
        assert!(!kept.ends_with('\u{FFFD}'));
        assert_eq!(
            kept.as_bytes().len() % 3,
            0,
            "cut must fall on a 3-byte boundary"
        );
        assert!(out.contains("original 24 bytes"));
    }

    #[test]
    fn truncate_stream_output_exact_cap_is_identity() {
        let s = "中".repeat(4); // exactly 12 bytes
        assert_eq!(truncate_stream_output(&s, 12), s);
    }

    // ── integration tests for run_agent_loop ──

    /// Mock LLM client that returns a fixed response sequence.
    struct MockLlm {
        responses: Vec<MockResponse>,
        idx: usize,
    }

    impl MockLlm {
        fn new(responses: Vec<MockResponse>) -> Self {
            MockLlm { responses, idx: 0 }
        }
    }

    #[async_trait]
    impl oz_core_types::LlmClient for MockLlm {
        async fn chat(
            &mut self,
            _messages: &[Message],
            _tools: &[ToolDefinition],
        ) -> Result<MockResponse, oz_core_types::LlmError> {
            if self.idx < self.responses.len() {
                let resp = self.responses[self.idx].clone();
                self.idx += 1;
                Ok(resp)
            } else {
                Ok(MockResponse::new("done"))
            }
        }
    }

    /// Mock handler that records calls and returns controlled outcomes.
    struct MockHandler {
        working: WorkingMemory,
        /// Map from tool_name to list of (outcome, times_to_return)
        tool_outcomes: std::sync::Mutex<
            std::collections::HashMap<String, Vec<Result<StepOutcome, oz_core_types::ToolError>>>,
        >,
        tool_calls: std::sync::Mutex<Vec<(String, serde_json::Value)>>,
        turn_end_calls: std::sync::Mutex<Vec<String>>,
    }

    impl MockHandler {
        fn new() -> Self {
            MockHandler {
                working: WorkingMemory::default(),
                tool_outcomes: std::sync::Mutex::new(std::collections::HashMap::new()),
                tool_calls: std::sync::Mutex::new(Vec::new()),
                turn_end_calls: std::sync::Mutex::new(Vec::new()),
            }
        }

        fn on_tool(
            self,
            name: &str,
            outcome: Result<StepOutcome, oz_core_types::ToolError>,
        ) -> Self {
            self.tool_outcomes
                .lock()
                .unwrap()
                .entry(name.to_string())
                .or_default()
                .push(outcome);
            self
        }

        fn get_tool_calls(&self) -> Vec<(String, serde_json::Value)> {
            self.tool_calls.lock().unwrap().clone()
        }

        #[allow(dead_code)]
        fn get_turn_end_calls(&self) -> Vec<String> {
            self.turn_end_calls.lock().unwrap().clone()
        }
    }

    #[async_trait]
    impl Handler for MockHandler {
        fn working(&self) -> &WorkingMemory {
            &self.working
        }
        fn working_mut(&mut self) -> &mut WorkingMemory {
            &mut self.working
        }
        fn turn_end(
            &mut self,
            _response: &MockResponse,
            _tool_calls: &[MockToolCall],
            _tool_results: &[ToolResultItem],
            _turn: u32,
            next_prompt: String,
            _exit_reason: Option<String>,
        ) -> String {
            self.turn_end_calls
                .lock()
                .unwrap()
                .push(next_prompt.clone());
            next_prompt
        }
        async fn dispatch(
            &self,
            tool_name: &str,
            args: serde_json::Value,
            _response: &MockResponse,
            _index: u32,
            _ctx: &ToolContext,
        ) -> Result<StepOutcome, oz_core_types::ToolError> {
            let mut tool_calls = self.tool_calls.lock().unwrap();
            tool_calls.push((tool_name.to_string(), args));
            let mut outcomes = self.tool_outcomes.lock().unwrap();
            if let Some(list) = outcomes.get_mut(tool_name) {
                if !list.is_empty() {
                    return list.remove(0);
                }
            }
            Ok(StepOutcome::success(serde_json::json!({"status": "ok"})))
        }
    }

    fn default_ctx() -> ToolContext {
        ToolContext {
            working_dir: "/tmp".into(),
            assets_dir: "/tmp".into(),
            script_dir: "/tmp".into(),
            lang: "en".into(),
            skill_mcp_dir: None,
            harness_dir: None,
            session_id: String::new(),
        }
    }

    #[tokio::test]
    async fn agent_loop_basic_no_tool_exit() {
        let mut client = MockLlm::new(vec![MockResponse::new("Hello, I'm the assistant.")]);
        let mut handler = MockHandler::new().on_tool(
            "respond",
            Ok(StepOutcome::exit(serde_json::json!({"status": "ok"}))),
        );
        let signal = AtomicBool::new(false);
        let config = LoopConfig {
            max_turns: 10,
            verbose: false,
            ..Default::default()
        };

        let outcome = run_agent_loop(
            &mut client,
            "system".into(),
            "user input".into(),
            vec![],
            &mut handler,
            &[],
            &default_ctx(),
            &config,
            &signal,
        )
        .await;

        assert_eq!(outcome.exit_reason, "EXITED");
        assert_eq!(outcome.turn, 1);
    }

    #[tokio::test]
    async fn agent_loop_llm_error() {
        struct ErrorLlm;
        #[async_trait]
        impl oz_core_types::LlmClient for ErrorLlm {
            async fn chat(
                &mut self,
                _: &[Message],
                _: &[ToolDefinition],
            ) -> Result<MockResponse, oz_core_types::LlmError> {
                Err(oz_core_types::LlmError::HttpError {
                    status: 500,
                    body: "server error".into(),
                })
            }
        }

        let mut client = ErrorLlm;
        let mut handler = MockHandler::new();
        let signal = AtomicBool::new(false);
        let config = LoopConfig {
            max_turns: 10,
            verbose: false,
            ..Default::default()
        };

        let outcome = run_agent_loop(
            &mut client,
            "system".into(),
            "user".into(),
            vec![],
            &mut handler,
            &[],
            &default_ctx(),
            &config,
            &signal,
        )
        .await;

        assert_eq!(outcome.exit_reason, "llm_error");
        assert_eq!(outcome.turn, 1);
    }

    #[tokio::test]
    async fn agent_loop_stop_signal() {
        let mut client = MockLlm::new(vec![MockResponse::new("First response")]);
        let mut handler = MockHandler::new().on_tool(
            "respond",
            Ok(StepOutcome::success(
                serde_json::json!({"status": "continue"}),
            )),
        );
        let signal = AtomicBool::new(true);
        let config = LoopConfig {
            max_turns: 10,
            verbose: false,
            ..Default::default()
        };

        let outcome = run_agent_loop(
            &mut client,
            "system".into(),
            "user".into(),
            vec![],
            &mut handler,
            &[],
            &default_ctx(),
            &config,
            &signal,
        )
        .await;

        assert_eq!(outcome.exit_reason, "stopped_by_user");
        assert_eq!(outcome.turn, 0);
    }

    #[tokio::test]
    async fn agent_loop_max_turns_exceeded() {
        // Mock LLM that always returns a no_tool call that continues
        let no_tool_resp = {
            let mut r = MockResponse::new("still working");
            r.tool_calls = vec![oz_core_types::MockToolCall::new(
                "respond",
                serde_json::json!({"response": "progress"}),
            )];
            r
        };
        let responses = vec![no_tool_resp.clone(); 5];
        let mut client = MockLlm::new(responses);

        let mut handler = MockHandler::new().on_tool(
            "respond",
            Ok(StepOutcome::success(
                serde_json::json!({"status": "continue"}),
            )),
        );
        let signal = AtomicBool::new(false);
        let config = LoopConfig {
            max_turns: 3,
            verbose: false,
            ..Default::default()
        };

        let outcome = run_agent_loop(
            &mut client,
            "system".into(),
            "user".into(),
            vec![],
            &mut handler,
            &[],
            &default_ctx(),
            &config,
            &signal,
        )
        .await;

        assert_eq!(outcome.exit_reason, "max_turns_exhausted");
        assert_eq!(outcome.turn, 3);
    }

    #[tokio::test]
    async fn agent_loop_tool_call_then_exit() {
        let resp = {
            let mut r = MockResponse::new("using tools");
            r.tool_calls = vec![oz_core_types::MockToolCall::with_id(
                "read",
                serde_json::json!({"file_path": "/tmp/test.txt"}),
                "call_1",
            )];
            r
        };
        let mut client = MockLlm::new(vec![resp]);
        let mut handler = MockHandler::new().on_tool(
            "read",
            Ok(StepOutcome::exit(
                serde_json::json!({"content": "file data"}),
            )),
        );
        let signal = AtomicBool::new(false);
        let config = LoopConfig {
            max_turns: 10,
            verbose: false,
            ..Default::default()
        };

        let outcome = run_agent_loop(
            &mut client,
            "system".into(),
            "user".into(),
            vec![],
            &mut handler,
            &[],
            &default_ctx(),
            &config,
            &signal,
        )
        .await;

        assert_eq!(outcome.exit_reason, "EXITED");
        assert_eq!(outcome.turn, 1);
        let calls = handler.get_tool_calls();
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].0, "read");
    }

    #[tokio::test]
    async fn agent_loop_tool_error_continues() {
        let resp = {
            let mut r = MockResponse::new("using tools");
            r.tool_calls = vec![oz_core_types::MockToolCall::with_id(
                "failing_tool",
                serde_json::json!({"arg": "val"}),
                "call_err",
            )];
            r
        };
        let mut client = MockLlm::new(vec![resp, MockResponse::new("done now")]);
        let mut handler = MockHandler::new()
            .on_tool(
                "failing_tool",
                Err(oz_core_types::ToolError::Custom("something broke".into())),
            )
            .on_tool(
                "respond",
                Ok(StepOutcome::exit(serde_json::json!({"status": "ok"}))),
            );
        let signal = AtomicBool::new(false);
        let config = LoopConfig {
            max_turns: 10,
            verbose: false,
            ..Default::default()
        };

        let outcome = run_agent_loop(
            &mut client,
            "system".into(),
            "user".into(),
            vec![],
            &mut handler,
            &[],
            &default_ctx(),
            &config,
            &signal,
        )
        .await;

        // Tool error should be captured but loop should continue to next turn
        assert_eq!(outcome.exit_reason, "EXITED");
        assert_eq!(outcome.turn, 2);
    }

    #[tokio::test]
    async fn agent_loop_ask_user_keeps_run_alive() {
        let resp = {
            let mut r = MockResponse::new("need help");
            r.tool_calls = vec![oz_core_types::MockToolCall::with_id(
                "ask_user",
                serde_json::json!({"question": "what now?"}),
                "call_ask",
            )];
            r
        };
        // Single LLM turn: ask_user fires, gets the pre-supplied reply,
        // resumes, then exits normally (no more prompts) — NOT "ASK_USER".
        let mut client = MockLlm::new(vec![resp]);
        let mut handler = MockHandler::new().on_tool(
            "ask_user",
            Ok(StepOutcome {
                data: serde_json::json!({
                    "status": "INTERRUPT",
                    "intent": "HUMAN_INTERVENTION",
                    "data": {"question": "what now?", "candidates": []}
                }),
                next_prompt: None,
                should_exit: false,
                images: vec![],
            }),
        );
        let signal = AtomicBool::new(false);
        // Pre-populate under the legacy key so the loop's first poll
        // finds it regardless of which question id is being waited on.
        let ask_rx = std::sync::Arc::new(std::sync::Mutex::new(
            std::iter::once(("__last__".to_string(), "answer-1".to_string())).collect(),
        ));
        let config = LoopConfig {
            max_turns: 10,
            verbose: false,
            ask_user_rx: Some(ask_rx),
            ..Default::default()
        };

        let outcome = run_agent_loop(
            &mut client,
            "system".into(),
            "user".into(),
            vec![],
            &mut handler,
            &[],
            &default_ctx(),
            &config,
            &signal,
        )
        .await;

        assert_ne!(
            outcome.exit_reason, "ASK_USER",
            "ask_user must no longer exit the run"
        );
        // The mock has no more prompts, so the loop runs to the turn budget;
        // with honest exit-reason reporting this is max_turns_exhausted, not
        // a silent CURRENT_TASK_DONE.
        assert_eq!(
            outcome.exit_reason, "max_turns_exhausted",
            "loop should resume and complete normally"
        );
    }

    #[tokio::test]
    async fn agent_loop_multiple_tools_in_one_turn() {
        let resp = {
            let mut r = MockResponse::new("multi tool turn");
            r.tool_calls = vec![
                oz_core_types::MockToolCall::with_id(
                    "tool_a",
                    serde_json::json!({"x": 1}),
                    "call_a",
                ),
                oz_core_types::MockToolCall::with_id(
                    "tool_b",
                    serde_json::json!({"y": 2}),
                    "call_b",
                ),
            ];
            r
        };
        let mut client = MockLlm::new(vec![resp, MockResponse::new("all done")]);
        let mut handler = MockHandler::new()
            .on_tool(
                "tool_a",
                Ok(StepOutcome::success(serde_json::json!({"result": "a"}))),
            )
            .on_tool(
                "tool_b",
                Ok(StepOutcome::exit(serde_json::json!({"result": "b"}))),
            );
        let signal = AtomicBool::new(false);
        let config = LoopConfig {
            max_turns: 10,
            verbose: false,
            ..Default::default()
        };

        let outcome = run_agent_loop(
            &mut client,
            "system".into(),
            "user".into(),
            vec![],
            &mut handler,
            &[],
            &default_ctx(),
            &config,
            &signal,
        )
        .await;

        assert_eq!(outcome.exit_reason, "EXITED");
        assert_eq!(outcome.turn, 1);
        // Both tools should have been dispatched
        let calls = handler.get_tool_calls();
        assert_eq!(calls.len(), 2);
        assert_eq!(calls[0].0, "tool_a");
        assert_eq!(calls[1].0, "tool_b");
    }

    #[tokio::test]
    async fn test_parallel_execution_concurrent() {
        // Two tools each with a 200ms delay should complete in ~200ms (not 400ms)
        struct SlowHandler {
            working: WorkingMemory,
            calls: std::sync::Mutex<Vec<String>>,
        }
        #[async_trait]
        impl Handler for SlowHandler {
            fn working(&self) -> &WorkingMemory {
                &self.working
            }
            fn working_mut(&mut self) -> &mut WorkingMemory {
                &mut self.working
            }
            fn turn_end(
                &mut self,
                _r: &MockResponse,
                _tc: &[MockToolCall],
                _tr: &[ToolResultItem],
                _t: u32,
                np: String,
                _er: Option<String>,
            ) -> String {
                np
            }
            async fn dispatch(
                &self,
                name: &str,
                _a: serde_json::Value,
                _r: &MockResponse,
                _i: u32,
                _c: &ToolContext,
            ) -> Result<StepOutcome, oz_core_types::ToolError> {
                self.calls.lock().unwrap().push(name.to_string());
                tokio::time::sleep(Duration::from_millis(200)).await;
                Ok(StepOutcome::exit(serde_json::json!({"status": "ok"})))
            }
        }
        let resp = {
            let mut r = MockResponse::new("parallel turn");
            r.tool_calls = vec![
                oz_core_types::MockToolCall::with_id("tool_a", serde_json::json!({}), "call_a"),
                oz_core_types::MockToolCall::with_id("tool_b", serde_json::json!({}), "call_b"),
            ];
            r
        };
        let mut client = MockLlm::new(vec![resp]);
        let mut handler = SlowHandler {
            working: WorkingMemory::default(),
            calls: std::sync::Mutex::new(Vec::new()),
        };
        let signal = AtomicBool::new(false);
        let config = LoopConfig {
            max_turns: 1,
            verbose: false,
            max_concurrent_tools: 8,
            tool_timeout_secs: 30,
            ..Default::default()
        };

        let start = std::time::Instant::now();
        let outcome = run_agent_loop(
            &mut client,
            "sys".into(),
            "user".into(),
            vec![],
            &mut handler,
            &[],
            &default_ctx(),
            &config,
            &signal,
        )
        .await;
        let elapsed = start.elapsed();

        assert_eq!(outcome.exit_reason, "EXITED");
        // With parallel execution, 200ms of work should take ~200ms, not the
        // 400ms+ serial sum. The 600ms bound tolerates CI/parallel-test load
        // while still catching a serial-execution regression (which would
        // take >= 400ms even unloaded).
        assert!(
            elapsed.as_millis() < 600,
            "parallel execution took {}ms, expected <600ms",
            elapsed.as_millis()
        );
        let calls = handler.calls.lock().unwrap();
        assert_eq!(calls.len(), 2);
    }

    #[tokio::test]
    async fn test_parallel_cancellation_on_should_exit() {
        // tool_a exits immediately, tool_b should be cancelled
        struct CancelHandler {
            working: WorkingMemory,
            dispatched: std::sync::Mutex<Vec<String>>,
        }
        #[async_trait]
        impl Handler for CancelHandler {
            fn working(&self) -> &WorkingMemory {
                &self.working
            }
            fn working_mut(&mut self) -> &mut WorkingMemory {
                &mut self.working
            }
            fn turn_end(
                &mut self,
                _r: &MockResponse,
                _tc: &[MockToolCall],
                _tr: &[ToolResultItem],
                _t: u32,
                np: String,
                _er: Option<String>,
            ) -> String {
                np
            }
            async fn dispatch(
                &self,
                name: &str,
                _a: serde_json::Value,
                _r: &MockResponse,
                _i: u32,
                _c: &ToolContext,
            ) -> Result<StepOutcome, oz_core_types::ToolError> {
                self.dispatched.lock().unwrap().push(name.to_string());
                match name {
                    "tool_a" => Ok(StepOutcome::exit(serde_json::json!({"status": "exited"}))),
                    _ => Ok(StepOutcome::success(serde_json::json!({"status": "ok"}))),
                }
            }
        }
        let resp = {
            let mut r = MockResponse::new("cancel turn");
            r.tool_calls = vec![
                oz_core_types::MockToolCall::with_id("tool_a", serde_json::json!({}), "call_a"),
                oz_core_types::MockToolCall::with_id("tool_b", serde_json::json!({}), "call_b"),
            ];
            r
        };
        let mut client = MockLlm::new(vec![resp]);
        let mut handler = CancelHandler {
            working: WorkingMemory::default(),
            dispatched: std::sync::Mutex::new(Vec::new()),
        };
        let signal = AtomicBool::new(false);
        let config = LoopConfig {
            max_turns: 1,
            verbose: false,
            max_concurrent_tools: 8,
            tool_timeout_secs: 30,
            ..Default::default()
        };

        let outcome = run_agent_loop(
            &mut client,
            "sys".into(),
            "user".into(),
            vec![],
            &mut handler,
            &[],
            &default_ctx(),
            &config,
            &signal,
        )
        .await;
        assert_eq!(outcome.exit_reason, "EXITED");
    }

    #[tokio::test]
    async fn test_parallel_tool_timeout() {
        // A slow tool should be timed out (1s timeout vs 10s sleep)
        struct TimeoutHandler {
            working: WorkingMemory,
        }
        #[async_trait]
        impl Handler for TimeoutHandler {
            fn working(&self) -> &WorkingMemory {
                &self.working
            }
            fn working_mut(&mut self) -> &mut WorkingMemory {
                &mut self.working
            }
            fn turn_end(
                &mut self,
                _r: &MockResponse,
                _tc: &[MockToolCall],
                _tr: &[ToolResultItem],
                _t: u32,
                np: String,
                _er: Option<String>,
            ) -> String {
                np
            }
            async fn dispatch(
                &self,
                name: &str,
                _a: serde_json::Value,
                _r: &MockResponse,
                _i: u32,
                _c: &ToolContext,
            ) -> Result<StepOutcome, oz_core_types::ToolError> {
                if name != "slow_tool" {
                    return Ok(StepOutcome::success(serde_json::json!({"status": "ok"})));
                }
                tokio::time::sleep(Duration::from_secs(10)).await;
                Ok(StepOutcome::success(serde_json::json!({"status": "ok"})))
            }
        }
        let resp = {
            let mut r = MockResponse::new("timeout turn");
            r.tool_calls = vec![oz_core_types::MockToolCall::with_id(
                "slow_tool",
                serde_json::json!({}),
                "call_slow",
            )];
            r
        };
        let mut client = MockLlm::new(vec![resp, MockResponse::new("after error")]);
        let mut handler = TimeoutHandler {
            working: WorkingMemory::default(),
        };
        let signal = AtomicBool::new(false);
        let config = LoopConfig {
            max_turns: 2,
            verbose: false,
            max_concurrent_tools: 8,
            tool_timeout_secs: 1,
            ..Default::default()
        };

        let start = std::time::Instant::now();
        let outcome = run_agent_loop(
            &mut client,
            "sys".into(),
            "user".into(),
            vec![],
            &mut handler,
            &[],
            &default_ctx(),
            &config,
            &signal,
        )
        .await;
        let elapsed = start.elapsed();

        // Should finish quickly (tool times out after 1s, not 10s)
        assert!(
            elapsed.as_secs() < 5,
            "timeout tool took {}s, should be <5s",
            elapsed.as_secs()
        );
        // max_turns=2 runs out after the timed-out turn; honest exit reason.
        assert_eq!(outcome.exit_reason, "max_turns_exhausted");
    }

    /// Regression test for the multi-turn amnesia bug. Previously
    /// `messages = vec![new_user_msg]` ran at the end of each turn and
    /// wiped the system prompt plus all prior turns, so turn 1+ saw
    /// only the new prompt and degraded into "I'm here, what can I
    /// help?" with no memory of prior tool calls.
    #[tokio::test]
    async fn test_messages_accumulate_across_turns() {
        struct CapturingLlm {
            responses: Vec<MockResponse>,
            idx: usize,
            snapshots: std::sync::Mutex<Vec<(usize, usize, usize, usize, usize)>>,
        }

        #[async_trait]
        impl oz_core_types::LlmClient for CapturingLlm {
            async fn chat(
                &mut self,
                messages: &[Message],
                _tools: &[ToolDefinition],
            ) -> Result<MockResponse, oz_core_types::LlmError> {
                let turn = self.idx;
                let user_msgs: Vec<&Message> = messages
                    .iter()
                    .filter(|m| matches!(m.role, oz_core_types::Role::User))
                    .collect();
                let assistant_msgs: Vec<&Message> = messages
                    .iter()
                    .filter(|m| matches!(m.role, oz_core_types::Role::Assistant))
                    .collect();
                let tool_msg_count: usize = messages
                    .iter()
                    .map(|m| {
                        let in_content = m
                            .content
                            .iter()
                            .filter(|b| matches!(b, oz_core_types::ContentBlock::ToolResult { .. }))
                            .count();
                        let in_tool_results = m.tool_results.as_ref().map(|v| v.len()).unwrap_or(0);
                        in_content + in_tool_results
                    })
                    .sum();
                self.snapshots.lock().unwrap().push((
                    turn,
                    messages.len(),
                    user_msgs.len(),
                    assistant_msgs.len(),
                    tool_msg_count,
                ));
                let resp = if self.idx < self.responses.len() {
                    self.responses[self.idx].clone()
                } else {
                    MockResponse::new("fallback")
                };
                self.idx += 1;
                Ok(resp)
            }
        }

        let mut r0 = MockResponse::new("calling tool_a");
        r0.tool_calls = vec![oz_core_types::MockToolCall::with_id(
            "tool_a",
            serde_json::json!({}),
            "call_a",
        )];
        let r1 = MockResponse::new("all done, no more tools");
        let responses = vec![r0, r1];
        let mut client = CapturingLlm {
            responses,
            idx: 0,
            snapshots: std::sync::Mutex::new(Vec::new()),
        };
        let mut handler = MockHandler::new()
            .on_tool(
                "tool_a",
                Ok(StepOutcome::success(serde_json::json!({"ok": true}))),
            )
            .on_tool(
                "respond",
                Ok(StepOutcome::success(
                    serde_json::json!({"status": "continue"}),
                )),
            );
        let signal = AtomicBool::new(false);
        let config = LoopConfig {
            max_turns: 3,
            verbose: false,
            max_concurrent_tools: 4,
            tool_timeout_secs: 30,
            ..Default::default()
        };

        let _ = run_agent_loop(
            &mut client,
            "system prompt here".into(),
            "initial user request".into(),
            vec![],
            &mut handler,
            &[],
            &default_ctx(),
            &config,
            &signal,
        )
        .await;

        let snapshots = client.snapshots.lock().unwrap().clone();
        assert!(
            snapshots.len() >= 2,
            "expected at least 2 LLM calls (one per turn), got {}",
            snapshots.len()
        );

        let (turn0_idx, turn0_total, _turn0_users, _turn0_assistants, _turn0_tools) = snapshots[0];
        assert_eq!(turn0_idx, 0, "first LLM call must be turn 0");
        assert!(turn0_total >= 1, "turn 0 must see at least one message");

        let (_t1, total1, user_count1, assistant_count1, tool_count1) = snapshots[1];
        assert!(
            total1 >= 3,
            "turn 1 must see >=3 messages (prior user + assistant with tool_use + user with tool_result), got total={}",
            total1
        );
        assert!(
            assistant_count1 >= 1,
            "turn 1 must see the prior assistant turn (containing the tool_use), got assistant_count={}",
            assistant_count1
        );
        assert!(
            tool_count1 >= 1,
            "turn 1 must see the tool_result block, got tool_count={}",
            tool_count1
        );
        assert!(
            user_count1 >= 1,
            "turn 1 must see a user-role message, got user_count={}",
            user_count1
        );
    }
}
