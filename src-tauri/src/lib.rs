use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use oz_core_types::ToolContext;
use oz_platform::{AgentBridge, PlatformAdapter, PlatformContext, PlatformRegistry};
use oz_server::webui::sessions::SessionStore;
use oz_server::webui::sse_bus::SseEvent;
use oz_skill_mcp::SKILL_MCP_DIR;
use serde::Serialize;
use tauri::menu::{MenuBuilder, MenuItemBuilder};
use tauri::tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent};
use tauri::{AppHandle, Emitter, Manager};
use tokio::task::JoinHandle;

mod approval;
#[cfg(test)]
mod import_write_path_tests;
mod plans;
pub mod projects;
mod sidepanel;
mod subagents;

const SESSION_STATE_FILE: &str = "openzen/sessions.json";

/// Size-based rotation for the debug log so a 7x24 run can't grow
/// openzen.log without bound. Keeps openzen.log + one rotated copy.
const MAX_DEBUG_LOG_BYTES: u64 = 20 * 1024 * 1024;

struct DebugLogWriter {
    path: PathBuf,
    file: Option<std::io::BufWriter<std::fs::File>>,
    written: u64,
}

impl DebugLogWriter {
    fn write(&mut self, msg: &str) {
        let line = format!(
            "[{}] [openzen] {}\n",
            chrono::Local::now().format("%Y-%m-%d %H:%M:%S"),
            msg.trim_end()
        );
        self.write_raw(&line);
    }

    /// Append a pre-formatted line (tracing events already carry level
    /// and timestamp) under the same size-based rotation.
    fn write_raw(&mut self, line: &str) {
        use std::io::Write as _;
        if self.file.is_none() || self.written + line.len() as u64 > MAX_DEBUG_LOG_BYTES {
            self.rotate();
        }
        if let Some(f) = self.file.as_mut() {
            if f.write_all(line.as_bytes()).is_ok() {
                self.written += line.len() as u64;
            }
        }
    }

    fn rotate(&mut self) {
        self.file = None;
        // Keep 7 generations (log.1 … log.7) instead of a single `.log.1`:
        // the effective retention window was one rotation, which on a busy
        // day is hours, not the "40 MB" the size cap suggests
        // (round5 P1-10 ③).
        for i in (1..7).rev() {
            let from = if i == 1 {
                self.path.with_extension("log.1")
            } else {
                self.path.with_extension(format!("log.{i}"))
            };
            let to = self.path.with_extension(format!("log.{}", i + 1));
            if from.exists() {
                let _ = std::fs::remove_file(&to);
                let _ = std::fs::rename(&from, &to);
            }
        }
        let _ = std::fs::rename(&self.path, self.path.with_extension("log.1"));
        self.file = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.path)
            .ok()
            .map(std::io::BufWriter::new);
        self.written = 0;
    }

    fn flush(&mut self) {
        use std::io::Write as _;
        if let Some(f) = self.file.as_mut() {
            let _ = f.flush();
        }
    }

    /// Append one line (used by the panic hook, which must not go through
    /// the tracing subscriber).
    fn write_line(&mut self, line: &str) {
        use std::io::Write as _;
        if let Some(f) = self.file.as_mut() {
            let _ = writeln!(f, "{line}");
        }
    }
}

static DEBUG_LOG_WRITER: std::sync::OnceLock<std::sync::Mutex<DebugLogWriter>> =
    std::sync::OnceLock::new();

/// Routes tracing events into the same size-rotated file as debug_log so
/// 7x24 log output is persisted AND bounded (stderr is invisible in a GUI
/// app and grew unbounded where redirected).
struct RotatedLogMakeWriter;

struct RotatedLogWriter;

