use std::collections::HashMap;
use std::io::Write;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
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
pub mod projects;
mod sidepanel;

const SESSION_STATE_FILE: &str = "openzen/sessions.json";

pub(crate) fn debug_log(msg: &str) {
    let log_dir = data_dir().join("logs");
    let _ = std::fs::create_dir_all(&log_dir);
    if let Ok(mut f) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(log_dir.join("openzen.log"))
    {
        let _ = writeln!(f, "[openzen] {}", msg);
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
fn init_erme_store(
    base_dir: &std::path::Path,
    idle_interval_secs: u64,
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
            ..Default::default()
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
    let reflection = Arc::new(
        ReflectionEngine::new(
            Arc::clone(&soul),
            Arc::clone(&store),
            observer,
            conflict_resolver,
            reflection_cfg,
        )
        .with_rambling(Arc::clone(&rambling))
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
        std::thread::Builder::new()
            .name("erme-idle-cycle".into())
            .spawn(move || loop {
                std::thread::sleep(std::time::Duration::from_secs(idle_interval_secs));
                let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    reflection.run_full_cycle();
                    orchestrator.run_idle_cycle();
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
        std::env::var("LANG")
            .unwrap_or_default()
            .starts_with("zh")
            .then(|| "zh".to_string())
            .unwrap_or_else(|| "en".to_string())
    }
    #[cfg(not(target_os = "macos"))]
    {
        std::env::var("LANG")
            .unwrap_or_default()
            .to_lowercase()
            .contains("zh")
            .then(|| "zh".to_string())
            .unwrap_or_else(|| "en".to_string())
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

use crate::sidepanel::state::SidePanelState;
use crate::sidepanel::terminal::TerminalRegistry;

type AskUserSlot = Arc<Mutex<Option<String>>>;

pub struct AppState {
    pub sessions: Arc<Mutex<SessionStore>>,
    pub stop_signals: Arc<Mutex<HashMap<String, Arc<AtomicBool>>>>,
    pub approval_handler: Arc<Mutex<Option<Arc<dyn oz_safety::ApprovalHandler>>>>,
    pub pending_approvals: approval::PendingApprovals,
    pub running_agents: Arc<Mutex<HashMap<String, JoinHandle<()>>>>,
    pub detached_agents: Arc<Mutex<HashMap<String, JoinHandle<()>>>>,
    pub ask_user_rxs: Arc<Mutex<HashMap<String, AskUserSlot>>>,
    pub sidepanel: Mutex<SidePanelState>,
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
    pub projects: Mutex<Vec<projects::store::ProjectRecord>>,
    pub crystallization_enabled: AtomicBool,
    /// "完全访问" (full access): when set, the approval handler auto-allows
    /// every request so the agent never asks the user for permission.
    pub full_access: Arc<AtomicBool>,
    /// Long-lived ERME runtime (semantic store + L0 soul layer).
    /// Created once at startup; None when construction failed.
    pub erme_store: Option<Arc<ErmeRuntime>>,
    pub intervention_queues: Mutex<
        HashMap<
            String,
            Arc<Mutex<std::collections::VecDeque<oz_core::checkpoint::InterventionEvent>>>,
        >,
    >,
    /// session_id → webview window label for dedicated session windows
    /// (`session-{id}`), used to route session-scoped events (approvals)
    /// to the owning window instead of broadcasting to all windows.
    pub session_windows: Arc<Mutex<HashMap<String, String>>>,
}

impl AppState {
    pub fn new() -> Self {
        let data_root = data_dir();
        // Agent working dir lives under the data root, never the source tree.
        let working_dir = data_root.join("workspace");
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
            let old = data_root.join(SKILL_MCP_DIR).join("harness").join("harness_state.json");
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
        let memory_cfg = match oz_config::mykey::MyKeyConfig::from_file(&config_path) {
            Ok(cfg) => Some(cfg),
            Err(e) => {
                tracing::warn!(
                    "mykey.toml unreadable ({e}); memory backend defaults to \"erme\""
                );
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
        let erme_store = if memory_backend == "erme" {
            init_erme_store(&data_root, erme_idle_secs)
        } else {
            tracing::info!("memory_backend = \"file\": ERME store not built (set memory_backend = \"erme\" to enable)");
            None
        };

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
            html_roots: std::sync::Mutex::new(Vec::new()),
            artifact_roots: std::sync::Mutex::new(Vec::new()),
            terminal_registry: Arc::new(std::sync::Mutex::new(HashMap::new())),
            config_path: data_root.join("mykey.toml").to_string_lossy().to_string(),
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
            projects: Mutex::new(projects::store::load_projects()),
            crystallization_enabled: AtomicBool::new(false),
            full_access: Arc::new(AtomicBool::new(false)),
            erme_store,
            intervention_queues: Mutex::new(HashMap::new()),
            session_windows: Arc::new(Mutex::new(HashMap::new())),
        }
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
    pub provider: String,
    pub context_win: usize,
    pub is_local: bool,
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

// ── Desktop notification helper ──
/// Fire a system notification WITH sound, but only when the main window is
/// not focused — when the user is looking at the app, the UI itself is the
/// notification. Used for task completion, pending questions and
/// compression alerts so background work is always noticeable.
/// Every outcome is logged to `~/.openzen/logs/openzen.log` with a
/// `[notify]` prefix so notification issues are diagnosable remotely.
pub(crate) fn notify_if_unfocused(app: &AppHandle, title: &str, body: &str) {
    use tauri_plugin_notification::NotificationExt;
    let focused = app
        .get_webview_window("main")
        .map(|w| w.is_focused().unwrap_or(false))
        .unwrap_or(false);
    if focused {
        debug_log(&format!("[notify] skipped (main window focused): {title}"));
        return;
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

pub fn run() {
    // Initialize tracing so agent loop / LLM errors are visible on stderr.
    let filter =
        tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into());
    let _ = tracing_subscriber::fmt().with_env_filter(filter).try_init();

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
        eprintln!("{msg}");
        let bt = std::backtrace::Backtrace::force_capture();
        eprintln!("{bt}");
        // Also write crash log for post-mortem
        let crash_path = std::path::PathBuf::from("/tmp/openzen-crash.log");
        let ts = chrono::Local::now().format("%Y-%m-%d %H:%M:%S");
        let _ = std::fs::write(&crash_path, format!("{ts} {msg}\n{bt}"));
    }));

    let rt = tokio::runtime::Runtime::new().expect("Failed to create Tokio runtime");
    let _guard = rt.enter();

    let app_state = Arc::new(AppState::new());

    let builder = tauri::Builder::default()
        .plugin(tauri_plugin_shell::init())
        .plugin(tauri_plugin_notification::init())
        .plugin(tauri_plugin_dialog::init());

    let builder = sidepanel::scheme::register(builder);

    builder
        .setup(|app| {
            let app_handle = app.handle().clone();
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
            scheduler.register(Box::new(oz_scheduler::SessionCleanup {
                max_idle_days: 7,
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

            let state_for_platforms = Arc::clone(&state);
            tokio::spawn(async move {
                let cfg_path = std::path::Path::new(&state_for_platforms.config_path);
                eprintln!("[openzen] Platform init: config_path={}", cfg_path.display());
                if !cfg_path.exists() {
                    eprintln!("[openzen] Platform init: config file not found at {}", cfg_path.display());
                    return;
                }
                let raw: toml::Table = match std::fs::read_to_string(cfg_path)
                    .ok()
                    .and_then(|s| {
                        let lines: Vec<&str> = s.lines().filter(|l| l.contains("platforms")).collect();
                        eprintln!("[openzen] Config lines with 'platforms': {:?}", lines);
                        toml::from_str(&s).ok()
                    })
                {
                    Some(t) => t,
                    None => {
                        eprintln!("[openzen] Platform init: failed to parse config");
                        return;
                    }
                };
                let platforms_table = match raw.get("platforms").and_then(|v| v.as_table()) {
                    Some(t) => {
                        eprintln!("[openzen] Platform init: found {} platform(s): {:?}", t.len(), t.keys().collect::<Vec<_>>());
                        t
                    }
                    None => {
                        eprintln!("[openzen] Platform init: no [platforms] section. Keys present: {:?}", raw.keys().collect::<Vec<_>>());
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
                    eprintln!("[openzen] Platform init: processing '{}'", name);
                    let config: oz_platform::PlatformConfig =
                        match val.clone().try_into() {
                            Ok(c) => c,
                            Err(e) => {
                                eprintln!("[openzen] Platform '{}' config parse error: {:?}", name, e);
                                continue;
                            }
                        };
                    if !config.enabled {
                        eprintln!("[openzen] Platform '{}' is disabled, skipping", name);
                        continue;
                    }
                    let adapter: Option<Arc<dyn PlatformAdapter>> = match name.as_str() {
                        "feishu" => {
                            let result = oz_platform_feishu::FeishuAdapter::new(&config);
                            match &result {
                                Ok(_) => eprintln!("[openzen] Feishu adapter created OK"),
                                Err(e) => eprintln!("[openzen] Feishu adapter FAILED: {:?}", e),
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
                        eprintln!("[openzen] Platform '{}' registered", name);
                        registry.register(a);
                    } else {
                        eprintln!("[openzen] Platform '{}' adapter creation returned None", name);
                    }
                }
                if !registry.is_empty() {
                    eprintln!("[openzen] Starting platform adapters...");
                    let ctx = PlatformContext {
                        agent: bridge,
                        platform_config: oz_platform::PlatformConfig::default(),
                        working_dir: std::path::PathBuf::from(
                            &state_for_platforms.working_dir,
                        ),
                    };
                    registry.start_all(ctx);
                    eprintln!("[openzen] Platform adapters started");
                } else {
                    eprintln!("[openzen] No platform adapters to start (registry empty)");
                }
            });

            tauri::async_runtime::spawn(scheduler.run());

            let (reminder_tx, mut reminder_rx) = tokio::sync::mpsc::unbounded_channel::<oz_core_types::Reminder>();
            *lock_poison_guard(&state.reminder_tx) = Some(reminder_tx.clone());
            let set_result = oz_core_types::REMINDER_TX.set(reminder_tx);
            eprintln!("[reminder] REMINDER_TX.set() ok={}", set_result.is_ok());

            let state_for_reminders = Arc::clone(&state);
            debug_log("reminder checker: starting");
            tokio::spawn(async move {
                let mut check_interval = tokio::time::interval(std::time::Duration::from_secs(2));
                loop {
                    tokio::select! {
                        Some(reminder) = reminder_rx.recv() => {
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
                                    let session_id = reminder.session_id.clone();
                                    let message = reminder.message.clone();
                                    let app = lock_poison_guard(&state_for_reminders.app_handle).clone();
                                    if let Some(app) = app {
                                        let _ = app.emit("sse_event", serde_json::to_value(&SseEvent::system(
                                            &session_id, &format!("[Reminder] {}", message),
                                        )).unwrap_or_default());
                                        // Structured event so the right-rail
                                        // reminder card can decrement repeats.
                                        let _ = app.emit("sse_event", serde_json::json!({
                                            "session_id": session_id,
                                            "event_type": "reminder_fired",
                                            "data": serde_json::to_string(&serde_json::json!({
                                                "message": message.clone(),
                                                "remaining_repeats": reminder.repeat_count,
                                            })).unwrap_or_default(),
                                        }));
                                        let next_reminder = if reminder.repeat_count > 0 {
                                            Some(oz_core_types::Reminder {
                                                session_id: session_id.clone(),
                                                message: message.clone(),
                                                fire_at_ms: now + (reminder.repeat_interval_secs * 1000),
                                                repeat_count: reminder.repeat_count - 1,
                                                repeat_interval_secs: reminder.repeat_interval_secs,
                                            })
                                        } else { None };
                                        if let Some(r) = next_reminder {
                                            lock_poison_guard(&state_for_reminders.pending_reminders).push(r);
                                        }
                                    } else {
                                        // app_handle not yet available (platform mode / early startup):
                                        // keep the reminder for the next tick instead of dropping it.
                                        lock_poison_guard(&state_for_reminders.pending_reminders).push(reminder);
                                    }
                                } else {
                                    lock_poison_guard(&state_for_reminders.pending_reminders).push(reminder);
                                }
                            }
                        }
                        _ = check_interval.tick() => {
                            let now = std::time::SystemTime::now()
                                .duration_since(std::time::UNIX_EPOCH)
                                .map(|d| d.as_millis() as u64)
                                .unwrap_or(0);
                            let mut pending = lock_poison_guard(&state_for_reminders.pending_reminders);
                            let mut i = 0;
                            while i < pending.len() {
                                let reminder = &pending[i];
                                if reminder.fire_at_ms <= now + 100 {
                                    let reminder = pending.remove(i);
                                    let session_id = reminder.session_id.clone();
                                    let message = reminder.message.clone();
                                    let app = lock_poison_guard(&state_for_reminders.app_handle).clone();
                                    if let Some(app) = app {
                                        let _ = app.emit("sse_event", serde_json::to_value(&SseEvent::system(
                                            &session_id, &format!("[Reminder] {}", message),
                                        )).unwrap_or_default());
                                        // Structured event so the right-rail
                                        // reminder card can decrement repeats.
                                        let _ = app.emit("sse_event", serde_json::json!({
                                            "session_id": session_id,
                                            "event_type": "reminder_fired",
                                            "data": serde_json::to_string(&serde_json::json!({
                                                "message": message.clone(),
                                                "remaining_repeats": reminder.repeat_count,
                                            })).unwrap_or_default(),
                                        }));
                                        let next_reminder = if reminder.repeat_count > 0 {
                                            Some(oz_core_types::Reminder {
                                                session_id: session_id.clone(),
                                                message: message.clone(),
                                                fire_at_ms: now + (reminder.repeat_interval_secs * 1000),
                                                repeat_count: reminder.repeat_count - 1,
                                                repeat_interval_secs: reminder.repeat_interval_secs,
                                            })
                                        } else { None };
                                        if let Some(r) = next_reminder {
                                            pending.push(r);
                                        }
                                    } else {
                                        // app_handle not yet available (platform mode / early startup):
                                        // keep the reminder for the next tick instead of dropping it.
                                        pending.insert(i, reminder);
                                        i += 1;
                                    }
                                } else {
                                    i += 1;
                                }
                            }
                        }
                    }
                }
            });

            Ok(())
        })
        .manage(app_state)
        .invoke_handler(tauri::generate_handler![
            commands::clear_session_messages,
            commands::ping,
            commands::get_working_dir,
commands::get_working_dir_for_session,
            commands::get_dashboard_stats,
            commands::get_memory_status,
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
            crate::sidepanel::commands::get_sidepanel_state,
            crate::sidepanel::commands::close_artifact_tab,
            crate::sidepanel::commands::switch_artifact_tab,
            crate::sidepanel::commands::clear_sidepanel_artifacts,
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
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
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
