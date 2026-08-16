//! Agent loop runner — spawns the OpenZen agent loop in a Tauri session.
//!
//! One runner per session. Wires up SSE streaming, stop signals, ask_user
//! slots, and persists the final assistant message with duration + token
//! counts back to the session store.

use std::path::PathBuf;
use std::sync::atomic::AtomicBool;
use std::sync::{Arc, Mutex};

use oz_config::mykey::{MyKeyConfig, SessionType};
use oz_config::profile::load_profile;
use oz_core::handler::LoopConfig;
use oz_core_types::{ContentBlock, Message, StreamEvent};
use oz_memory::MemorySystem;
use oz_server::webui::sessions::SessionStatus;
use oz_server::webui::sse_bus::SseEvent;
use oz_tools::handler::ToolRegistryHandler;
use oz_tools::registry::ToolRegistry;
use tauri::{AppHandle, Emitter};

use crate::{
    data_dir, debug_log, home_dir, load_system_prompt, lock_poison_guard, tauri_ctx, AppState,
};

/// Truncate a string to at most `max` chars (boundary-safe) for use in
/// notification bodies — a hostile/loquacious question must not produce a
/// wall-of-text banner.
fn truncate_chars(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        return s.to_string();
    }
    let mut out: String = s.chars().take(max).collect();
    out.push('…');
    out
}

/// Cap the largest stream-event payloads (tool outputs, errors) so a run
/// that dumps megabytes of logs/files keeps bounded memory instead of
/// holding it all until the run ends. Aligns with the 100KB cap applied
/// when session messages are persisted.
fn truncate_stream_event(event: oz_core_types::StreamEvent) -> oz_core_types::StreamEvent {
    const MAX_EVENT_FIELD_CHARS: usize = 100_000;
    use oz_core_types::StreamEvent as E;
    match event {
        E::ToolOutputAvailable {
            tool_call_id,
            name,
            output,
        } => E::ToolOutputAvailable {
            tool_call_id,
            name,
            output: truncate_chars(&output, MAX_EVENT_FIELD_CHARS),
        },
        E::Error { message } => E::Error {
            message: truncate_chars(&message, MAX_EVENT_FIELD_CHARS),
        },
        other => other,
    }
}

/// Distills session transcripts into the skill/MCP knowledge store in the
/// background (U3). Kept in the Tauri layer because it owns the store path.
struct McpMemoryDistiller {
    skill_dir: PathBuf,
}

impl McpMemoryDistiller {
    fn new() -> Self {
        McpMemoryDistiller {
            skill_dir: home_dir().join(".skill_mcp"),
        }
    }
}

/// Distills session transcripts into the long-lived ERME semantic store
/// (M5). Runs inside the shared MemoryJobScheduler worker so distillation
/// never blocks the agent loop; failures are retried with backoff by the
/// scheduler instead of being lost.
///
/// Also ingests harness ledger entries so model-written, evidence-backed
/// lessons become semantically recallable (the ledger itself stays the
/// audit layer — this only mirrors Memory entries into the store).
struct ErmeMemoryDistiller {
    store: std::sync::Arc<entropy_memory_engine::memory_store::MemoryStore>,
    harness_dir: Option<std::path::PathBuf>,
}

impl ErmeMemoryDistiller {
    fn new(
        store: std::sync::Arc<entropy_memory_engine::memory_store::MemoryStore>,
        harness_dir: Option<std::path::PathBuf>,
    ) -> Self {
        ErmeMemoryDistiller { store, harness_dir }
    }
}

/// Distance below which a recall hit is treated as the same lesson
/// (aligned with the vendor's `SearchConfig::default().full_match_dist`).
const NEAR_DUP_DIST: f32 = 0.05;

/// Ingest `HarnessKind::Memory` entries from the ledger into the semantic
/// store as high-importance summaries. A lesson is skipped when a recall of
/// its own text already surfaces it (near-zero distance, or an exact content
/// match in the top results — the latter survives consolidation, which folds
/// L2 sources into L3 summaries). Returns the number stored.
fn ingest_harness_entries(
    store: &entropy_memory_engine::memory_store::MemoryStore,
    harness_dir: &std::path::Path,
) -> usize {
    use entropy_memory_engine::core::types::{MemoryContent, MemoryInput};
    let state = oz_core::harness::HarnessState::load(harness_dir);
    let mut stored = 0usize;
    for entry in state.entries_of(oz_core::harness::HarnessKind::Memory) {
        // recall returns distance (ascending, lower = closer); an identical
        // lesson embeds to the same vector → distance ≈ 0.
        let already_present = store
            .recall_by_text(&entry.content, 5)
            .ok()
            .map(|recalls| {
                recalls.iter().any(|(mem, dist, _)| {
                    *dist <= NEAR_DUP_DIST || mem.content_text().trim() == entry.content.trim()
                })
            })
            .unwrap_or(false);
        if already_present {
            continue;
        }
        let input =
            MemoryInput::new(MemoryContent::Summary(entry.content.clone())).with_importance(0.8);
        if store.store(input).is_ok() {
            stored += 1;
        }
    }
    stored
}

#[async_trait::async_trait]
impl oz_core::memory_job::MemoryDistiller for ErmeMemoryDistiller {
    async fn distill(&self, session_id: &str, transcript: &str) -> Result<usize, String> {
        let store = std::sync::Arc::clone(&self.store);
        let harness_dir = self.harness_dir.clone();
        let transcript = transcript.to_string();
        let result = tokio::task::spawn_blocking(move || -> Result<usize, String> {
            let mut stored = store
                .distill_and_store(&transcript)
                .map_err(|e| e.to_string())?;
            if let Some(dir) = &harness_dir {
                stored += ingest_harness_entries(&store, dir);
            }
            store.consolidate();
            Ok(stored)
        })
        .await
        .map_err(|e| e.to_string())?;
        result.map_err(|e| format!("erme distill failed for {session_id}: {e}"))
    }
}