impl std::io::Write for RotatedLogWriter {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        let text = String::from_utf8_lossy(buf);
        if !text.trim().is_empty() {
            rotated_log_append(&text);
        }
        Ok(buf.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

impl<'a> tracing_subscriber::fmt::MakeWriter<'a> for RotatedLogMakeWriter {
    type Writer = RotatedLogWriter;
    fn make_writer(&'a self) -> Self::Writer {
        RotatedLogWriter
    }
}

fn rotated_log_append(line: &str) {
    let writer = DEBUG_LOG_WRITER.get_or_init(|| {
        let log_dir = data_dir().join("logs");
        let _ = std::fs::create_dir_all(&log_dir);
        std::sync::Mutex::new(DebugLogWriter {
            path: log_dir.join("openzen.log"),
            file: None,
            written: 0,
        })
    });
    if let Ok(mut w) = writer.lock() {
        w.write_raw(line);
    }
}

pub(crate) fn debug_log(msg: &str) {
    // One shared buffered writer for the whole process — the previous
    // per-line open/append/close also serialized on the FS on the hot path.
    let writer = DEBUG_LOG_WRITER.get_or_init(|| {
        let log_dir = data_dir().join("logs");
        let _ = std::fs::create_dir_all(&log_dir);
        std::sync::Mutex::new(DebugLogWriter {
            path: log_dir.join("openzen.log"),
            file: None,
            written: 0,
        })
    });
    if let Ok(mut w) = writer.lock() {
        w.write(msg);
    }
}

/// Flush the buffered debug log — used by the frontend log bridge so
/// release-build diagnosis doesn't sit in an unflushed 8KB BufWriter.
pub(crate) fn debug_log_flush() {
    let writer = DEBUG_LOG_WRITER.get_or_init(|| {
        let log_dir = data_dir().join("logs");
        let _ = std::fs::create_dir_all(&log_dir);
        std::sync::Mutex::new(DebugLogWriter {
            path: log_dir.join("openzen.log"),
            file: None,
            written: 0,
        })
    });
    if let Ok(mut w) = writer.lock() {
        w.flush();
    }
}

pub(crate) fn home_dir() -> PathBuf {
    std::env::var("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|_| PathBuf::from("/tmp"))
}

/// Runtime data root. Overridable via `OPENZEN_DATA_DIR` so a dev build can
/// run against an isolated data tree instead of the user's real one.
/// Otherwise the active profile's `data_dir` (P1-6) wins, falling back to
/// `~/.openzen`.
pub(crate) fn data_dir() -> PathBuf {
    if let Ok(d) = std::env::var("OPENZEN_DATA_DIR") {
        return PathBuf::from(d);
    }
    let profile = oz_config::load_profile();
    profile
        .data_dir
        .unwrap_or_else(|| home_dir().join(".openzen"))
}

/// Default agent working directory: `<data root>/workspace`.
///
/// Deliberately NOT `$HOME`. The working directory is the fence root for
/// `read` / `write` / `edit` / `patch` / `grep` / `glob` / `ls`, so falling
/// back to the home directory handed an unapproved tool call the entire
/// `$HOME` — dotfiles, `~/.npmrc`, `~/.netrc`, `~/.kube/config`, other
/// projects — and, combined with the免审批 `web_fetch`, a ready-made
/// exfiltration path (round5 P1-8d). Sessions without a project get this
/// sandboxed scratch directory instead, and the user can still point a
/// session at a real project directory explicitly.
pub(crate) fn default_workspace_dir() -> PathBuf {
    data_dir().join("workspace")
}

/// Directories that may hold checkpoint artifacts for a session: the data
/// root's workspace (the default working dir) plus the session's own project
/// directory. Used by delete-session cleanup and the startup orphan sweep.
pub(crate) fn checkpoint_dirs_for(working_dir: &Option<String>) -> Vec<std::path::PathBuf> {
    let mut dirs = vec![oz_core::checkpoint::checkpoint_dir(&default_workspace_dir())];
    if let Some(wd) = working_dir {
        let p = oz_core::checkpoint::checkpoint_dir(std::path::Path::new(wd));
        if !dirs.contains(&p) {
            dirs.push(p);
        }
    }
    dirs
}

/// Startup sweep: remove checkpoint/WAL files whose session no longer exists
/// in the store, and enforce the per-session byte budget. Runs in the
/// background — it is pure disk housekeeping and must never delay the window.
pub(crate) fn spawn_checkpoint_gc(state: &Arc<AppState>) {
    let state = Arc::clone(state);
    std::thread::Builder::new()
        .name("checkpoint-gc".into())
        .spawn(move || {
            let live: std::collections::HashSet<String> =
                lock_poison_guard(&state.sessions).list().into_iter().map(|s| s.id).collect();
            let dirs = {
                let store = lock_poison_guard(&state.sessions);
                let mut d = vec![oz_core::checkpoint::checkpoint_dir(&default_workspace_dir())];
                for info in store.list() {
                    if let Some(wd) = info.working_dir {
                        let p = oz_core::checkpoint::checkpoint_dir(std::path::Path::new(&wd));
                        if !d.contains(&p) {
                            d.push(p);
                        }
                    }
                }
                d
            };
            let mut removed = 0usize;
            let mut freed: u64 = 0;
            for dir in &dirs {
                if !dir.exists() {
                    continue;
                }
                freed += dir_size(dir);
                removed += oz_core::checkpoint::gc_orphan_checkpoints(
                    dir,
                    &live,
                    std::time::Duration::from_secs(3600),
                );
            }
            if removed > 0 {
                tracing::info!(
                    "[gc] removed {removed} orphan checkpoint file(s); checkpoint dirs hold {:.1} MB",
                    freed as f64 / (1024.0 * 1024.0)
                );
            }
        })
        .map_err(|e| tracing::warn!("failed to spawn checkpoint gc: {e}"))
        .ok();
}

/// Recursive byte size of a directory (best effort).
fn dir_size(dir: &std::path::Path) -> u64 {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return 0;
    };
    entries
        .flatten()
        .map(|e| {
            e.metadata()
                .map(|m| {
                    if m.is_dir() {
                        dir_size(&e.path())
                    } else {
                        m.len()
                    }
                })
                .unwrap_or(0)
        })
        .sum()
}

/// Resolve the mykey.toml config path: explicit state path first, then the
/// data-root / home / repo / cwd fallback chain. Shared between the ERME
/// backend gate (AppState::new) and the agent runner so both always see the
/// same config — a config living at a fallback path must not silently flip
/// the memory backend decision.
pub(crate) fn resolve_config_path(state_config_path: impl AsRef<std::path::Path>) -> PathBuf {
    let explicit = state_config_path.as_ref();
    if explicit.exists() {
        return explicit.to_path_buf();
    }
    [
        data_dir().join("mykey.toml"),
        home_dir().join("mykey.toml"),
        PathBuf::from("config/mykey.toml"),
        PathBuf::from("mykey.toml"),
    ]
    .into_iter()
    .find(|c| c.exists())
    .unwrap_or_else(|| explicit.to_path_buf())
}

/// Harness ledger directory under the data root (never the source tree).
pub(crate) fn harness_dir() -> PathBuf {
    data_dir().join("harness")
}

/// Shared L2 engine construction (384-dim HNSW semantic index). One place so
/// a dimension/metric change applies everywhere.
fn l2_engine() -> Arc<entropy_memory_engine::l2::L2Engine> {
    use entropy_memory_engine::l2::{HnswConfig, L2Config};
    Arc::new(entropy_memory_engine::l2::L2Engine::new(L2Config {
        hnsw: HnswConfig {
            dimension: 384,
            ..Default::default()
        },
        ..Default::default()
    }))
}

/// Long-lived ERME runtime: semantic store + L0 soul layer (M7).
///
/// The reflection engine subscribes to store events via [`MemoryStore::attach_soul`]
/// and runs curiosity-driven introspection on an idle interval; its soul model
/// is exposed through [`PromptInjector::build_system_prefix`] so the agent loop
/// injects the soul state into every system prompt.
pub struct ErmeRuntime {
    pub store: Arc<entropy_memory_engine::memory_store::MemoryStore>,
    pub reflection: Arc<entropy_memory_engine::l0::ReflectionEngine>,
    pub injector: entropy_memory_engine::l0::PromptInjector,
}

/// Build the long-lived ERME semantic memory store rooted at `base_dir`.
///
/// Storage lands in `{base_dir}/memory_erme/erme_memory.bin` (sibling of the
/// legacy `memory/` tree). Returns `None` on failure so the app degrades to
/// the file backend instead of crashing at startup.
#[allow(clippy::field_reassign_with_default)]
fn init_erme_store(
    base_dir: &std::path::Path,
    idle_interval_secs: u64,
    last_user_activity: std::sync::Arc<std::sync::atomic::AtomicU64>,
) -> Option<Arc<ErmeRuntime>> {
    use entropy_memory_engine::consolidation::ConsolidationConfig;
    use entropy_memory_engine::l0::soul::{SoulHandle, SoulModel};
    use entropy_memory_engine::l0::{PromptInjector, ReflectionConfig, ReflectionEngine};
    use entropy_memory_engine::l1::L1Cache;
    use entropy_memory_engine::l3::{BudgetConfig, L3Config, L3Engine};
    use entropy_memory_engine::memory_store::MemoryStore;
    use entropy_memory_engine::orchestrator::MemoryOrchestrator;
    use entropy_memory_engine::phase1::ConflictResolver;
    use entropy_memory_engine::phase2::{RamblingConfig, RamblingEngine};
    use entropy_memory_engine::phase4::{QuarantineConfig, QuarantineManager};
    use std::sync::RwLock;

    let memory_erme_dir = base_dir.join("memory_erme");
    if std::fs::create_dir_all(&memory_erme_dir).is_err() {
        return None;
    }

    let l1 = L1Cache::builder().capacity(10_000).build();
    let l2 = l2_engine();
    let l3 = L3Engine::new(L3Config {
        storage_path: memory_erme_dir.join("erme_memory.bin"),
        budget: BudgetConfig {
            daily_token_limit: 256_000,
            annual_storage_limit: 50_000_000,
        },
        compression_max_chars: 400,
        ..Default::default()
    });

    let store = Arc::new(MemoryStore::new(
        l1,
        Arc::clone(&l2),
        l3,
        ConsolidationConfig {
            align_on_write: true,
            ..Default::default()
        },
    ));

    // L2 (HNSW + TimeGraph) is memory-only by design: after every restart
    // the semantic layer was empty until new writes trickled in, so recall
    // collapsed to the keyword path. Rebuild it from the persisted L3
    // snapshot in a background thread (importance-descending, superseded
    // entries skipped) — startup is not blocked and the index converges.
    {
        let store_for_backfill = Arc::clone(&store);
        std::thread::Builder::new()
            .name("erme-l2-backfill".into())
            .spawn(move || {
                let started = std::time::Instant::now();
                let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    let mut memories = store_for_backfill
                        .router()
                        .l3_engine()
                        .storage()
                        .all()
                        .into_iter()
                        .filter(|m| m.metadata.superseded_by.is_none())
                        .collect::<Vec<_>>();
                    memories.sort_by(|a, b| {
                        b.metadata
                            .importance
                            .partial_cmp(&a.metadata.importance)
                            .unwrap_or(std::cmp::Ordering::Equal)
                    });
                    let l2 = store_for_backfill.router().l2_engine();
                    let mut inserted = 0usize;
                    for m in memories {
                        let input = entropy_memory_engine::core::MemoryInput {
                            content: m.content.clone(),
                            importance: m.metadata.importance,
                            alias: m.alias.clone(),
                            tags: m.tags.clone(),
                            layer: entropy_memory_engine::core::LayerId::L2,
                        };
                        if l2.insert_with_id(input, m.id).is_ok() {
                            inserted += 1;
                        }
                    }
                    inserted
                }));
                match result {
                    Ok(inserted) if inserted > 0 => tracing::info!(
                        "ERME L2 backfill: {inserted} entries re-indexed in {:.1}s",
                        started.elapsed().as_secs_f64()
                    ),
                    Ok(_) => {}
                    Err(panic) => tracing::error!("ERME L2 backfill panicked: {panic:?}"),
                }
            })
            .map_err(|e| tracing::error!("failed to spawn ERME L2 backfill thread: {e}"))
            .ok();
    }

    let conflict_resolver = Arc::new(ConflictResolver::new(l2_engine()));

    let quarantine = Arc::new(QuarantineManager::new(
        l2_engine(),
        QuarantineConfig::default(),
    ));

    // L0 灵魂模型：从磁盘加载（首次运行则新建默认模型）。
    let soul_path = memory_erme_dir.join("soul.json");
    let soul: SoulHandle = Arc::new(RwLock::new(
        SoulModel::load_from(&soul_path).unwrap_or_default(),
    ));

    // 联想引擎与 store 共享同一 L2 语义图（含时间图 seed），
    // 并被 orchestrator 与 reflection 共用同一实例（状态不分裂）。
    let rambling = Arc::new(RamblingEngine::new(
        RamblingConfig::default(),
        Arc::clone(&l2.graph),
        Arc::clone(&l2),
    ));

    let orchestrator = Arc::new(
        MemoryOrchestrator::new(
            Arc::clone(&store),
            Arc::clone(&conflict_resolver),
            quarantine,
        )
        .with_idle_cycle(Arc::clone(&rambling)),
    );
    store.attach_orchestrator(Arc::clone(&orchestrator));

    // L0 反思引擎：订阅 store 事件，共享 orchestrator 的行为观察日志与
    // 联想引擎；画像持久化到 soul.json（与加载路径一致，进程内升级可复用）。
    let observer = Arc::new(orchestrator.observer().clone());
    let mut reflection_cfg = ReflectionConfig::default();
    reflection_cfg.idle_interval_secs = idle_interval_secs;
    let reality_anchor = std::sync::Arc::new(entropy_memory_engine::phase4::RealityAnchor::new());
    let reflection = Arc::new(
        ReflectionEngine::new(
            Arc::clone(&soul),
            Arc::clone(&store),
            observer,
            conflict_resolver,
            reflection_cfg,
        )
        .with_rambling(Arc::clone(&rambling))
        // Without an anchor, high-sss rambling conjectures entered
        // self_portrait unverified — the anchor gates them against
        // persisted memories first.
        .with_reality_anchor(std::sync::Arc::clone(&reality_anchor))
        .with_persist_path(soul_path),
    );
    store.attach_soul(Arc::clone(&reflection));

    let injector = PromptInjector::new(Arc::clone(&soul));

    // 后台内省循环：idle 间隔驱动 L0 完整内省 + Phase2-5 idle 管道。
    // Hardened: a panic inside a cycle is caught and logged so the thread
    // never dies silently (the soul would stop evolving with no signal).
    {
        let reflection = Arc::clone(&reflection);
        let orchestrator = Arc::clone(&orchestrator);
        let last_user_activity = std::sync::Arc::clone(&last_user_activity);
        let store_for_maintenance = Arc::clone(&store);
        let mut last_daily_maintenance_nanos: i64 = 0;
        std::thread::Builder::new()
            .name("erme-idle-cycle".into())
            .spawn(move || loop {
                std::thread::sleep(std::time::Duration::from_secs(idle_interval_secs));
                // Skip the soul cycle while the user is actively
                // conversing: it competes with the main agent for
                // MLX/CPU and mid-conversation is not "idle".
                let last = last_user_activity.load(std::sync::atomic::Ordering::Relaxed);
                if last > 0 {
                    let now = std::time::SystemTime::now()
                        .duration_since(std::time::UNIX_EPOCH)
                        .map(|d| d.as_secs())
                        .unwrap_or(0);
                    if now.saturating_sub(last) < idle_interval_secs {
                        continue;
                    }
                }
                let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    // Drain the event queue first: portrait/relationship
                    // updates (UserStatement, MemoryStored, …) only happen
                    // in process_pending — without this call the queue
                    // grows forever and the portraits never evolve.
                    let drained = reflection.process_pending();
                    if drained > 0 {
                        tracing::debug!("ERME reflection consumed {drained} pending events");
                    }
                    reflection.run_full_cycle();
                    orchestrator.run_idle_cycle();
                    // Daily maintenance: recursive consolidation merges
                    // similar entries and applies the forgetting strategy
                    // (nothing else ever forgot, so the store only grew).
                    const TWENTY_FOUR_H_NANOS: i64 = 24 * 3600 * 1_000_000_000;
                    if entropy_memory_engine::core::now_nanos() - last_daily_maintenance_nanos
                        > TWENTY_FOUR_H_NANOS
                    {
                        last_daily_maintenance_nanos =
                            entropy_memory_engine::core::now_nanos();
                        let stats = store_for_maintenance.consolidate_recursive();
                        tracing::info!(
                            "ERME daily consolidation: rounds={} merged={} deduped={} forgotten_l2={} forgotten_l3={}",
                            stats.rounds.len(),
                            stats.total_merged,
                            stats.total_deduped,
                            stats.total_forgotten_l2,
                            stats.total_forgotten_l3
                        );
                    }
                }));
                if let Err(panic) = outcome {
                    tracing::error!(
                        "ERME idle cycle panicked (soul evolution paused): {:?}",
                        panic
                    );
                }
            })
            .map_err(|e| tracing::error!("failed to spawn ERME idle thread: {e}"))
            .ok();
    }

    tracing::info!(
        "ERME memory engine initialised at {} (idle cycle every {}s)",
        memory_erme_dir.display(),
        idle_interval_secs
    );
    Some(Arc::new(ErmeRuntime {
        store,
        reflection,
        injector,
    }))
}

fn load_locale() -> String {
    let path = data_dir().join("locale.json");
    if let Ok(content) = std::fs::read_to_string(&path) {
        if let Ok(parsed) = serde_json::from_str::<serde_json::Value>(&content) {
            if let Some(lang) = parsed.get("lang").and_then(|v| v.as_str()) {
                match lang {
                    "zh" | "en" => return lang.to_string(),
                    _ => {}
                }
            }
        }
    }
    // Detect system locale; fall back to "en" (international default)
    // rather than hardcoding "zh" which confuses non-Chinese users.
    #[cfg(target_os = "macos")]
    {
        if std::env::var("LANG").unwrap_or_default().starts_with("zh") {
            "zh".to_string()
        } else {
            "en".to_string()
        }
    }
    #[cfg(not(target_os = "macos"))]
    {
        if std::env::var("LANG")
            .unwrap_or_default()
            .to_lowercase()
            .contains("zh")
        {
            "zh".to_string()
        } else {
            "en".to_string()
        }
    }
}

pub(crate) fn find_assets_dir() -> PathBuf {
    if let Ok(exe) = std::env::current_exe() {
        if let Some(exe_dir) = exe.parent() {
            let candidate = exe_dir.join("assets");
            if candidate.is_dir() {
                return candidate;
            }
            let candidate = exe_dir.join("../Resources/assets");
            if candidate.is_dir() {
                return candidate;
            }
        }
    }
    if let Ok(cwd) = std::env::current_dir() {
        let candidate = cwd.join("assets");
        if candidate.is_dir() {
            return candidate;
        }
    }
    let candidate = data_dir().join("assets");
    if candidate.is_dir() {
        return candidate;
    }
    home_dir()
}

pub(crate) fn tauri_ctx() -> ToolContext {
    // Agent working dir lives under the data root, never the source tree.
    let working_dir = data_dir().join("workspace");
    let working_dir = working_dir.to_string_lossy().to_string();
    let assets_dir = find_assets_dir().to_string_lossy().to_string();
    let _ = std::fs::create_dir_all(&working_dir);

    let skill_mcp_dir = [data_dir().join(SKILL_MCP_DIR)]
        .into_iter()
        .chain(
            std::env::current_dir()
                .ok()
                .map(|cwd| cwd.join(SKILL_MCP_DIR)),
        )
        .find(|p| p.is_dir())
        .map(|p| p.to_string_lossy().to_string());

    // Harness ledger lives under the data root, never the source tree.
    let ledger_dir = harness_dir();
    let _ = std::fs::create_dir_all(&ledger_dir);

    ToolContext {
        working_dir,
        assets_dir: assets_dir.clone(),
        script_dir: assets_dir,
        lang: load_locale(),
        skill_mcp_dir,
        harness_dir: Some(ledger_dir.to_string_lossy().to_string()),
        session_id: String::new(),
        tool_budget_secs: 0,
        is_subagent: false,
    }
}

/// Load the system prompt based on the current locale stored in ctx.lang.
pub(crate) fn load_system_prompt(ctx: &ToolContext) -> String {
    let sys_prompt_filename = if ctx.lang == "en" {
        "sys_prompt_en.txt"
    } else {
        "sys_prompt.txt"
    };
    let sys_prompt_path = std::path::PathBuf::from(&ctx.assets_dir).join(sys_prompt_filename);
    if sys_prompt_path.exists() {
        std::fs::read_to_string(&sys_prompt_path).unwrap_or_default()
    } else {
        String::new()
    }
}

use crate::sidepanel::state::{ParkedTabs, SidePanelState};
use crate::sidepanel::terminal::TerminalRegistry;

type AskUserSlot = Arc<Mutex<HashMap<String, String>>>;

pub struct AppState {
    pub sessions: Arc<Mutex<SessionStore>>,
    pub stop_signals: Arc<Mutex<HashMap<String, Arc<AtomicBool>>>>,
    pub approval_handler: Arc<Mutex<Option<Arc<dyn oz_safety::ApprovalHandler>>>>,
    pub pending_approvals: approval::PendingApprovals,
    pub running_agents: Arc<Mutex<HashMap<String, JoinHandle<()>>>>,
    pub detached_agents: Arc<Mutex<HashMap<String, JoinHandle<()>>>>,
    pub ask_user_rxs: Arc<Mutex<HashMap<String, AskUserSlot>>>,
    pub sidepanel: Mutex<SidePanelState>,
    /// Tabs of the sessions the user is not currently in, keyed by session id
    /// (parked by `set_sidepanel_session`). Keeps A's artifacts off B's panel
    /// without losing them: switching back restores the set.
    pub sidepanel_sessions: Mutex<HashMap<String, ParkedTabs>>,
    pub html_roots: std::sync::Mutex<Vec<std::path::PathBuf>>,
    /// Files (and, for html artifacts, their parent directories) that were
    /// explicitly opened in the side panel. The read_file_* / parse_excel /
    /// get_git_diff / get_file_info / open_external_file commands only serve
    /// paths inside this whitelist, so the webview cannot read arbitrary
    /// files off disk.
    pub artifact_roots: std::sync::Mutex<Vec<std::path::PathBuf>>,
    pub terminal_registry: TerminalRegistry,
    pub config_path: String,
    pub working_dir: String,
    pub assets_dir: String,
    pub scheduler_started: AtomicBool,
    pub pending_reminders: Mutex<Vec<oz_core_types::Reminder>>,
    pub app_handle: Mutex<Option<AppHandle>>,
    pub reminder_tx: Mutex<Option<tokio::sync::mpsc::UnboundedSender<oz_core_types::Reminder>>>,
    pub locale: Arc<Mutex<String>>,
    pub skill_mcp_dir: Option<String>,
    /// Process-wide shared skill/SOP store (round3 P1-d): built once from
    /// `skill_mcp_dir`, kept parsed, and reloaded incrementally (mtime-gated)
    /// before each run instead of re-walking the tree per user message.
    pub shared_skill_store: SharedSkillStoreCell,
    pub projects: Mutex<Vec<projects::store::ProjectRecord>>,
    pub crystallization_enabled: AtomicBool,
    /// "完全访问" (full access): when set, the approval handler auto-allows
    /// every request so the agent never asks the user for permission.
    pub full_access: Arc<AtomicBool>,
    /// Runtime memory switch. Initialised from `mykey.toml`'s
    /// `memory_backend`, but flipped in-process so the soul tab's switch
    /// takes effect on the NEXT message instead of the next app start
    /// (round5 P1-12 ①: memory could only be toggled by editing the config
    /// and restarting).
    pub memory_enabled: Arc<AtomicBool>,
    /// Long-lived ERME runtime (semantic store + L0 soul layer).
    /// Created once at startup; None when construction failed.
    /// ERME runtime, initialized on a BACKGROUND thread: the L3 snapshot
    /// load + WAL replay + budget backfill is proportional to memory size
    /// and used to block the first window. Readers see None until the slot
    /// is filled (they already degrade to file memory / status=disabled);
    /// `erme()` is the accessor.
    pub erme_store: Arc<std::sync::OnceLock<Option<Arc<ErmeRuntime>>>>,
    /// Platform adapter registry, stored once started so the exit path can
    /// stop adapters (supervisors killed mid-await would otherwise leave WS
    /// connections and child processes behind).
    pub platform_registry: std::sync::Mutex<Option<Arc<PlatformRegistry>>>,
    /// Scheduler shutdown flag — set by graceful exit so maintenance tasks
    /// stop cleanly instead of racing the teardown.
    pub scheduler_shutdown: std::sync::Mutex<Option<Arc<AtomicBool>>>,
    /// Unix-seconds timestamp of the last user message; the ERME idle
    /// cycle skips itself while the user is actively conversing.
    pub last_user_activity: Arc<AtomicU64>,
    /// Completion notices for background jobs that finished while no run was
    /// live. Drained into the next run's user turn; otherwise a long job that
    /// outlives its turn would be invisible to the model forever.
    pub pending_job_notices: Mutex<HashMap<String, Vec<String>>>,
    /// parent session → live child agent sessions (for stop cascade).
    pub child_sessions: Mutex<HashMap<String, Vec<String>>>,
    pub intervention_queues: Mutex<
        HashMap<
            String,
            Arc<Mutex<std::collections::VecDeque<oz_core::checkpoint::InterventionEvent>>>,
        >,
    >,
    /// One-shot context injected when a session is resumed from a checkpoint
    /// (an ask_user reply that arrived after the app restarted). Consumed by
    /// `resume_session`; never rendered as a user bubble.
    pub resume_notes: Arc<Mutex<HashMap<String, String>>>,
    /// session_id → webview window label for dedicated session windows
    /// (`session-{id}`), used to route session-scoped events (approvals)
    /// to the owning window instead of broadcasting to all windows.
    pub session_windows: Arc<Mutex<HashMap<String, String>>>,
}