#[async_trait::async_trait]
impl oz_core::memory_job::MemoryDistiller for McpMemoryDistiller {
    async fn distill(&self, _session_id: &str, transcript: &str) -> Result<usize, String> {
        let store = oz_skill_mcp::SkillMcpStore::new(&self.skill_dir, None);
        store
            .distill_memory(transcript, &oz_core_types::SkillMcpType::Insight)
            .await
            .map(|_| 1)
            .map_err(|e| e.to_string())
    }
}

/// Build the agent-loop's `additional_messages` from persisted session
/// messages. Translates the persistent JSON shape back into Claude
/// Content-Block protocol:
///   - assistant message + `tool_use_blocks` field → assistant_with_blocks(Text + ToolUse)
///   - legacy `role:"tool"` messages OR new `tool_results` field → user_with_blocks(ToolResult)
///   - legacy assistant without tool_use_blocks followed by role:"tool"
///     messages: synthesise ToolUse blocks from the tool messages' ids
///     + names so the protocol pairing is restored.
fn build_history_messages(messages: &[serde_json::Value]) -> Vec<Message> {
    if messages.is_empty() {
        eprintln!("[runner::build_history] EMPTY messages array");
        return Vec::new();
    }
    let slice = &messages[..messages.len().saturating_sub(1)];
    eprintln!(
        "[runner::build_history] total={} slice_len={}",
        messages.len(),
        slice.len()
    );
    let mut out: Vec<Message> = Vec::new();
    let mut pending_tool_results: Vec<ContentBlock> = Vec::new();
    let mut skip_next = false;

    let flush_tool_results = |out: &mut Vec<Message>, pending: &mut Vec<ContentBlock>| {
        if !pending.is_empty() {
            out.push(Message::user_with_blocks(std::mem::take(pending)));
        }
    };

    for (i, m) in slice.iter().enumerate() {
        if skip_next {
            skip_next = false;
            continue;
        }
        let role = match m.get("role").and_then(|v| v.as_str()) {
            Some(r) => r,
            None => continue,
        };
        match role {
            "user" | "assistant" => {
                flush_tool_results(&mut out, &mut pending_tool_results);
                let content = m
                    .get("content")
                    .and_then(|c| c.as_str())
                    .unwrap_or("")
                    .to_string();

                if role == "assistant" {
                    let tool_use_blocks = m.get("tool_use_blocks").and_then(|v| v.as_array());
                    let has_tool_uses = tool_use_blocks
                        .map(|arr| {
                            arr.iter()
                                .any(|b| b.get("id").and_then(|v| v.as_str()).is_some())
                        })
                        .unwrap_or(false);

                    // Legacy sessions keep assistant messages WITHOUT tool_use_blocks.
                    // Reconstruct from subsequent legacy role:"tool" ids + names so
                    // the tool_use ↔ tool_result pairing is restored.
                    let legacy_tool_use_blocks: Vec<serde_json::Value> = if !has_tool_uses {
                        let mut acc = Vec::new();
                        for nm in &slice[i + 1..] {
                            if nm.get("role").and_then(|v| v.as_str()) != Some("tool") {
                                break;
                            }
                            let tu_id =
                                nm.get("tool_use_id").and_then(|v| v.as_str()).unwrap_or("");
                            let tu_name =
                                nm.get("tool_name").and_then(|v| v.as_str()).unwrap_or("");
                            if !tu_id.is_empty() {
                                acc.push(serde_json::json!({
                                    "id": tu_id,
                                    "name": tu_name,
                                    "input": {},
                                }));
                            }
                        }
                        acc
                    } else {
                        Vec::new()
                    };

                    let blocks_to_emit = tool_use_blocks
                        .map(|arr| arr.to_vec())
                        .unwrap_or(legacy_tool_use_blocks);

                    if blocks_to_emit.is_empty() {
                        out.push(Message::assistant(&content));
                    } else {
                        let mut blocks: Vec<ContentBlock> = Vec::new();
                        let mut seen_tool_ids: Vec<String> = Vec::new();
                        if !content.is_empty() {
                            blocks.push(ContentBlock::text(&content));
                        }
                        for tb in &blocks_to_emit {
                            let id = tb
                                .get("id")
                                .and_then(|v| v.as_str())
                                .unwrap_or("")
                                .to_string();
                            let name = tb
                                .get("name")
                                .and_then(|v| v.as_str())
                                .unwrap_or("")
                                .to_string();
                            let input = tb.get("input").cloned().unwrap_or(serde_json::Value::Null);
                            if !id.is_empty() && !seen_tool_ids.contains(&id) {
                                seen_tool_ids.push(id.clone());
                                blocks.push(ContentBlock::tool_use(id, name, input));
                            }
                        }
                        if blocks.is_empty() {
                            blocks.push(ContentBlock::text(""));
                        }
                        out.push(Message::assistant_with_blocks(blocks));
                    }
                } else {
                    if let Some(tool_results) = m.get("tool_results").and_then(|v| v.as_array()) {
                        for tr in tool_results {
                            let tu_id = tr
                                .get("tool_use_id")
                                .and_then(|v| v.as_str())
                                .unwrap_or("")
                                .to_string();
                            let tr_content = tr
                                .get("content")
                                .and_then(|v| v.as_str())
                                .unwrap_or("")
                                .to_string();
                            if !tu_id.is_empty() {
                                pending_tool_results
                                    .push(ContentBlock::tool_result(tu_id, tr_content));
                            }
                        }
                        // Merge tool_results + content into a SINGLE user message
                        // (splitting into two breaks the LLM protocol pairing)
                        if !content.is_empty() {
                            pending_tool_results.push(ContentBlock::text(&content));
                        } else {
                            // Empty content but has tool_results: peek at the NEXT message.
                            // If it's a plain user text (no tool_results), merge its text
                            // into this same user message so the LLM sees a single
                            // user(tool_result + text) turn instead of two separate messages.
                            if i + 1 < slice.len() {
                                let next = &slice[i + 1];
                                if next.get("role").and_then(|v| v.as_str()) == Some("user")
                                    && next.get("tool_results").is_none()
                                {
                                    if let Some(next_content) =
                                        next.get("content").and_then(|v| v.as_str())
                                    {
                                        if !next_content.is_empty() {
                                            pending_tool_results
                                                .push(ContentBlock::text(next_content));
                                            skip_next = true;
                                        }
                                    }
                                }
                            }
                        }
                        flush_tool_results(&mut out, &mut pending_tool_results);
                    } else {
                        out.push(Message::user(&content));
                    }
                }
            }
            "tool" => {
                let tu_id = m
                    .get("tool_use_id")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string();
                let tr_content = m
                    .get("content")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string();
                if !tu_id.is_empty() {
                    pending_tool_results.push(ContentBlock::tool_result(tu_id, tr_content));
                }
            }
            _ => {}
        }
    }
    flush_tool_results(&mut out, &mut pending_tool_results);
    out
}