impl AppState {
    pub fn new() -> Self {
        let data_root = data_dir();
        // Agent working dir lives under the data root, never the source tree
        // and never $HOME (see `default_workspace_dir`).
        let working_dir = default_workspace_dir();
        let _ = std::fs::create_dir_all(&working_dir);

        // Data migration: ensure projects.json exists (first-run)
        {
            let pp = data_root.join("projects.json");
            if !pp.exists() {
                let _ = std::fs::write(&pp, "[]");
            }
        }

        // One-time migration: the pre-P0 harness ledger lived at
        // {data_root}/.skill_mcp/harness — move it to {data_root}/harness so
        // the unified ledger dir does not orphan historical lessons.
        {
            let old = data_root
                .join(SKILL_MCP_DIR)
                .join("harness")
                .join("harness_state.json");
            let new_dir = harness_dir();
            let new = new_dir.join("harness_state.json");
            if old.exists() && !new.exists() {
                let _ = std::fs::create_dir_all(&new_dir);
                match std::fs::copy(&old, &new) {
                    Ok(_) => tracing::info!("migrated harness ledger from {}", old.display()),
                    Err(e) => tracing::warn!("harness ledger migration failed: {e}"),
                }
            }
        }

        let state_path = data_root.join(SESSION_STATE_FILE);

        // ERME semantic memory: build the long-lived runtime only when the
        // memory backend is "erme" (the default). A "file" backend skips
        // construction entirely — no HNSW resident memory, no idle thread.
        // Uses the same config fallback chain as the runner so a config at a
        // fallback path never silently flips this decision.
        let config_path = resolve_config_path(data_root.join("mykey.toml"));
        // One-time migration before anything reads the config: fold model
        // entries that repeat an identical apibase+apikey into a shared
        // [providers.<id>] table (backs up the file first; no-op when the
        // config already has providers or nothing is duplicated).
        commands::migrate_mykey_to_providers(&config_path.to_string_lossy());
        let memory_cfg = match oz_config::mykey::MyKeyConfig::from_file(&config_path) {
            Ok(cfg) => Some(cfg),
            Err(e) => {
                tracing::warn!("mykey.toml unreadable ({e}); memory backend defaults to \"erme\"");
                None
            }
        };
        let memory_backend = memory_cfg
            .as_ref()
            .map(|c| c.memory_backend.as_str())
            .unwrap_or("erme");
        let erme_idle_secs = memory_cfg
            .as_ref()
            .and_then(|c| c.erme_idle_interval_secs)
            .unwrap_or(300);
        let memory_backend_is_erme = memory_backend == "erme";
        let last_user_activity = Arc::new(std::sync::atomic::AtomicU64::new(0));
        let erme_store: Arc<std::sync::OnceLock<Option<Arc<ErmeRuntime>>>> =
            Arc::new(std::sync::OnceLock::new());
        if memory_backend == "erme" {
            // Background init keeps the first window responsive: the L3
            // snapshot + WAL replay + budget backfill scale with memory
            // size. The first message may briefly fall back to file
            // memory while the slot fills.
            let slot = std::sync::Arc::clone(&erme_store);
            let root = data_root.clone();
            let activity = std::sync::Arc::clone(&last_user_activity);
            std::thread::Builder::new()
                .name("erme-init".into())
                .spawn(move || {
                    let started = std::time::Instant::now();
                    let built = init_erme_store(&root, erme_idle_secs, activity);
                    if built.is_some() {
                        tracing::info!(
                            "ERME init finished in {:.2}s (background)",
                            started.elapsed().as_secs_f64()
                        );
                    }
                    let _ = slot.set(built);
                })
                .map_err(|e| tracing::error!("failed to spawn ERME init thread: {e}"))
                .ok();
        } else {
            tracing::info!("memory_backend = \"file\": ERME store not built (set memory_backend = \"erme\" to enable)");
            let _ = erme_store.set(None);
        }

        AppState {
            // Cap live sessions at 500; evicted ones are archived to
            // sessions_archive/ by the store, so nothing is silently lost
            // (P3/A8).
            sessions: Arc::new(Mutex::new(
                SessionStore::persisted(state_path).with_max(500),
            )),
            stop_signals: Arc::new(Mutex::new(HashMap::new())),
            approval_handler: Arc::new(Mutex::new(None)),
            pending_approvals: approval::new_pending(),
            running_agents: Arc::new(Mutex::new(HashMap::new())),
            detached_agents: Arc::new(Mutex::new(HashMap::new())),
            ask_user_rxs: Arc::new(Mutex::new(HashMap::new())),
            sidepanel: Mutex::new(SidePanelState::new()),
            sidepanel_sessions: Mutex::new(HashMap::new()),
            html_roots: std::sync::Mutex::new(Vec::new()),
            artifact_roots: std::sync::Mutex::new(Vec::new()),
            terminal_registry: Arc::new(std::sync::Mutex::new(HashMap::new())),
            config_path: resolve_config_path(data_root.join("mykey.toml"))
                .to_string_lossy()
                .to_string(),
            working_dir: working_dir.to_string_lossy().to_string(),
            assets_dir: find_assets_dir().to_string_lossy().to_string(),
            scheduler_started: AtomicBool::new(false),
            pending_reminders: Mutex::new(Vec::new()),
            app_handle: Mutex::new(None),
            reminder_tx: Mutex::new(None),
            locale: Arc::new(Mutex::new(load_locale())),
            skill_mcp_dir: [data_root.join(SKILL_MCP_DIR)]
                .into_iter()
                .chain(
                    std::env::current_dir()
                        .ok()
                        .map(|cwd| cwd.join(SKILL_MCP_DIR)),
                )
                .find(|p| p.is_dir())
                .map(|p| p.to_string_lossy().to_string()),
            shared_skill_store: std::sync::Mutex::new(None),
            projects: Mutex::new(projects::store::load_projects()),
            // Auto-crystallization (skill/SOP distillation after each run) is
            // opt-in: it writes new skills from finished runs, so it starts
            // off and the user turns it on from the title bar.
            crystallization_enabled: AtomicBool::new(false),
            full_access: Arc::new(AtomicBool::new(false)),
            memory_enabled: Arc::new(AtomicBool::new(memory_backend_is_erme)),
            erme_store,
            platform_registry: std::sync::Mutex::new(None),
            scheduler_shutdown: std::sync::Mutex::new(None),
            last_user_activity,
            pending_job_notices: Mutex::new(HashMap::new()),
            child_sessions: Mutex::new(HashMap::new()),
            intervention_queues: Mutex::new(HashMap::new()),
            resume_notes: Arc::new(Mutex::new(HashMap::new())),
            session_windows: Arc::new(Mutex::new(HashMap::new())),
        }
    }
}

impl AppState {
    /// ERME runtime if the background init has completed (and the backend
    /// is configured to "erme").
    pub fn erme(&self) -> Option<&Arc<ErmeRuntime>> {
        self.erme_store.get().and_then(|slot| slot.as_ref())
    }
}

impl Default for AppState {
    fn default() -> Self {
        Self::new()
    }
}

#[derive(Debug, Serialize)]
pub struct SendMessageResponse {
    pub session_id: String,
    pub status: String,
}

#[derive(Debug, Serialize)]
pub struct ModelEntry {
    pub name: String,
    pub model: String,
    /// Derived protocol family ("openai"/"claude"), from the entry name.
    pub provider: String,
    /// `[providers.<id>]` this entry borrows apibase/apikey from, if any.
    pub provider_id: Option<String>,
    pub context_win: usize,
    /// Declared input modalities; always non-empty (["text"] fallback).
    pub modalities: Vec<String>,
    pub is_local: bool,
    /// True when `default_session` in mykey.toml points at this entry.
    #[serde(default)]
    pub is_default: bool,
}

/// A named endpoint credential (`[providers.<id>]`) shared by model entries
/// that reference it via `provider = "<id>"`.
#[derive(Debug, Serialize)]
pub struct ProviderEntry {
    pub id: String,
    /// Full base URL — shown in the settings UI, not a secret.
    pub apibase: String,
    /// Masked key hint ("sk-…abcd"); the literal key never leaves the config.
    pub key_hint: String,
    /// How many session entries reference this provider.
    pub model_count: usize,
}

pub(crate) fn is_local_deploy(apibase: &str) -> bool {
    let base = apibase.to_lowercase();
    base.contains("localhost")
        || base.contains("127.0.0.1")
        || base.contains("0.0.0.0")
        || base.starts_with("http://")
            && (base.contains(".local") || base.contains(".lan") || base.contains(".internal"))
}

// ── Poison-resistant mutex helper ──
// std::sync::Mutex poisoning means a thread panicked while holding the lock.
// In a desktop app, crashing the entire process is worse than serving
// potentially-stale state. This helper recovers the guard instead of panicking.
pub(crate) fn lock_poison_guard<T>(m: &std::sync::Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|poisoned| {
        tracing::error!("Recovered from poisoned mutex — state may be inconsistent");
        poisoned.into_inner()
    })
}

/// Cell holding the process-wide skill store plus the directory it was
/// built from, so a changed config rebuilds it (round3 P1-d).
pub(crate) type SharedSkillStoreCell = std::sync::Mutex<
    Option<(
        String,
        std::sync::Arc<tokio::sync::Mutex<oz_skill_mcp::SkillMcpStore>>,
    )>,
>;

/// Fetch the process-wide parsed skill store for `dir`, building it on first
/// use and rebuilding if the configured directory changed (P1-d).
pub(crate) fn get_or_init_skill_store(
    cell: &SharedSkillStoreCell,
    dir: &str,
) -> std::sync::Arc<tokio::sync::Mutex<oz_skill_mcp::SkillMcpStore>> {
    let mut guard = lock_poison_guard(cell);
    if let Some((cached_dir, store)) = guard.as_ref() {
        if cached_dir == dir {
            return std::sync::Arc::clone(store);
        }
    }
    let store = std::sync::Arc::new(tokio::sync::Mutex::new(oz_skill_mcp::SkillMcpStore::new(
        std::path::Path::new(dir),
        Some(dir.into()),
    )));
    *guard = Some((dir.to_string(), std::sync::Arc::clone(&store)));
    store
}

// ── Desktop notification helper ──
/// Seconds since the last hardware input event on macOS. `None` when the
/// probe is unavailable (other platforms, ioreg failure).
#[cfg(target_os = "macos")]
fn user_idle_seconds() -> Option<u64> {
    let out = std::process::Command::new("ioreg")
        .args(["-c", "IOHIDSystem"])
        .output()
        .ok()?;
    let text = String::from_utf8_lossy(&out.stdout);
    for line in text.lines() {
        let Some(idx) = line.find("HIDIdleTime") else {
            continue;
        };
        let rest = line[idx..].split('=').nth(1)?;
        let digits: String = rest.chars().filter(|c| c.is_ascii_digit()).collect();
        let nanos: u64 = digits.parse().ok()?;
        return Some(nanos / 1_000_000_000);
    }
    None
}

#[cfg(not(target_os = "macos"))]
fn user_idle_seconds() -> Option<u64> {
    None
}

/// Fire a system notification WITH sound, but only when the user is not
/// actively at the machine. Window focus alone is not presence: leaving
/// OpenZen focused while walking away is common, and the old focus check
/// silently swallowed the "needs your answer" notification at 10:28 in the
/// 2026-10-23 session. Treat focus + recent input as "looking at it", and
/// focused + idle > 2 minutes as "away".
/// Every outcome is logged to `~/.openzen/logs/openzen.log` with a
/// `[notify]` prefix so notification issues are diagnosable remotely.
pub(crate) fn notify_if_unfocused(app: &AppHandle, title: &str, body: &str) {
    use tauri_plugin_notification::NotificationExt;
    let focused = app
        .get_webview_window("main")
        .map(|w| w.is_focused().unwrap_or(false))
        .unwrap_or(false);
    let idle = user_idle_seconds();
    if focused && idle.is_some_and(|secs| secs < 120) {
        debug_log(&format!(
            "[notify] skipped (window focused, user active): {title}"
        ));
        return;
    }
    if focused {
        debug_log(&format!(
            "[notify] focused but user idle {:?}s — notifying: {title}",
            idle
        ));
    }
    match app
        .notification()
        .builder()
        .title(title)
        .body(body)
        .sound("default")
        .show()
    {
        Ok(_) => debug_log(&format!("[notify] sent: {title}")),
        Err(e) => debug_log(&format!("[notify] FAILED ({title}): {e}")),
    }
}

// ── Sub-modules ──
pub(crate) mod commands;
pub(crate) mod runner;

/// Guards against re-entering the graceful-shutdown path: the cleanup calls
/// `app.exit()` again when done, which re-fires ExitRequested.
static SHUTDOWN_CLEANUP_DONE: AtomicBool = AtomicBool::new(false);

/// Ordered, time-bounded shutdown so quitting never orphans processes or
/// loses the final session snapshot:
/// agents (stop signal → checkpoint recovery) → scheduler → platform
/// adapters → terminal shells → session store flush.
/// Each step is best-effort: a stuck step must not block exit forever.
fn graceful_shutdown(state: &AppState) {
    use std::time::Duration;
    tracing::info!("[openzen] graceful shutdown: stopping agents…");
    {
        let children: Vec<String> = lock_poison_guard(&state.child_sessions)
            .values()
            .flatten()
            .cloned()
            .collect();
        let signals = lock_poison_guard(&state.stop_signals);
        for c in &children {
            if let Some(sig) = signals.get(c) {
                sig.store(true, Ordering::SeqCst);
            }
        }
    }
    let cancelled = oz_core::jobs::global().cancel_all(Some("application quit".into()));
    if cancelled > 0 {
        tracing::info!("[openzen] graceful shutdown: cancelled {cancelled} background job(s)");
    }
    {
        let signals = lock_poison_guard(&state.stop_signals);
        for sig in signals.values() {
            sig.store(true, Ordering::Relaxed);
        }
    }
    if let Some(flag) = lock_poison_guard(&state.scheduler_shutdown).clone() {
        flag.store(true, Ordering::Relaxed);
    }
    let registry = lock_poison_guard(&state.platform_registry).clone();
    if let Some(registry) = registry {
        tauri::async_runtime::block_on(async {
            let _ = tokio::time::timeout(Duration::from_secs(5), registry.stop_all()).await;
        });
    }
    // Terminal shells: SIGTERM now, background escalation to SIGKILL + reap
    // is handled by close_terminal itself.
    {
        let ids: Vec<String> = lock_poison_guard(&state.terminal_registry)
            .keys()
            .cloned()
            .collect();
        for id in ids {
            let _ =
                crate::sidepanel::terminal::close_terminal(state.terminal_registry.clone(), &id);
        }
    }
    // In-flight agent finalization persists through the sessions store;
    // give it a moment, then flush and wait for the persist worker.
    tracing::info!("[openzen] graceful shutdown: flushing sessions…");
    std::thread::sleep(Duration::from_millis(500));
    {
        let store = lock_poison_guard(&state.sessions);
        store.save();
        store.wait_persisted(Duration::from_secs(3));
    }
    tracing::info!("[openzen] graceful shutdown complete");
}

/// Durable reminders live in `{data_dir}/openzen/pending_reminders.json`:
/// run-scoped ones (persist=false) are deliberately excluded so the
/// existing lifecycles (heartbeats die with their run) are unchanged.
fn reminders_persist_path() -> std::path::PathBuf {
    data_dir().join("openzen").join("pending_reminders.json")
}

fn save_persistent_reminders(state: &AppState) {
    let durable: Vec<oz_core_types::Reminder> = {
        let pending = lock_poison_guard(&state.pending_reminders);
        pending.iter().filter(|r| r.persist).cloned().collect()
    };
    let path = reminders_persist_path();
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    match serde_json::to_string_pretty(&durable) {
        Ok(json) => {
            if let Err(e) = std::fs::write(&path, json) {
                tracing::warn!("[reminder] persist failed: {e}");
            }
        }
        Err(e) => tracing::warn!("[reminder] serialize failed: {e}"),
    }
}

/// Fire one due reminder: broadcast it, re-arm a repeat, and — the part that
/// was missing — wake the agent so a scheduled task actually executes.
/// Shared by the immediate-receive path and the 2s tick so both behave the
/// same (they used to carry two copies of the emit code, both of which only
/// emitted SSE events: `schedule_reminder` promises the backend "triggers the
/// agent run when the delay expires", but a fired reminder never started one,
/// so "report the download progress every 5 minutes" never ran — user report
/// 2026-09-22).
fn fire_reminder(
    state: &Arc<AppState>,
    app: &tauri::AppHandle,
    reminder: &oz_core_types::Reminder,
    now_ms: u64,
) {
    let session_id = reminder.session_id.clone();
    let message = reminder.message.clone();
    // Deliver BEFORE the run starts so the `reminder_fired` event reaches the
    // webview first — the live bubble must exist before the run's stream
    // events arrive (startAssistantMessageInternal resets streamingParts).
    let delivery = crate::commands::deliver_fired_reminder(state, app, &session_id, &message);
    let woke = matches!(delivery, crate::commands::ReminderDelivery::WakeRun);
    let _ = app.emit(
        "sse_event",
        serde_json::to_value(SseEvent::system(
            &session_id,
            &format!("[Reminder] {}", message),
        ))
        .unwrap_or_default(),
    );
    // Structured event so the right-rail reminder card can decrement repeats
    // and the chat store can enter the live state for a reminder-driven run.
    let _ = app.emit(
        "sse_event",
        serde_json::json!({
            "session_id": session_id,
            "event_type": "reminder_fired",
            "data": serde_json::to_string(&serde_json::json!({
                "message": message.clone(),
                "remaining_repeats": reminder.repeat_count,
                "woke": woke,
            })).unwrap_or_default(),
        }),
    );
    if reminder.repeat_count > 0 {
        lock_poison_guard(&state.pending_reminders).push(oz_core_types::Reminder {
            session_id: session_id.clone(),
            message,
            fire_at_ms: now_ms + (reminder.repeat_interval_secs * 1000),
            repeat_count: reminder.repeat_count - 1,
            repeat_interval_secs: reminder.repeat_interval_secs,
            persist: reminder.persist,
        });
    }
    if woke {
        crate::commands::start_reminder_run(state, app, &session_id);
    }
}

/// Restore durable reminders on startup. Past-due entries are kept with
/// their original fire time so the 2s tick fires them immediately (the
/// user sees "while you were away" instead of silence).
fn load_persistent_reminders(state: &AppState) -> usize {
    let Ok(raw) = std::fs::read_to_string(reminders_persist_path()) else {
        return 0;
    };
    match serde_json::from_str::<Vec<oz_core_types::Reminder>>(&raw) {
        Ok(restored) => {
            let count = restored.len();
            if count > 0 {
                let mut pending = lock_poison_guard(&state.pending_reminders);
                for r in restored {
                    if !pending.iter().any(|p| {
                        p.message == r.message
                            && p.session_id == r.session_id
                            && p.fire_at_ms == r.fire_at_ms
                    }) {
                        pending.push(r);
                    }
                }
                tracing::info!("[reminder] restored {count} durable reminder(s)");
            }
            count
        }
        Err(e) => {
            tracing::warn!("[reminder] durable file unreadable ({e}); starting empty");
            0
        }
    }
}