#[allow(clippy::field_reassign_with_default)]
pub async fn run_agent_for_session(
    app: &AppHandle,
    state: &Arc<AppState>,
    session_id: &str,
    model_name: Option<&str>,
    resume: bool,
) -> anyhow::Result<()> {
    // Resolve config through the same fallback chain as the ERME backend
    // gate (AppState::new) so both always agree on the active config.
    let config_path = crate::resolve_config_path(&state.config_path);
    debug_log(&format!("config_path={}", config_path.display()));

    let cfg = MyKeyConfig::from_file(std::path::Path::new(&config_path))
        // config_path is already debug_logged above — keep it out of the
        // error string, which is broadcast to all windows via sse_event
        // (local paths must not leak into the UI) (P3/A7).
        .map_err(|e| anyhow::anyhow!("Config error: {e}"))?;
    let profile = load_profile();
    let session_name = model_name
        .or(profile.default_model.as_deref())
        .or(cfg.default_session.as_deref())
        .unwrap_or("claude_sonnet");
    let sess_config = cfg
        .get(session_name)
        .ok_or_else(|| anyhow::anyhow!("Session '{session_name}' not found"))?
        .clone();
    let sess_type = cfg.session_type(session_name);

    let mut ctx = tauri_ctx();
    ctx.lang = lock_poison_guard(&state.locale).clone();
    ctx.session_id = session_id.to_string();

    // Broadcast model info
    let provider = match sess_type {
        SessionType::Claude => "claude",
        SessionType::Oai => "openai",
        SessionType::NativeClaude => "claude",
        SessionType::NativeOai => "openai",
        SessionType::Mixin => "mixin",
    };
    let sse_model_info = SseEvent::model_info(
        session_id,
        &sess_config.model,
        provider,
        sess_config.context_win,
        crate::is_local_deploy(&sess_config.apibase),
    );
    let _ = app.emit(
        "sse_event",
        serde_json::to_value(&sse_model_info).unwrap_or_default(),
    );

    let backend: Box<dyn oz_llm::Session> = match sess_type {
        SessionType::Claude => Box::new(oz_llm::ClaudeSession::new(sess_config.clone())),
        SessionType::Oai => Box::new(oz_llm::OaiSession::new(sess_config.clone())),
        SessionType::NativeClaude => {
            Box::new(oz_llm::NativeClaudeSession::new(sess_config.clone()))
        }
        SessionType::NativeOai => Box::new(oz_llm::NativeOAISession::new(sess_config.clone())),
        SessionType::Mixin => anyhow::bail!("Mixin session not supported in Tauri"),
    };
    let mut client = oz_llm::NativeToolClient::new(backend);

    // Resolve working directory from session → project → fallback.
    // Priority: session.working_dir > project.root_path > home_dir.
    let (project_root, project_ga_dir) = {
        let store = lock_poison_guard(&state.sessions);
        let sess_working_dir = store.get(session_id).and_then(|e| e.working_dir.clone());
        let pid = store.get(session_id).and_then(|e| e.project_id.clone());
        drop(store);
        // Use session's stored working_dir if available (eagerly resolved at creation)
        if let Some(ref wd) = sess_working_dir {
            let root = std::path::PathBuf::from(wd);
            let ga = root.join("openzen");
            debug_log(&format!(
                "run_agent: session={} using stored working_dir={}",
                session_id,
                root.display()
            ));
            (root, ga)
        } else if let Some(ref pid) = pid {
            let projects = lock_poison_guard(&state.projects);
            let found = projects.iter().find(|p| p.id == *pid).cloned();
            let project_count = projects.len();
            drop(projects);
            if let Some(ref p) = found {
                let root = std::path::PathBuf::from(&p.root_path);
                let ga = root.join("openzen");
                debug_log(&format!(
                    "run_agent: resolved project_root={}, project={}",
                    root.display(),
                    p.name
                ));
                (root, ga)
            } else {
                debug_log(&format!(
                    "run_agent: session={} has project_id={} but project not found among {} projects, using home_dir",
                    session_id, pid, project_count
                ));
                let root = home_dir();
                (root.clone(), root.join("openzen"))
            }
        } else {
            let root = home_dir();
            debug_log(&format!(
                "run_agent: session={} has no project_id or working_dir, using home_dir={}",
                session_id,
                root.display()
            ));
            (root.clone(), root.join("openzen"))
        }
    };
    ctx.working_dir = project_root.to_string_lossy().to_string();
    let _ = std::fs::create_dir_all(&project_ga_dir);

    let memory = MemorySystem::new(&project_root, &ctx.lang);
    let memory_context = if cfg.memory_backend == "erme" {
        // ERME semantic recall: inject top-k memories relevant to the user
        // message instead of the legacy full-text scan.
        match &state.erme_store {
            Some(runtime) => {
                let store = &runtime.store;
                let query = {
                    let store = lock_poison_guard(&state.sessions);
                    store
                        .get(session_id)
                        .and_then(|s| s.messages.last())
                        .and_then(|m| m.get("content").and_then(|c| c.as_str()))
                        .unwrap_or_default()
                        .to_string()
                };
                let query = if query.is_empty() {
                    "resume session context".to_string()
                } else {
                    query
                };
                // Feed the user's statement to the L0 reflection engine so
                // the user portrait and relationship actually evolve from
                // real conversations (notify only queues here — the events
                // are consumed by the idle cycle's process_pending).
                if !query.is_empty() {
                    runtime.reflection.notify(
                        entropy_memory_engine::l0::ReflectionEvent::UserStatement {
                            text: query.clone(),
                            tags: Vec::new(),
                        },
                    );
                }
                let store = std::sync::Arc::clone(store);
                let recalls = tokio::task::spawn_blocking(move || store.recall_by_text(&query, 5))
                    .await
                    .unwrap_or_else(|e| {
                        debug_log(&format!("ERME recall task failed: {e}"));
                        Ok(Vec::new())
                    })
                    .unwrap_or_default();
                if recalls.is_empty() {
                    debug_log("ERME recall returned 0 memories; falling back to file memory");
                    memory.get_global_memory().await.unwrap_or_default()
                } else {
                    let mut buf = String::new();
                    for (mem, score, _layer) in &recalls {
                        let text = match &mem.content {
                            entropy_memory_engine::core::types::MemoryContent::Fact(f) => {
                                format!("{} {} {}", f.subject, f.predicate, f.object)
                            }
                            entropy_memory_engine::core::types::MemoryContent::Summary(s) => {
                                s.clone()
                            }
                            _ => continue,
                        };
                        // recall returns distance (lower = closer); label it
                        // honestly so the model reads it correctly.
                        buf.push_str(&format!("- [dist {score:.2}] {text}\n"));
                    }
                    debug_log(&format!("ERME recall injected {} memories", recalls.len()));
                    buf
                }
            }
            None => {
                debug_log("ERME store unavailable; falling back to file memory");
                memory.get_global_memory().await.unwrap_or_default()
            }
        }
    } else {
        memory.get_global_memory().await.unwrap_or_default()
    };

    let registry = ToolRegistry::build_default();

    // MCP servers.toml is not started here: bridge tools are unregistered
    // stubs, and starting them + mem::forget leaked MCP child processes per run.

    let definitions = registry.to_schema(&ctx.lang);
    let mut handler = ToolRegistryHandler::new(registry);

    let mut system_prompt = load_system_prompt(&ctx);
    // L0 soul-layer injection (M7): prepend the persistent soul-model prefix
    // so every turn carries identity/narrative/portrait state.
    if cfg.memory_backend == "erme" {
        if let Some(runtime) = &state.erme_store {
            let prefix = runtime.injector.build_system_prefix();
            if !prefix.is_empty() {
                system_prompt = format!("{prefix}{system_prompt}");
            }
        }
    }
    // Harness ledger injection: surface model-written, evidence-backed
    // lessons every turn (a write-only ledger would silently rot).
    if let Some(harness_dir) = &ctx.harness_dir {
        let harness_ctx = oz_core::harness::render_context(
            std::path::Path::new(harness_dir),
            oz_core::harness::HarnessKind::Memory,
            8,
        );
        if !harness_ctx.is_empty() {
            system_prompt.push_str("\n\n## Persistent Harness Lessons\n\n");
            system_prompt.push_str(&harness_ctx);
        }
    }
    // Crystallized user facts/insights (skill-mcp L2 memory): without this
    // the facts the model writes were invisible to later sessions —
    // build_memory_prompt was only reachable through a rarely-taken
    // fallback path inside the agent loop. SkillMcpMemory::new is path
    // setup only (no directory scan); the prompt is byte-capped.
    if let Some(dir) = &state.skill_mcp_dir {
        const MAX_USER_MEMORY_PROMPT_BYTES: usize = 8 * 1024;
        let mem = oz_skill_mcp::SkillMcpMemory::new(std::path::Path::new(dir));
        if let Ok(mut prompt) = mem
            .build_memory_prompt(std::path::Path::new(&ctx.working_dir))
            .await
        {
            if prompt.len() > MAX_USER_MEMORY_PROMPT_BYTES {
                let mut end = MAX_USER_MEMORY_PROMPT_BYTES;
                while end > 0 && !prompt.is_char_boundary(end) {
                    end -= 1;
                }
                prompt.truncate(end);
                prompt.push_str("\n…[memory truncated]");
            }
            if !prompt.is_empty() {
                system_prompt.push_str("\n\n## User Memory (facts/insights)\n\n");
                system_prompt.push_str(&prompt);
            }
        }
    }
    if !memory_context.is_empty() {
        system_prompt.push_str("\n\n## Persistent Memory Context\n\n");
        system_prompt.push_str(&memory_context);
    }

    // Claude/OpenAI protocol requires paired
    // assistant-tool_use ↔ user-tool_result blocks; projecting to
    // standalone Message::assistant(text) + Message::tool(id,text)
    // breaks pairing, so the LLM loses prior tool turns between runs.
    let (user_message, history): (String, Vec<Message>) = if resume {
        // /resume path: the checkpoint already contains the full message
        // history (system prompt + all turns). The initial user_message and
        // history are replaced by the checkpoint in agent_loop anyway, so we
        // use a placeholder to satisfy the non-empty check below.
        ("[resume]".to_string(), Vec::new())
    } else {
        let store = lock_poison_guard(&state.sessions);
        let session = store.get(session_id);
        let user_msg = session
            .and_then(|s| s.messages.last())
            .and_then(|m| m.get("content").and_then(|c| c.as_str()))
            .unwrap_or_default()
            .to_string();
        let hist = session
            .map(|s| build_history_messages(&s.messages))
            .unwrap_or_default();
        (user_msg, hist)
    };
    if user_message.is_empty() {
        anyhow::bail!("No user message to process");
    }

    let mut loop_config = LoopConfig::default();
    loop_config.session_id = session_id.to_string();
    loop_config.lang = ctx.lang.clone();
    loop_config.verbose = true; // Enable SOP/skill loading logs for debugging
    loop_config.context_win = sess_config.context_win;
    // Local quantized models (MLX/omlx on 127.0.0.1) prefill slowly and
    // generate tokens at a fraction of cloud speed, so the 300s cloud
    // default reliably triggers llm_timeout mid-response. Give local
    // deployments a 30-minute ceiling; keep the tight default for cloud.
    loop_config.stream_timeout_secs = if crate::is_local_deploy(&sess_config.apibase) {
        1800
    } else {
        300
    };
    // Engine-pool contention (other sessions swapping models mid-stream)
    // aborts in-flight requests; give local engines double the retry
    // budget so a wedge doesn't kill the whole long task.
    loop_config.llm_error_retries = if crate::is_local_deploy(&sess_config.apibase) {
        6
    } else {
        3
    };
    loop_config.skill_mcp_dir = state.skill_mcp_dir.clone();
    loop_config.enable_crystallization = state
        .crystallization_enabled
        .load(std::sync::atomic::Ordering::Relaxed);
    loop_config.working_dir = project_root.to_string_lossy().to_string();
    loop_config.checkpoint_dir = Some(
        oz_core::checkpoint::checkpoint_dir(&project_root)
            .to_string_lossy()
            .to_string(),
    );

    // Background memory distillation (U3): a worker drains the job queue so
    // session distillation never blocks the agent loop. The distiller is
    // selected by memory_backend: ERME semantic store vs legacy skill/MCP.
    let distiller: std::sync::Arc<dyn oz_core::memory_job::MemoryDistiller> =
        if cfg.memory_backend == "erme" {
            match &state.erme_store {
                Some(runtime) => std::sync::Arc::new(ErmeMemoryDistiller::new(
                    std::sync::Arc::clone(&runtime.store),
                    ctx.harness_dir.as_ref().map(std::path::PathBuf::from),
                )),
                None => {
                    debug_log("ERME store unavailable; using MCP distiller for memory jobs");
                    std::sync::Arc::new(McpMemoryDistiller::new())
                }
            }
        } else {
            std::sync::Arc::new(McpMemoryDistiller::new())
        };
    let memory_scheduler =
        std::sync::Arc::new(oz_core::memory_job::MemoryJobScheduler::new(distiller));
    // The 30s drain tick is aborted when the run finishes — previously it
    // outlived the run and leaked one interval task per send/regenerate/resume.
    // RAII: the tick must also die when the agent loop panics, otherwise the
    // crashed run leaks an interval task that keeps the memory scheduler
    // (and the whole ERME store) resident forever.
    struct MemoryTickGuard {
        handle: tokio::task::JoinHandle<()>,
    }
    impl Drop for MemoryTickGuard {
        fn drop(&mut self) {
            self.handle.abort();
        }
    }
    let memory_tick_handle = {
        let scheduler = std::sync::Arc::clone(&memory_scheduler);
        MemoryTickGuard {
            handle: tokio::spawn(async move {
                let mut tick = tokio::time::interval(std::time::Duration::from_secs(30));
                loop {
                    tick.tick().await;
                    scheduler.drain().await;
                }
            }),
        }
    };
    loop_config.memory_scheduler = Some(std::sync::Arc::clone(&memory_scheduler));
    loop_config.checkpoint_interval = 3; // save every 3 turns
    if resume {
        loop_config.resume_from = loop_config.checkpoint_dir.clone();
    }
    loop_config.trust_path = Some(
        project_ga_dir
            .join("trust.json")
            .to_string_lossy()
            .to_string(),
    );

    // Wire compression logs to the log file so the monitor can see them.
    // Without this, tracing::warn! only goes to stderr.
    loop_config.log_fn = Some(std::sync::Arc::new(|msg: &str| {
        crate::debug_log(msg);
    }));

    // Wire up the intervention channel so the user can inject messages while agent runs
    let intervention_queue: Arc<
        Mutex<std::collections::VecDeque<oz_core::checkpoint::InterventionEvent>>,
    > = Arc::new(Mutex::new(std::collections::VecDeque::new()));
    loop_config.intervention_rx = Some(intervention_queue.clone());
    lock_poison_guard(&state.intervention_queues)
        .insert(session_id.to_string(), intervention_queue);

    // Pick summary model: explicit config > auto-detect first local model
    if let Some(ref name) = cfg.summary_model {
        // Search by section name first, then by model field value
        let found = cfg.get(name).or_else(|| {
            cfg.sessions
                .iter()
                .find(|(_, s)| s.model == *name)
                .map(|(n, s)| {
                    debug_log(&format!(
                        "compression summary: found '{}' via model field match",
                        n
                    ));
                    s
                })
        });
        if let Some(sc) = found {
            // Use the actual model name (sc.model), not the config key,
            // because the API expects the real model identifier.
            loop_config.summary_model_name = Some(sc.model.clone());
            loop_config.summary_apibase = Some(sc.apibase.clone());
            loop_config.summary_apikey = Some(sc.apikey.clone());
            debug_log(&format!(
                "compression summary model (explicit): {} ({}) @ {}",
                name, sc.model, sc.apibase
            ));
        } else {
            debug_log(&format!("WARNING: summary_model '{}' not found (neither section nor model field), falling back to auto-detect", name));
        }
    }
    if loop_config.summary_model_name.is_none() {
        if let Some((name, sc)) = cfg
            .sessions
            .iter()
            .find(|(_, s)| crate::is_local_deploy(&s.apibase))
        {
            loop_config.summary_model_name = Some(sc.model.clone());
            loop_config.summary_apibase = Some(sc.apibase.clone());
            loop_config.summary_apikey = Some(sc.apikey.clone());
            debug_log(&format!(
                "compression summary model (auto): {} ({}) @ {}",
                name, sc.model, sc.apibase
            ));
        }
    }

    let trust_store = oz_safety::TrustStore::new(Some(project_ga_dir.join("trust.json")));
    let mut guard = oz_safety::SafetyGuard::new(trust_store);
    // B7a: project trust level from {data_dir}/trust.json gates coarse
    // capabilities (execution / writes) per project root. Default Full.
    let trust_level = oz_safety::project_trust(&data_dir(), &project_root.to_string_lossy());
    if trust_level != oz_safety::ProjectTrustLevel::Full {
        debug_log(&format!(
            "project trust level for {}: {:?}",
            project_root.display(),
            trust_level
        ));
        guard = guard.with_project_trust(trust_level);
    }
    let permissions = match &profile.permission_file {
        Some(f) => {
            let path = if f.is_absolute() {
                f.clone()
            } else {
                data_dir().join(f)
            };
            debug_log(&format!(
                "permission policy (profile {}): loading {}",
                profile.name,
                path.display()
            ));
            oz_safety::Permissions::from_toml(&std::fs::read_to_string(&path).unwrap_or_default())
        }
        None => oz_safety::Permissions::load_from_dir(&data_dir()),
    };
    if !permissions.rules.is_empty() {
        debug_log(&format!(
            "permission policy loaded: {} rule(s)",
            permissions.rules.len(),
        ));
        guard = guard.with_permissions(permissions);
    }
    loop_config.safety_guard = Some(Arc::new(guard));
    if let Some(hooks) = oz_core::TomlHooks::load_from_dir(&data_dir()) {
        debug_log(&format!(
            "hooks loaded from {}",
            data_dir().join("hooks.toml").display()
        ));
        loop_config.hooks = Some(Arc::new(hooks));
    }
    // P2-8: surface compile diagnostics in the startup reminder.
    loop_config.include_diagnostics = true;
    loop_config.approval_handler = lock_poison_guard(&state.approval_handler).clone();

    // Wire ask_user reply slot
    {
        let mut ask_rxs = lock_poison_guard(&state.ask_user_rxs);
        let slot = ask_rxs
            .entry(session_id.to_string())
            .or_insert_with(|| Arc::new(Mutex::new(None)));
        *lock_poison_guard(slot) = None;
        loop_config.ask_user_rx = Some(slot.clone());
    }

    // Event channel: capture streaming events from the agent loop
    let (event_tx, mut event_rx) =
        tokio::sync::mpsc::unbounded_channel::<oz_core_types::StreamEvent>();
    let collected_events: Arc<Mutex<Vec<oz_core_types::StreamEvent>>> =
        Arc::new(Mutex::new(Vec::new()));
    let event_arrival_ms: Arc<Mutex<Vec<u64>>> = Arc::new(Mutex::new(Vec::new()));
    let start_ms: Arc<Mutex<Option<u64>>> = Arc::new(Mutex::new(None));

    let collector_handle: tokio::task::JoinHandle<()> = {
        let sid = session_id.to_string();
        let app_for_collector = app.clone();
        let events_for_collector = collected_events.clone();
        let arrivals_for_collector = event_arrival_ms.clone();
        let start_for_collector = start_ms.clone();
        let lang_for_collector = ctx.lang.clone();
        tokio::spawn(async move {
            while let Some(event) = event_rx.recv().await {
                let event = truncate_stream_event(event);
                let now_ms = std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map(|d| d.as_millis() as u64)
                    .unwrap_or(0);
                {
                    let mut s = lock_poison_guard(&start_for_collector);
                    if s.is_none() {
                        *s = Some(now_ms);
                    }
                }
                let base = lock_poison_guard(&start_for_collector).unwrap_or(now_ms);
                let arr = now_ms.saturating_sub(base);
                // Coalesce per-token deltas into their block so the collected
                // Vec stays O(blocks) — a long streamed run must not accumulate
                // one entry (and later one persisted streamEvents entry) per
                // token. Arrival samples stay index-aligned by skipping the
                // push for merged events.
                let merged = {
                    let mut events = lock_poison_guard(&events_for_collector);
                    oz_core_types::append_coalesced(&mut events, event.clone())
                };
                if !merged {
                    lock_poison_guard(&arrivals_for_collector).push(arr);
                }

                // ask_user pending → system notification so the user knows
                // the agent is waiting even when the app is in the background.
                if let oz_core_types::StreamEvent::AskUserPending { data } = &event {
                    let question = serde_json::from_str::<serde_json::Value>(data)
                        .ok()
                        .and_then(|v| {
                            v.pointer("/payload/question")
                                .or_else(|| v.get("question"))
                                .and_then(|q| q.as_str())
                                .map(|s| s.to_string())
                        })
                        .unwrap_or_default();
                    let question = truncate_chars(&question, 80);
                    let (title, body) = if lang_for_collector == "zh" {
                        ("OpenZen 需要你的回答", format!("等待你的回答：{question}"))
                    } else {
                        (
                            "OpenZen needs you",
                            format!("Waiting for your reply: {question}"),
                        )
                    };
                    crate::notify_if_unfocused(&app_for_collector, title, &body);
                }

                if !matches!(event, oz_core_types::StreamEvent::ToolCallReady { .. }) {
                    if let Ok(value) = serde_json::to_value(&event) {
                        let sse_ev = SseEvent::protocol_v1_json(&sid, &value);
                        let _ = app_for_collector.emit(
                            "sse_event",
                            serde_json::to_value(&sse_ev).unwrap_or_default(),
                        );
                    }
                }
            }
        })
    };

    let event_tx_for_after = event_tx.clone();
    loop_config.event_tx = Some(event_tx);

    // Stop signal
    let stop_signal = Arc::new(AtomicBool::new(false));
    {
        let mut map = lock_poison_guard(&state.stop_signals);
        map.insert(session_id.to_string(), stop_signal.clone());
    }

    let run_start = std::time::Instant::now();

    let outcome = oz_core::agent_loop::run_agent_loop(
        &mut client,
        system_prompt,
        user_message,
        history,
        &mut handler,
        &definitions,
        &ctx,
        &loop_config,
        &stop_signal,
    )
    .await;

    // Drain any final memory jobs, then stop the 30s drain tick task.
    memory_scheduler.drain().await;
    drop(memory_tick_handle);
    {
        let err_msg = outcome
            .data
            .as_ref()
            .and_then(|d| d.get("error"))
            .and_then(|v| v.as_str());
        let full_len = outcome
            .data
            .as_ref()
            .and_then(|d| d.get("full_response"))
            .and_then(|v| v.as_str())
            .map(|s| s.len())
            .unwrap_or(0);
        debug_log(&format!(
            "agent outcome: exit_reason={} turn={} error={} full_len={}",
            outcome.exit_reason,
            outcome.turn,
            err_msg.unwrap_or("(none)"),
            full_len,
        ));
    }

    // Send terminal event through the event channel
    if let Some(ref err_msg) = outcome
        .data
        .as_ref()
        .and_then(|d| d.get("error"))
        .and_then(|v| v.as_str())
    {
        let _ = event_tx_for_after.send(oz_core_types::StreamEvent::Error {
            message: err_msg.to_string(),
        });
    } else {
        let _ = event_tx_for_after.send(oz_core_types::StreamEvent::FinishMessage {
            stop_reason: outcome.exit_reason.clone(),
        });
    }
    drop(event_tx_for_after);
    loop_config.event_tx.take();
    let _ = collector_handle.await;

    {
        let mut map = lock_poison_guard(&state.stop_signals);
        map.remove(session_id);
    }

    // Mark idle and persist assistant message
    let full_response = outcome
        .data
        .as_ref()
        .and_then(|d| d.get("full_response"))
        .and_then(|v| v.as_str())
        .map(|s| s.to_string());
    {
        let mut store = lock_poison_guard(&state.sessions);
        if let Some(s) = store.get_mut(session_id) {
            s.status = SessionStatus::Idle;
            {
                let has_events = !lock_poison_guard(&collected_events).is_empty();
                let full = full_response.as_deref().unwrap_or("");
                // When the agent is stopped mid-stream, full_response
                // may be empty even though the UI already rendered text
                // deltas (the stream parser emits events in real time but
                // full_response is only populated after stream completes).
                // Reconstruct content from TextDelta events so the saved
                // message shows what the user already saw.
                let display_content: String = if full.is_empty() && has_events {
                    let events = lock_poison_guard(&collected_events);
                    events
                        .iter()
                        .filter_map(|e| match e {
                            oz_core_types::StreamEvent::TextDelta { text, .. } => {
                                Some(text.as_str())
                            }
                            _ => None,
                        })
                        .collect::<Vec<&str>>()
                        .join("")
                } else {
                    full.to_string()
                };
                if has_events || !display_content.is_empty() {
                    let now = chrono::Utc::now();
                    let mut msg = serde_json::json!({
                        "role": "assistant",
                        "content": display_content,
                        "timestamp": now.to_rfc3339(),
                    });

                    let stream_events_json: Vec<serde_json::Value> = {
                        let events = lock_poison_guard(&collected_events);
                        let arrivals = lock_poison_guard(&event_arrival_ms);
                        let now_ms = std::time::SystemTime::now()
                            .duration_since(std::time::UNIX_EPOCH)
                            .map(|d| d.as_millis() as u64)
                            .unwrap_or(0);
                        events
                            .iter()
                            .enumerate()
                            .filter_map(|(i, e)| {
                                let mut v = serde_json::to_value(e).ok()?;
                                let arr_i = arrivals.get(i).copied().unwrap_or(0);
                                let next_arr = arrivals.get(i + 1).copied().unwrap_or(now_ms);
                                let dur = next_arr.saturating_sub(arr_i);
                                if let Some(obj) = v.as_object_mut() {
                                    obj.insert(
                                        "duration_ms".to_string(),
                                        serde_json::Value::Number(dur.into()),
                                    );
                                }
                                Some(v)
                            })
                            .collect()
                    };
                    if !stream_events_json.is_empty() {
                        msg["streamEvents"] = serde_json::Value::Array(stream_events_json);
                    }

                    let dur = run_start.elapsed().as_millis() as u64;
                    if dur > 0 {
                        msg["duration"] = serde_json::json!(dur);
                    }

                    msg["modelInfo"] = serde_json::json!({
                        "model": sess_config.model,
                        "provider": provider,
                        "contextWindow": sess_config.context_win,
                        "isLocal": crate::is_local_deploy(&sess_config.apibase),
                    });

                    msg["exitReason"] = serde_json::json!(outcome.exit_reason);

                    if let Some(ref data) = outcome.data {
                        if let Some(thinking) = data.get("full_thinking").and_then(|v| v.as_str()) {
                            if !thinking.is_empty() {
                                msg["thinking"] = serde_json::Value::String(thinking.to_string());
                            }
                        }
                        if let Some(tools) = data.get("tool_calls").and_then(|v| v.as_array()) {
                            if !tools.is_empty() {
                                msg["toolCalls"] = serde_json::Value::Array(tools.clone());
                            }
                        }
                        if let Some(ti) = data.get("input_tokens_est").and_then(|v| v.as_u64()) {
                            msg["tokensIn"] = serde_json::Value::Number(ti.into());
                        }
                        if let Some(to) = data.get("output_tokens_est").and_then(|v| v.as_u64()) {
                            msg["tokensOut"] = serde_json::Value::Number(to.into());
                        }
                        if let Some(ct) = data.get("context_tokens_est").and_then(|v| v.as_u64()) {
                            msg["contextTokens"] = serde_json::Value::Number(ct.into());
                        }
                    }

                    // Embed ToolUse blocks (id + name + input) directly on the
                    // assistant message so the next agent run can reconstruct
                    // the tool_use ↔ tool_result pairing mandated by the chat-
                    // completion protocol. Without this, the prior assistant
                    // turn is just `assistant(text)` and any ToolResult blocks
                    // would be rejected by the API.
                    //
                    // IMPORTANT: Deduplicate by tool_call_id — the agent loop
                    // can emit multiple ToolInputAvailable for the same tool
                    // (speculative execution + regular execution), which would
                    // create unmatched tool_use ↔ tool_result pairs and cause
                    // the LLM to repeat the previous task on the next run.
                    {
                        let events = lock_poison_guard(&collected_events);
                        let mut seen_ids = std::collections::HashSet::new();
                        let tool_use_blocks: Vec<serde_json::Value> = events
                            .iter()
                            .filter_map(|e| match e {
                                StreamEvent::ToolInputAvailable {
                                    tool_call_id,
                                    name,
                                    args,
                                } => {
                                    let id_str = tool_call_id.as_str();
                                    if id_str.is_empty() {
                                        return None;
                                    }
                                    // Deduplicate: keep only the first occurrence per tool_call_id
                                    if !seen_ids.insert(id_str.to_string()) {
                                        return None;
                                    }
                                    let input: serde_json::Value = serde_json::from_str(args)
                                        .unwrap_or(serde_json::Value::Null);
                                    Some(serde_json::json!({
                                        "id": id_str,
                                        "name": name,
                                        "input": input,
                                    }))
                                }
                                _ => None,
                            })
                            .collect();
                        if !tool_use_blocks.is_empty() {
                            msg["tool_use_blocks"] = serde_json::Value::Array(tool_use_blocks);
                        }
                    }

                    s.messages.push(msg);

                    // Persist all ToolOutputAvailable blocks as ONE user-role
                    // message with `tool_results` (not as stand-alone
                    // role:"tool" messages, which break the Claude/OpenAI
                    // protocol pairing). Deduplicate by tool_call_id to
                    // prevent unmatched tool_use ↔ tool_result pairs.
                    {
                        let events = lock_poison_guard(&collected_events);
                        let mut seen_trids = std::collections::HashSet::new();
                        let tool_results: Vec<serde_json::Value> = events
                            .iter()
                            .filter_map(|e| match e {
                                StreamEvent::ToolOutputAvailable {
                                    tool_call_id,
                                    output,
                                    ..
                                } => {
                                    if tool_call_id.is_empty() {
                                        return None;
                                    }
                                    if !seen_trids.insert(tool_call_id.to_string()) {
                                        return None;
                                    }
                                    Some(serde_json::json!({
                                        "tool_use_id": tool_call_id,
                                        "content": output,
                                    }))
                                }
                                _ => None,
                            })
                            .collect();
                        if !tool_results.is_empty() {
                            let user_msg = serde_json::json!({
                                "role": "user",
                                "content": "",
                                "tool_results": tool_results,
                                "timestamp": now.to_rfc3339(),
                            });
                            s.messages.push(user_msg);
                        }
                    }
                }
            }
        }
        store.save();
    }

    // Send done event with token counts and full response
    let tokens_in: usize = outcome
        .data
        .as_ref()
        .and_then(|d| d.get("input_tokens_est"))
        .and_then(|v| v.as_u64())
        .unwrap_or(0) as usize;
    let tokens_out: usize = outcome
        .data
        .as_ref()
        .and_then(|d| d.get("output_tokens_est"))
        .and_then(|v| v.as_u64())
        .unwrap_or(0) as usize;
    let context_tokens: usize = outcome
        .data
        .as_ref()
        .and_then(|d| d.get("context_tokens_est"))
        .and_then(|v| v.as_u64())
        .unwrap_or(0) as usize;
    let done_evt = SseEvent::done(
        session_id,
        full_response.as_deref(),
        tokens_in,
        tokens_out,
        context_tokens,
        Some(&outcome.exit_reason),
    );
    let _ = app.emit(
        "sse_event",
        serde_json::to_value(&done_evt).unwrap_or_default(),
    );

    // Desktop notification (with sound) — skipped when the user is looking
    // at the main window, since the chat UI already shows the result.
    let summary = outcome
        .data
        .as_ref()
        .and_then(|d| d.get("full_response"))
        .and_then(|v| v.as_str())
        .map(|s| {
            if s.len() > 100 {
                let mut end = 100;
                while !s.is_char_boundary(end) {
                    end -= 1;
                }
                format!("{}...", &s[..end])
            } else {
                s.to_string()
            }
        })
        .unwrap_or_else(|| "Task completed".to_string());
    crate::notify_if_unfocused(app, "OpenZen", &summary);

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Minimal ERME store with the same configuration as the app
    /// (align_on_write for conflict resolution).
    fn test_store(
        dir: &std::path::Path,
    ) -> std::sync::Arc<entropy_memory_engine::memory_store::MemoryStore> {
        use entropy_memory_engine::consolidation::ConsolidationConfig;
        use entropy_memory_engine::l1::L1Cache;
        use entropy_memory_engine::l2::{HnswConfig, L2Config, L2Engine};
        use entropy_memory_engine::l3::{L3Config, L3Engine};
        use entropy_memory_engine::memory_store::MemoryStore;
        let l1 = L1Cache::builder().capacity(100).build();
        let l2 = L2Engine::new(L2Config {
            hnsw: HnswConfig {
                dimension: 384,
                ..Default::default()
            },
            ..Default::default()
        });
        let l3 = L3Engine::new(L3Config {
            storage_path: dir.join("test.bin"),
            ..Default::default()
        });
        std::sync::Arc::new(MemoryStore::new(
            l1,
            std::sync::Arc::new(l2),
            l3,
            ConsolidationConfig {
                align_on_write: true,
                ..Default::default()
            },
        ))
    }

    #[test]
    fn test_ingest_harness_entries_stores_lessons() {
        let dir = std::env::temp_dir().join(format!("oz-erme-ingest-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let harness_dir = dir.join("harness");
        oz_core::harness::refine(
            &harness_dir,
            oz_core::harness::HarnessKind::Memory,
            "always use --locked for reproducible builds",
            "seen two lockfile failures this session",
            "test",
            "upsert",
        )
        .unwrap();
        let store = test_store(&dir);

        let stored = ingest_harness_entries(&store, &harness_dir);
        assert_eq!(stored, 1, "one ledger lesson must be ingested");

        let recalls = store.recall_by_text("locked build", 5).unwrap_or_default();
        assert!(!recalls.is_empty(), "ingested lesson must be recallable");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_ingest_harness_entries_idempotent() {
        let dir = std::env::temp_dir().join(format!("oz-erme-ingest-idem-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let harness_dir = dir.join("harness");
        oz_core::harness::refine(
            &harness_dir,
            oz_core::harness::HarnessKind::Memory,
            "deploy with `--no-verify` only after tests pass",
            "deploy failed once when skipping tests",
            "test",
            "upsert",
        )
        .unwrap();
        let store = test_store(&dir);

        let first = ingest_harness_entries(&store, &harness_dir);
        assert_eq!(first, 1);
        let second = ingest_harness_entries(&store, &harness_dir);
        assert_eq!(second, 0, "re-ingestion must skip already-present lessons");
        let recalls = store
            .recall_by_text("deploy no-verify", 5)
            .unwrap_or_default();
        assert_eq!(recalls.len(), 1, "no duplicates in the store");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