pub fn run() {
    // Initialize tracing into the size-rotated log file (stderr is
    // invisible in a GUI app and grew unbounded where redirected).
    let filter =
        tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into());
    let _ = tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_writer(RotatedLogMakeWriter)
        .with_ansi(false)
        .try_init();

    // Panic hook: log panic location and message before unwinding.
    // With panic="abort", the backtrace is lost; with panic="unwind",
    // this hook captures it to stderr (visible in macOS crash reports).
    std::panic::set_hook(Box::new(|info| {
        let location = info
            .location()
            .map(|l| format!("{}:{}:{}", l.file(), l.line(), l.column()))
            .unwrap_or_else(|| "unknown".into());
        let payload = if let Some(s) = info.payload().downcast_ref::<&str>() {
            s.to_string()
        } else if let Some(s) = info.payload().downcast_ref::<String>() {
            s.clone()
        } else {
            "unknown payload".into()
        };
        let msg = format!("PANIC at {location}: {payload}");
        // The hook must never block: `tracing::info!` goes through the same
        // non-reentrant `std::sync::Mutex` as `debug_log`, so a panic that
        // happens while that lock is held would self-deadlock — worse than
        // the crash itself for a 7x24 process (round5 P1-10 ④).
        let bt = std::backtrace::Backtrace::force_capture();
        if let Some(writer) = DEBUG_LOG_WRITER.get() {
            if let Ok(mut guard) = writer.try_lock() {
                let ts = chrono::Local::now().format("%Y-%m-%d %H:%M:%S");
                guard.write_line(&format!("{ts} {msg}"));
                guard.write_line(&bt.to_string());
                guard.flush();
            }
        }
        // Crash file lives in the DATA ROOT (a single overwritten slot in
        // /tmp is wiped by the OS and invisible to the user), one file per
        // panic so two crashes do not erase each other.
        let log_dir = data_dir().join("logs");
        let _ = std::fs::create_dir_all(&log_dir);
        let stamp = chrono::Local::now().format("%Y%m%d-%H%M%S");
        let crash_path = log_dir.join(format!("crash-{stamp}.log"));
        let _ = std::fs::write(
            &crash_path,
            format!(
                "{} {msg}\nversion={}\n\n{bt}",
                chrono::Local::now().to_rfc3339(),
                env!("CARGO_PKG_VERSION"),
            ),
        );
        // Keep the last 10 crash files; a crash loop must not fill the disk.
        if let Ok(entries) = std::fs::read_dir(&log_dir) {
            let mut crashes: Vec<std::path::PathBuf> = entries
                .flatten()
                .map(|e| e.path())
                .filter(|p| {
                    p.file_name()
                        .map(|n| n.to_string_lossy().starts_with("crash-"))
                        .unwrap_or(false)
                })
                .collect();
            crashes.sort();
            while crashes.len() > 10 {
                let oldest = crashes.remove(0);
                let _ = std::fs::remove_file(oldest);
            }
        }
    }));

    let rt = tokio::runtime::Runtime::new().expect("Failed to create Tokio runtime");
    let _guard = rt.enter();

    // Tool-call audit log (round5 P1-10 ①): every tool call is written as one
    // JSON line with a redacted argument digest, so "what did it actually run"
    // is answerable after the fact (the previous only record was a debug-level
    // tracing line, and release builds filter debug out).
    oz_core::audit::set_audit_dir(data_dir().join("logs"));

    let app_state = Arc::new(AppState::new());
    // Disk housekeeping in the background: drop checkpoint/WAL files whose
    // session is gone (measured 175 orphans / 64 MB on the author's machine)
    // and report the directory size. Never blocks the first window.
    spawn_checkpoint_gc(&app_state);
    // One-time fact hygiene: move sections of `memory/global_mem.txt` that
    // contradict the current tree (the pre-rename `ga-*` architecture list)
    // into `global_mem.stale.txt`. The read path also filters them, so the
    // prompt is clean even before this runs — this only tidies the file, and
    // keeps the removed text on disk.
    {
        let wd = app_state.working_dir.clone();
        let root = data_dir();
        std::thread::Builder::new()
            .name("fact-hygiene".into())
            .spawn(move || {
                // Every facts file on disk, not just `memory/`: the real
                // `ga-*` list lived in `workspace/facts/global_mem.txt` while
                // this only looked at `workspace/memory/global_mem.txt`, so
                // the quarantine reported 0 and the stale text stayed
                // (review A6).
                let roots = [
                    std::path::Path::new(&wd).join("memory"),
                    std::path::Path::new(&wd).join("facts"),
                    std::path::Path::new(&wd).join(".skill_mcp").join("facts"),
                    root.clone(),
                ];
                // EVERY candidate, not just the first one that exists: the
                // real data root has `memory/global_mem.txt` (clean) AND
                // `facts/global_mem.txt` (the stale ga-* list), and breaking on
                // the first hit meant the stale file was never examined
                // (found on the author's machine, 2026-10-22).
                // EVERY candidate, not just the first one that exists: the
                // real data root has `memory/global_mem.txt` (clean) AND
                // `facts/global_mem.txt` (the stale ga-* list), and breaking on
                // the first hit meant the stale file was never examined
                // (found on the author's machine, 2026-10-22).
                let mut total = 0usize;
                let mut hit_files: Vec<String> = Vec::new();
                for root in &roots {
                    let candidate = root.join("global_mem.txt");
                    if !candidate.exists() {
                        continue;
                    }
                    match oz_memory::quarantine_stale_fact_sections(
                        &candidate,
                        chrono::Utc::now().date_naive(),
                    ) {
                        Ok(0) => {}
                        Ok(n) => {
                            total += n;
                            hit_files.push(format!("{} ({n})", candidate.display()));
                        }
                        Err(e) => tracing::warn!(
                            "[memory] fact hygiene failed for {}: {e}",
                            candidate.display()
                        ),
                    }
                }
                if total > 0 {
                    tracing::warn!(
                        "[memory] quarantined {total} stale fact section(s) from {}",
                        hit_files.join(", ")
                    );
                    // Record the correction in the harness ledger too, as an
                    // explicit NEGATIVE example: the next time the model is
                    // tempted to describe the crate layout from memory, the
                    // ledger says which list is wrong (round5 P1-11 ②).
                    let dir = harness_dir();
                    let mut state = oz_core::harness::HarnessState::load(&dir);
                    let _ = state.apply_refine(
                        oz_core::harness::HarnessKind::Memory,
                        "OpenZen 的 crate 前缀是 `oz-`（oz-core / oz-tools / oz-agent…）。\n\
                         反例：数据根里那份写着 `ga-agent` / `ga-core` / `ga-memory` 的\"Crate \
                         架构清单\"是 2026-06 的重命名前快照，已过期并移入 `global_mem.stale.txt`；\
                         不要照它回答架构问题，要看仓库真实目录。",
                        "fact-hygiene: quarantined the pre-rename ga-* architecture list",
                        "stale facts were being injected into every system prompt",
                        "upsert",
                    );
                    let _ = state.save(&dir);
                }
            })
            .ok();
    }

    let builder = tauri::Builder::default()
        .plugin(tauri_plugin_shell::init())
        .plugin(tauri_plugin_notification::init())
        .plugin(tauri_plugin_dialog::init())
        // Session-window bookkeeping: a destroyed `session-{id}` window
        // must not leave a dead label in session_windows (approvals would
        // be routed to a nonexistent window and the fallback broadcast
        // skipped). The agent run itself keeps going in the background,
        // matching main-window behavior; approvals then fall back to the
        // main window's broadcast.
        .on_window_event(|window, event| {
            if let tauri::WindowEvent::Destroyed = event {
                let label = window.label().to_string();
                if let Some(session_id) = label.strip_prefix("session-") {
                    let state = window.app_handle().state::<Arc<AppState>>();
                    lock_poison_guard(&state.session_windows).remove(session_id);
                    tracing::info!("[session-window] cleaned up mapping for closed window {label}");
                }
            }
        })
        // Auto-update: check/install updates (frontend UpdateButton) and
        // relaunch after install. Endpoints + pubkey are configured in
        // tauri.conf.json under plugins.updater; without them check()
        // simply errors and the UI stays hidden.
        .plugin(tauri_plugin_updater::Builder::new().build())
        .plugin(tauri_plugin_process::init());

    let builder = sidepanel::scheme::register(builder);

    builder
        .setup(|app| {
            let app_handle = app.handle().clone();

            // ── Background jobs: one process-wide listener ──
            // Started/finished snapshots go to the UI; a finished job also
            // pushes one line into the owner's intervention queue so the
            // agent learns about it on its next step (no polling, no new
            // turn when the session is idle).
            {
                let job_app = app.handle().clone();
                let job_state = app.state::<std::sync::Arc<AppState>>().inner().clone();
                oz_core::jobs::global().set_listener(std::sync::Arc::new(move |ev| {
                    use oz_core::jobs::JobEvent;
                    let (kind, snap) = match &ev {
                        JobEvent::Started(s) => ("started", s),
                        JobEvent::Finished(s) => ("finished", s),
                    };
                    let _ = job_app.emit(
                        "job://update",
                        serde_json::json!({ "event": kind, "job": snap.to_json() }),
                    );
                    if let JobEvent::Finished(s) = &ev {
                        if oz_core::jobs::global().is_reported(&s.id) {
                            return;
                        }
                        let detail = match &s.detail {
                            Some(d) if !d.is_empty() => {
                                format!("{}, {}", s.status.as_str(), d)
                            }
                            _ => s.status.as_str().to_string(),
                        };
                        let notice = format!(
                            "[background] {} ({}) finished: {detail}. Read with job_output(\"{}\").",
                            s.id, s.label, s.id
                        );
                        let queue = job_state
                            .intervention_queues
                            .lock()
                            .ok()
                            .and_then(|m| m.get(&s.owner_session).cloned());
                        if let Some(queue) = queue {
                            let mut q = match queue.lock() {
                                Ok(q) => q,
                                Err(_) => return,
                            };
                            q.push_back(oz_core::checkpoint::InterventionEvent {
                                id: uuid::Uuid::new_v4().to_string(),
                                timestamp: chrono::Utc::now().timestamp() as f64,
                                kind: oz_core::checkpoint::InterventionKind::InjectInfo,
                                content: notice,
                            });
                        } else {
                            // No live run: keep the notice until the next turn.
                            if let Ok(mut pending) = job_state.pending_job_notices.lock() {
                                pending
                                    .entry(s.owner_session.clone())
                                    .or_default()
                                    .push(notice);
                            }
                            tracing::info!(
                                "[jobs] {} finished while session {} was idle; notice queued",
                                s.id, s.owner_session
                            );
                        }
                    }
                }));
            }

            // Children left `running` by a crash/quit are dead: rewrite their
            // sidecar meta so the UI never shows a ghost.
            crate::subagents::mark_interrupted_children(&app.state::<std::sync::Arc<AppState>>().inner().clone());

            // ── Subagents: in-process host ──
            {
                let sub_app = app.handle().clone();
                let sub_state = app.state::<std::sync::Arc<AppState>>().inner().clone();
                oz_core::subagent::set_host(std::sync::Arc::new(
                    crate::subagents::TauriSubagentHost {
                        app: sub_app,
                        state: sub_state,
                        cache: std::sync::Mutex::new(std::collections::HashMap::new()),
                    },
                ));
            }

            // ── Plan review inbox watcher ──
            // An out-of-process agent drops `*.json` into
            // <working_dir>/.openzen/plans/inbox/; this thread turns them into
            // plans and opens a side-panel tab. Cheap (2 s poll, one readdir)
            // and independent of the UI, so a plan submitted while the window
            // is in the background still shows up.
            {
                let watcher_handle = app.handle().clone();
                std::thread::spawn(move || loop {
                    std::thread::sleep(std::time::Duration::from_secs(2));
                    let Some(state) = watcher_handle.try_state::<std::sync::Arc<AppState>>() else {
                        continue;
                    };
                    let state = state.inner().clone();
                    if let Err(e) = crate::plans::import_inbox(&watcher_handle, &state) {
                        tracing::warn!("[plans] inbox scan failed: {e}");
                    }
                });
            }

            let show = MenuItemBuilder::with_id("show", "Open").build(app)?;
            let quit = MenuItemBuilder::with_id("quit", "Quit").build(app)?;
            let menu = MenuBuilder::new(app)
                .item(&show)
                .item(&quit)
                .build()?;

            let _tray = TrayIconBuilder::new()
                .icon(app.default_window_icon().cloned().unwrap())
                .menu(&menu)
                .on_menu_event(move |app, event| {
                    match event.id().as_ref() {
                        "show" => {
                            if let Some(w) = app.get_webview_window("main") {
                                let _ = w.show();
                                let _ = w.set_focus();
                            }
                        }
                        "quit" => {
                            app.exit(0);
                        }
                        _ => {}
                    }
                })
                .on_tray_icon_event(|tray, event| {
                    if let TrayIconEvent::Click {
                        button: MouseButton::Left,
                        button_state: MouseButtonState::Up, ..
                    } = event {
                        let app = tray.app_handle();
                        if let Some(w) = app.get_webview_window("main") {
                            let _ = w.show();
                            let _ = w.set_focus();
                        }
                    }
                })
                .build(app)?;

            let state = app.state::<Arc<AppState>>();
            *lock_poison_guard(&state.app_handle) = Some(app_handle.clone());
            let handler = Arc::new(approval::TauriApprovalHandler::new(
                app_handle.clone(),
                state.pending_approvals.clone(),
                state.full_access.clone(),
                state.session_windows.clone(),
            )) as Arc<dyn oz_safety::ApprovalHandler>;
            *lock_poison_guard(&state.approval_handler) = Some(handler);

            let mut scheduler = oz_scheduler::Scheduler::new();
            // Auto-archive of idle sessions is disabled: the 7-day window
            // silently moved older conversations out of the list (they were
            // archived to sessions_archive/ but never surfaced again). Keep
            // the task registered with an effectively unreachable window so
            // no conversation is ever pulled from under the user.
            scheduler.register(Box::new(oz_scheduler::SessionCleanup {
                max_idle_days: 36500,
                interval_secs: 3600,
            }));
            scheduler.register(Box::new(oz_scheduler::TrustDecay::default()));
            {
                let skill_mcp_exists = state.skill_mcp_dir.as_deref().is_some_and(|d| std::path::Path::new(d).is_dir());
                if skill_mcp_exists {
                    scheduler.register(Box::new(oz_scheduler::SkillMcpScan::default()));
                }
            }
            state.scheduler_started.store(true, Ordering::Relaxed);
            // Keep the shutdown flag reachable from the exit path.
            *lock_poison_guard(&state.scheduler_shutdown) = Some(scheduler.shutdown_signal());

            let state_for_platforms = Arc::clone(&state);
            tokio::spawn(async move {
                let cfg_path = std::path::Path::new(&state_for_platforms.config_path);
                tracing::info!("[openzen] Platform init: config_path={}", cfg_path.display());
                if !cfg_path.exists() {
                    tracing::info!("[openzen] Platform init: config file not found at {}", cfg_path.display());
                    return;
                }
                let raw: toml::Table = match std::fs::read_to_string(cfg_path)
                    .ok()
                    .and_then(|s| {
                        let lines: Vec<&str> = s.lines().filter(|l| l.contains("platforms")).collect();
                        tracing::info!("[openzen] Config lines with 'platforms': {:?}", lines);
                        toml::from_str(&s).ok()
                    })
                {
                    Some(t) => t,
                    None => {
                        tracing::warn!("[openzen] Platform init: failed to parse config");
                        return;
                    }
                };
                let platforms_table = match raw.get("platforms").and_then(|v| v.as_table()) {
                    Some(t) => {
                        tracing::info!("[openzen] Platform init: found {} platform(s): {:?}", t.len(), t.keys().collect::<Vec<_>>());
                        t
                    }
                    None => {
                        tracing::info!("[openzen] Platform init: no [platforms] section. Keys present: {:?}", raw.keys().collect::<Vec<_>>());
                        return;
                    }
                };

                // Share AppState's live instances with the platform bridge so
                // platform agents and the UI session store are the same data
                // (a second SessionStore instance would double-write the same
                // JSON file and a None approval_handler would bypass approvals).
                let sessions = state_for_platforms.sessions.clone();
                let running_agents = state_for_platforms.running_agents.clone();
                let stop_signals = state_for_platforms.stop_signals.clone();
                let ask_user_rxs = state_for_platforms.ask_user_rxs.clone();
                let approval_handler = state_for_platforms.approval_handler.clone();
                let locale = state_for_platforms.locale.clone();
                let skill_mcp_dir = data_dir()
                    .join(SKILL_MCP_DIR)
                    .is_dir()
                    .then(|| data_dir().join(SKILL_MCP_DIR).to_string_lossy().to_string());

                let bridge = Arc::new(AgentBridge {
                    sessions,
                    running_agents,
                    stop_signals,
                    config_path: state_for_platforms.config_path.clone(),
                    working_dir: state_for_platforms.working_dir.clone(),
                    assets_dir: state_for_platforms.assets_dir.clone(),
                    script_dir: state_for_platforms.assets_dir.clone(),
                    locale,
                    approval_handler,
                    ask_user_rxs,
                    skill_mcp_dir,
                });

                let mut registry = PlatformRegistry::new();
                for (name, val) in platforms_table {
                    tracing::info!("[openzen] Platform init: processing '{}'", name);
                    let config: oz_platform::PlatformConfig =
                        match val.clone().try_into() {
                            Ok(c) => c,
                            Err(e) => {
                                tracing::warn!("[openzen] Platform '{}' config parse error: {:?}", name, e);
                                continue;
                            }
                        };
                    if !config.enabled {
                        tracing::info!("[openzen] Platform '{}' is disabled, skipping", name);
                        continue;
                    }
                    let adapter: Option<Arc<dyn PlatformAdapter>> = match name.as_str() {
                        "feishu" => {
                            let result = oz_platform_feishu::FeishuAdapter::new(&config);
                            match &result {
                                Ok(_) => tracing::info!("[openzen] Feishu adapter created OK"),
                                Err(e) => tracing::warn!("[openzen] Feishu adapter FAILED: {:?}", e),
                            }
                            result.ok().map(|a| Arc::new(a) as Arc<dyn PlatformAdapter>)
                        }
                        "telegram" => oz_platform_telegram::TelegramAdapter::new(&config)
                            .ok()
                            .map(|a| Arc::new(a) as Arc<dyn PlatformAdapter>),
                        "wechat" => Some(Arc::new(
                            oz_platform_wechat::WechatAdapter::new(&config),
                        ) as Arc<dyn PlatformAdapter>),
                        _ => continue,
                    };
                    if let Some(a) = adapter {
                        tracing::info!("[openzen] Platform '{}' registered", name);
                        registry.register(a);
                    } else {
                        tracing::info!("[openzen] Platform '{}' adapter creation returned None", name);
                    }
                }
                if !registry.is_empty() {
                    tracing::info!("[openzen] Starting platform adapters...");
                    let ctx = PlatformContext {
                        agent: bridge,
                        platform_config: oz_platform::PlatformConfig::default(),
                        working_dir: std::path::PathBuf::from(
                            &state_for_platforms.working_dir,
                        ),
                    };
                    registry.start_all(ctx);
                    // Share with the exit path so adapters are stopped on quit.
                    *lock_poison_guard(&state_for_platforms.platform_registry) =
                        Some(Arc::new(registry));
                    tracing::info!("[openzen] Platform adapters started");
                } else {
                    tracing::info!("[openzen] No platform adapters to start (registry empty)");
                }
            });

            // Maintenance tasks must see the real data paths — with the
            // default context SessionCleanup looks for sessions.json under
            // "./" and TrustDecay never runs at all.
            let pruner_state = Arc::clone(&state);
            let task_ctx = oz_scheduler::TaskContext {
                working_dir: Some(data_dir().to_string_lossy().to_string()),
                skill_mcp_dir: state.skill_mcp_dir.clone(),
                trust_path: Some(
                    data_dir()
                        .join("openzen")
                        .join("trust.json")
                        .to_string_lossy()
                        .to_string(),
                ),
                // In-process pruning: the disk-side path edits sessions.json
                // directly, which the next AppState save would resurrect.
                session_pruner: Some(oz_scheduler::task::SessionPruner(
                    std::sync::Arc::new(move |max_idle_days| {
                        let threshold =
                            chrono::Utc::now() - chrono::Duration::days(max_idle_days);
                        // Live set: a session whose agent is running (or
                        // detached but still finishing) must never be
                        // ghost-flipped to Stopped — that used to turn Stop
                        // into a no-op for any run longer than 30 minutes.
                        let live: std::collections::HashSet<String> = {
                            let running = lock_poison_guard(&pruner_state.running_agents);
                            let detached = lock_poison_guard(&pruner_state.detached_agents);
                            running.keys().chain(detached.keys()).cloned().collect()
                        };
                        let mut store = lock_poison_guard(&pruner_state.sessions);
                        store.prune_expired(threshold, &live) as u32
                    }),
                )),
            };
            tauri::async_runtime::spawn(scheduler.run(task_ctx));

            let (reminder_tx, mut reminder_rx) = tokio::sync::mpsc::unbounded_channel::<oz_core_types::Reminder>();
            *lock_poison_guard(&state.reminder_tx) = Some(reminder_tx.clone());
            let set_result = oz_core_types::REMINDER_TX.set(reminder_tx);
            tracing::info!("[reminder] REMINDER_TX.set() ok={}", set_result.is_ok());

            let state_for_reminders = Arc::clone(&state);
            let state_for_restore = Arc::clone(&state);
            debug_log("reminder checker: starting");
            tokio::spawn(async move {
                let mut check_interval = tokio::time::interval(std::time::Duration::from_secs(2));
                loop {
                    tokio::select! {
                        Some(reminder) = reminder_rx.recv() => {
                            // Captured before the branches consume `reminder`.
                            let persist_hint = reminder.persist;
                            debug_log(&format!("reminder received: sid={} msg='{}' fire_at={}", 
                                reminder.session_id, reminder.message, reminder.fire_at_ms));
                            if reminder.session_id.is_empty() {
                                lock_poison_guard(&state_for_reminders.pending_reminders).push(reminder);
                            } else {
                                let now = std::time::SystemTime::now()
                                    .duration_since(std::time::UNIX_EPOCH)
                                    .map(|d| d.as_millis() as u64)
                                    .unwrap_or(0);
                                if reminder.fire_at_ms <= now + 100 {
                                    let app = lock_poison_guard(&state_for_reminders.app_handle).clone();
                                    if let Some(app) = app {
                                        fire_reminder(&state_for_reminders, &app, &reminder, now);
                                    } else {
                                        // app_handle not yet available (platform mode / early startup):
                                        // keep the reminder for the next tick instead of dropping it.
                                        lock_poison_guard(&state_for_reminders.pending_reminders).push(reminder);
                                    }
                                } else {
                                    lock_poison_guard(&state_for_reminders.pending_reminders).push(reminder);
                                }
                            }
                            if persist_hint {
                                save_persistent_reminders(&state_for_reminders);
                            }
                        }
                        _ = check_interval.tick() => {
                            let now = std::time::SystemTime::now()
                                .duration_since(std::time::UNIX_EPOCH)
                                .map(|d| d.as_millis() as u64)
                                .unwrap_or(0);
                            // Collect the due entries under the lock, then fire
                            // them outside it: firing takes the sessions /
                            // running-agent locks (deliver + start run).
                            let mut due: Vec<oz_core_types::Reminder> = Vec::new();
                            {
                                let mut pending = lock_poison_guard(&state_for_reminders.pending_reminders);
                                let mut i = 0;
                                while i < pending.len() {
                                    if pending[i].fire_at_ms <= now + 100 {
                                        due.push(pending.remove(i));
                                    } else {
                                        i += 1;
                                    }
                                }
                            }
                            let mut fired_any = false;
                            for reminder in due {
                                let app = lock_poison_guard(&state_for_reminders.app_handle).clone();
                                let Some(app) = app else {
                                    // app_handle not yet available (platform mode / early startup):
                                    // keep the reminder for the next tick instead of dropping it.
                                    lock_poison_guard(&state_for_reminders.pending_reminders).push(reminder);
                                    continue;
                                };
                                fired_any = true;
                                fire_reminder(&state_for_reminders, &app, &reminder, now);
                            }
                            if fired_any {
                                save_persistent_reminders(&state_for_reminders);
                            }
                        }
                    }
                }
            });

            // Restore durable reminders (persist=true) saved by a previous
            // process; past-due ones fire on the next 2s tick.
            {
                let restored = load_persistent_reminders(&state_for_restore);
                if restored > 0 {
                    debug_log(&format!("restored {restored} durable reminder(s)"));
                }
            }

            Ok(())
        })
        .manage(app_state)
        .invoke_handler(tauri::generate_handler![
            commands::clear_session_messages,
            commands::scan_import_sources,
            commands::import_list_sessions,
            commands::import_sessions,
            commands::ping,
            commands::log_frontend,
            commands::computer_screenshot_data,
            commands::get_working_dir,
commands::get_working_dir_for_session,
            commands::get_dashboard_stats,
            commands::get_memory_status,
            commands::set_soul_identity,
            commands::get_soul_portrait,
            commands::remove_portrait_fact,
            // round5 P1-12: memory must be closable / exportable / deletable.
            commands::set_memory_enabled,
            commands::export_memory,
            commands::list_memories,
            commands::forget_memory,
            commands::clear_memory,
            commands::log_frontend,
            commands::export_diagnostics,
            commands::reveal_path,
            commands::submit_message_feedback,
            commands::upsert_model,
            commands::delete_model,
            commands::set_default_model,
            commands::list_providers,
            commands::upsert_provider,
            commands::delete_provider,
            commands::list_skill_mcp,
            commands::toggle_skill_mcp,
            commands::list_mcp_servers,
            commands::toggle_mcp_server,
            commands::get_token_stats,
            commands::get_quality_report,
            commands::list_models,
            commands::list_sessions,
            commands::create_session,
            commands::create_session_in_project,
            commands::move_session_to_project,
            commands::get_session,
            commands::delete_session,
            commands::rename_session,
            commands::stop_session,
            commands::inject_message,
            commands::job_list,
            commands::job_output,
            commands::job_kill,
            commands::agent_list,
            commands::agent_read,
            commands::agent_cancel,
            commands::send_message,
            commands::regenerate,
            commands::resume_session,
            commands::ask_user_response,
            commands::open_session_window,
            commands::compress_session,
            commands::get_locale,
            commands::set_locale,
            commands::add_platform,
            commands::get_crystallization,
            commands::set_crystallization,
            commands::get_full_access,
            commands::set_full_access,
            projects::commands::add_project,
            projects::commands::list_projects,
            projects::commands::remove_project,
            projects::commands::rename_project,
            projects::commands::reveal_in_finder,
            approval::approve_tool,
            crate::sidepanel::commands::toggle_sidepanel,
            crate::sidepanel::commands::set_sidepanel_width,
            crate::sidepanel::commands::open_artifact,
            crate::sidepanel::commands::open_artifact_dialog,
            crate::sidepanel::commands::close_sidepanel,
            crate::plans::plan_list,
            crate::plans::plan_get,
            crate::plans::plan_open,
            crate::plans::plan_submit,
            crate::plans::plan_decide,
            crate::plans::plan_scan_inbox,
            crate::sidepanel::commands::get_sidepanel_state,
            crate::sidepanel::commands::close_artifact_tab,
            crate::sidepanel::commands::switch_artifact_tab,
            crate::sidepanel::commands::clear_sidepanel_artifacts,
            crate::sidepanel::commands::set_sidepanel_session,
            crate::sidepanel::commands::spawn_terminal,
            crate::sidepanel::commands::write_to_terminal,
            crate::sidepanel::commands::resize_terminal,
            crate::sidepanel::commands::close_terminal,
            crate::sidepanel::commands::read_file_content,
            crate::sidepanel::commands::read_file_bytes,
            crate::sidepanel::commands::parse_excel,
            crate::sidepanel::commands::get_git_diff,
            crate::sidepanel::commands::get_file_info,
            crate::sidepanel::commands::open_external_file,
        ])
        .build(tauri::generate_context!())
        .expect("error while building tauri application")
        .run(|app, event| {
            // Quit paths (tray Quit, app.exit, macOS Cmd+Q) all land here.
            // Intercept once, run the ordered teardown off the main thread,
            // then re-request the exit — the second pass is allowed through.
            if let tauri::RunEvent::ExitRequested { code, api, .. } = event {
                if SHUTDOWN_CLEANUP_DONE.swap(true, Ordering::Relaxed) {
                    return;
                }
                api.prevent_exit();
                let app = app.clone();
                let code = code.unwrap_or(0);
                std::thread::Builder::new()
                    .name("openzen-graceful-shutdown".into())
                    .spawn(move || {
                        let state = app.state::<Arc<AppState>>();
                        graceful_shutdown(&state);
                        app.exit(code);
                    })
                    .expect("failed to spawn graceful shutdown thread");
            }
        });
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Plan B (2026-08-09): `OPENZEN_DATA_DIR` must override the default data
    /// root so a dev build runs against an isolated data tree (zero pollution
    /// of `~/.openzen/` and of the source tree). Serialized via a file-level
    /// mutex because `set_var` is process-global and cargo runs tests in
    /// parallel threads.
    static ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

    #[test]
    fn data_dir_respects_openzen_data_dir_override() {
        let _guard = lock_poison_guard(&ENV_LOCK);
        let original = std::env::var("OPENZEN_DATA_DIR").ok();

        std::env::set_var("OPENZEN_DATA_DIR", "/tmp/openzen-dev-test");
        assert_eq!(
            data_dir(),
            PathBuf::from("/tmp/openzen-dev-test"),
            "OPENZEN_DATA_DIR must override the default ~/.openzen root"
        );

        match original {
            Some(v) => std::env::set_var("OPENZEN_DATA_DIR", v),
            None => std::env::remove_var("OPENZEN_DATA_DIR"),
        }
    }

    #[test]
    fn data_dir_defaults_to_home_dot_openzen() {
        let _guard = lock_poison_guard(&ENV_LOCK);
        let original = std::env::var("OPENZEN_DATA_DIR").ok();
        let original_home = std::env::var("HOME").ok();
        let tmp_home = std::env::temp_dir().join("oz-data-dir-default-test");
        std::fs::remove_dir_all(&tmp_home).ok();
        std::env::remove_var("OPENZEN_DATA_DIR");
        std::env::set_var("HOME", &tmp_home);

        assert_eq!(
            data_dir(),
            home_dir().join(".openzen"),
            "without OPENZEN_DATA_DIR, data root defaults to ~/.openzen"
        );

        match original {
            Some(v) => std::env::set_var("OPENZEN_DATA_DIR", v),
            None => {}
        }
        match original_home {
            Some(v) => std::env::set_var("HOME", v),
            None => std::env::remove_var("HOME"),
        }
    }

    #[test]
    fn data_dir_uses_profile_data_dir_override() {
        let _guard = lock_poison_guard(&ENV_LOCK);
        let original_dir = std::env::var("OPENZEN_DATA_DIR").ok();
        let original_profile = std::env::var("OPENZEN_PROFILE").ok();
        let original_home = std::env::var("HOME").ok();
        let tmp_home = std::env::temp_dir().join("oz-data-dir-profile-test");
        std::fs::create_dir_all(tmp_home.join(".openzen")).unwrap();
        std::fs::write(
            tmp_home.join(".openzen/profiles.toml"),
            "[profiles.dev]\ndata_dir = \"/tmp/openzen-dev-profile\"\n",
        )
        .unwrap();

        std::env::remove_var("OPENZEN_DATA_DIR");
        std::env::set_var("OPENZEN_PROFILE", "dev");
        std::env::set_var("HOME", &tmp_home);
        assert_eq!(
            data_dir(),
            PathBuf::from("/tmp/openzen-dev-profile"),
            "active profile's data_dir must override the default ~/.openzen root"
        );

        match original_dir {
            Some(v) => std::env::set_var("OPENZEN_DATA_DIR", v),
            None => std::env::remove_var("OPENZEN_DATA_DIR"),
        }
        match original_profile {
            Some(v) => std::env::set_var("OPENZEN_PROFILE", v),
            None => std::env::remove_var("OPENZEN_PROFILE"),
        }
        match original_home {
            Some(v) => std::env::set_var("HOME", v),
            None => std::env::remove_var("HOME"),
        }
    }

    #[test]
    fn working_dir_lives_under_data_root() {
        let _guard = lock_poison_guard(&ENV_LOCK);
        let original = std::env::var("OPENZEN_DATA_DIR").ok();
        std::env::set_var("OPENZEN_DATA_DIR", "/tmp/openzen-dev-test");

        let ctx = tauri_ctx();
        assert!(
            ctx.working_dir
                .starts_with("/tmp/openzen-dev-test/workspace"),
            "agent working dir must live under the data root, got: {}",
            ctx.working_dir
        );

        match original {
            Some(v) => std::env::set_var("OPENZEN_DATA_DIR", v),
            None => std::env::remove_var("OPENZEN_DATA_DIR"),
        }
    }
}
