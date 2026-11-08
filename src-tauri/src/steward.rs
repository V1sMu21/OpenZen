//! Tauri bridge for the steward flight recorder.
//!
//! The runtime is global and lazy so a feature-flagged/disabled steward has
//! zero cost: no DB open, no recorder allocation, no per-event work. The
//! recorder itself is fed from the runner's existing StreamEvent collector.

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, OnceLock};

use oz_core_types::supervision::{StepRecord, TicketSnapshot};
use oz_core_types::StreamEvent;
use oz_steward::{
    license, Acceptance, ContractBudget, CorrectionEngine, FileJournal, GateReport, JudgeOutcome,
    LLMJudge, Recorder, Report, ReportComposer, RiskLevel, StartLuxConfig, StartLuxProvider,
    StartLuxTier, StartLuxVariant, Store, Supervisor, TaskContract, Watchdog, WatchdogAction,
};
use serde_json::{json, Value};

pub struct StewardRuntime {
    store: Arc<Store>,
    recorders: Mutex<HashMap<String, Recorder>>,
    enabled: AtomicBool,
    db_path: std::path::PathBuf,
    stop_signals: Mutex<HashMap<String, Arc<AtomicBool>>>,
    journal: Arc<Mutex<FileJournal>>,
    corrections: Mutex<CorrectionEngine>,
}

static RUNTIME: OnceLock<Option<Arc<StewardRuntime>>> = OnceLock::new();
static WATCHDOG_STARTED: OnceLock<()> = OnceLock::new();

fn env_enabled() -> bool {
    std::env::var("OPENZEN_STEWARD")
        .map(|v| v == "1" || v.eq_ignore_ascii_case("true"))
        .unwrap_or(false)
}

static ENABLED_HINT: AtomicBool = AtomicBool::new(false);
static HINT_INIT: OnceLock<()> = OnceLock::new();
static NUDGE_AT: OnceLock<Mutex<HashMap<String, i64>>> = OnceLock::new();

fn enabled_hint() -> bool {
    HINT_INIT.get_or_init(|| {
        ENABLED_HINT.store(env_enabled(), Ordering::Relaxed);
    });
    ENABLED_HINT.load(Ordering::Relaxed)
}

pub fn runtime() -> Option<&'static Arc<StewardRuntime>> {
    RUNTIME
        .get_or_init(|| {
            let db_path = crate::data_dir().join("steward.db");
            match Store::open(&db_path) {
                Ok(store) => {
                    let enabled = enabled_hint();
                    let journal = match FileJournal::new(
                        crate::default_workspace_dir(),
                        crate::data_dir().join("steward-backups"),
                    ) {
                        Ok(journal) => journal,
                        Err(e) => {
                            tracing::warn!("steward journal unavailable: {e}");
                            return None;
                        }
                    };
                    Some(Arc::new(StewardRuntime {
                        store: Arc::new(store),
                        recorders: Mutex::new(HashMap::new()),
                        enabled: AtomicBool::new(enabled),
                        db_path,
                        stop_signals: Mutex::new(HashMap::new()),
                        journal: Arc::new(Mutex::new(journal)),
                        corrections: Mutex::new(CorrectionEngine::default()),
                    }))
                }
                Err(e) => {
                    tracing::warn!("steward store unavailable at {}: {e}", db_path.display());
                    None
                }
            }
        })
        .as_ref()
}

impl StewardRuntime {
    pub fn enabled(&self) -> bool {
        self.enabled.load(Ordering::Relaxed)
    }

    pub fn set_enabled(&self, enabled: bool) {
        self.enabled.store(enabled, Ordering::Relaxed);
    }

    pub fn register_stop_signal(&self, session_id: &str, signal: Arc<AtomicBool>) {
        if let Ok(mut map) = self.stop_signals.lock() {
            map.insert(session_id.to_string(), signal);
        }
    }

    fn record(&self, session_id: &str, event: &StreamEvent) {
        if !self.enabled() {
            return;
        }
        let mut recorders = match self.recorders.lock() {
            Ok(g) => g,
            Err(_) => return,
        };
        let recorder = recorders.entry(session_id.to_string()).or_insert_with(|| {
            let title = format!("Session {}", session_id.chars().take(8).collect::<String>());
            let _ = self.store.ensure_ticket(session_id, session_id, &title);
            Recorder::new(self.store.clone(), session_id, session_id)
        });
        if let Err(e) = recorder.on_event(event) {
            tracing::warn!("steward recorder failed for {session_id}: {e}");
        }
    }
}

fn startlux_ack_path() -> std::path::PathBuf {
    crate::data_dir().join("steward-startlux-license-ack")
}

fn startlux_acknowledged() -> bool {
    std::env::var("STARTLUX_LICENSE_ACK")
        .map(|v| v == "1" || v.eq_ignore_ascii_case("true"))
        .unwrap_or(false)
        || startlux_ack_path().exists()
}

fn require_startlux_license() -> Result<(), String> {
    if startlux_acknowledged() {
        Ok(())
    } else {
        Err("StartLux weights are CC BY-NC 4.0; acknowledge the license first (steward_ack_startlux_license)".into())
    }
}

fn startlux_config() -> Result<StartLuxConfig, String> {
    let tier = match std::env::var("OPENZEN_STARTLUX_TIER")
        .unwrap_or_else(|_| "4b".into())
        .to_ascii_lowercase()
        .as_str()
    {
        "0.8b" => StartLuxTier::B0_8,
        "2b" => StartLuxTier::B2,
        "9b" => StartLuxTier::B9,
        "27b" => StartLuxTier::B27,
        _ => StartLuxTier::B4,
    };
    let variant = if std::env::var("OPENZEN_STARTLUX_AUTOTHINK")
        .map(|v| v == "1" || v.eq_ignore_ascii_case("true"))
        .unwrap_or(false)
    {
        StartLuxVariant::AutoThink
    } else {
        StartLuxVariant::Instant
    };
    StartLuxConfig::new(tier, variant).map_err(|e| e.to_string())
}

/// Start the 30s stale-ticket sweep once. No-op while recording is disabled;
/// `steward_set_enabled(true)` calls it again.
pub fn start_watchdog() {
    start_watchdog_inner(false);
}

fn start_watchdog_inner(force: bool) {
    if !force && !enabled_hint() {
        return;
    }
    let Some(rt) = runtime() else { return };
    if !rt.enabled() || WATCHDOG_STARTED.get().is_some() {
        return;
    }
    let _ = WATCHDOG_STARTED.set(());
    let rt = rt.clone();
    tauri::async_runtime::spawn(async move {
        let watchdog = Watchdog::new(rt.store.clone(), 90_000);
        loop {
            tokio::time::sleep(std::time::Duration::from_secs(30)).await;
            if !rt.enabled() {
                continue;
            }
            match watchdog.tick(oz_core_types::supervision::now_ms()) {
                Ok(actions) => {
                    for action in actions {
                        if let WatchdogAction::StopWorker {
                            session_id, reason, ..
                        } = action
                        {
                            tracing::warn!("steward watchdog stopping {session_id}: {reason}");
                            if let Ok(map) = rt.stop_signals.lock() {
                                if let Some(signal) = map.get(&session_id) {
                                    signal.store(true, Ordering::Relaxed);
                                }
                            }
                        }
                    }
                }
                Err(e) => tracing::warn!("steward watchdog tick failed: {e}"),
            }
        }
    });
}

/// Keep a long LLM/tool call alive from the Watchdog's point of view even when
/// no tool-boundary event is emitted.
pub fn heartbeat(session_id: &str) {
    if !enabled_hint() {
        return;
    }
    let Some(rt) = runtime() else { return };
    if !rt.enabled() {
        return;
    }
    let mut recorders = match rt.recorders.lock() {
        Ok(g) => g,
        Err(_) => return,
    };
    let rec = recorders.entry(session_id.to_string()).or_insert_with(|| {
        let _ = rt.store.ensure_ticket(session_id, session_id, "Session");
        Recorder::new(rt.store.clone(), session_id, session_id)
    });
    let _ = rec.store_heartbeat();
}

/// Evaluate the session and return a single L1 nudge, throttled to once a
/// minute. OffTrack returns no nudge: escalation is a human decision.
pub fn evaluate_and_nudge(session_id: &str) -> Option<String> {
    if !enabled_hint() {
        return None;
    }
    let rt = runtime()?;
    if !rt.enabled() {
        return None;
    }
    let now = oz_core_types::supervision::now_ms();
    let map = NUDGE_AT.get_or_init(|| Mutex::new(HashMap::new()));
    {
        let g = map.lock().ok()?;
        if let Some(prev) = g.get(session_id) {
            if now.saturating_sub(*prev) < 60_000 {
                return None;
            }
        }
    }
    let sup = Supervisor::new(rt.store.clone());
    let judgment = sup.evaluate(session_id, false, false).ok()??;
    if let Some(oz_steward::Action::Nudge(text)) = judgment.action {
        if let Ok(mut g) = map.lock() {
            g.insert(session_id.to_string(), now);
        }
        Some(text)
    } else {
        None
    }
}

/// Feed one runtime event into the steward. No-op when disabled or when the
/// store failed to open; must never abort the agent run.
pub fn register_stop_signal(session_id: &str, signal: Arc<AtomicBool>) {
    if let Some(rt) = runtime() {
        rt.register_stop_signal(session_id, signal);
    }
}

pub fn record_event(session_id: &str, event: &StreamEvent) {
    if !enabled_hint() {
        return;
    }
    if let Some(rt) = runtime() {
        rt.record(session_id, event);
    }
}

#[tauri::command]
pub fn steward_status() -> Result<Value, String> {
    let Some(rt) = runtime() else {
        return Ok(json!({"available": false, "enabled": false}));
    };
    let tickets = rt.store.snapshots(50).map_err(|e| e.to_string())?;
    let inbox = rt.store.inbox().map_err(|e| e.to_string())?;
    Ok(json!({
        "available": true,
        "enabled": rt.enabled(),
        "db_path": rt.db_path.to_string_lossy(),
        "tickets": tickets.len(),
        "inbox": inbox.len(),
        "steps": tickets.iter().map(|t| t.step_count).sum::<u64>(),
    }))
}

#[tauri::command]
pub fn steward_set_enabled(enabled: bool) -> Result<bool, String> {
    let Some(rt) = runtime() else {
        return Err("steward store unavailable".into());
    };
    rt.set_enabled(enabled);
    ENABLED_HINT.store(enabled, Ordering::Relaxed);
    if enabled {
        start_watchdog_inner(true);
    }
    Ok(rt.enabled())
}

#[tauri::command]
pub fn steward_snapshot(limit: Option<usize>) -> Result<Vec<TicketSnapshot>, String> {
    let Some(rt) = runtime() else {
        return Ok(Vec::new());
    };
    rt.store
        .snapshots(limit.unwrap_or(50).min(200))
        .map_err(|e| e.to_string())
}

#[tauri::command]
pub fn steward_inbox() -> Result<Vec<TicketSnapshot>, String> {
    let Some(rt) = runtime() else {
        return Ok(Vec::new());
    };
    rt.store.inbox().map_err(|e| e.to_string())
}

#[tauri::command]
pub fn steward_steps(ticket_id: String, limit: Option<usize>) -> Result<Vec<StepRecord>, String> {
    let Some(rt) = runtime() else {
        return Ok(Vec::new());
    };
    rt.store
        .steps(&ticket_id, limit.unwrap_or(50).min(500))
        .map_err(|e| e.to_string())
}

#[tauri::command]
pub fn steward_sweep() -> Result<usize, String> {
    let Some(rt) = runtime() else {
        return Ok(0);
    };
    let sup = Supervisor::new(rt.store.clone());
    sup.sweep(oz_core_types::supervision::now_ms())
        .map(|v| v.len())
        .map_err(|e| e.to_string())
}

#[tauri::command]
pub fn steward_evaluate(
    ticket_id: String,
    claimed_done: bool,
    verified: bool,
) -> Result<Option<oz_steward::DeterministicJudgment>, String> {
    let Some(rt) = runtime() else {
        return Ok(None);
    };
    let sup = Supervisor::new(rt.store.clone());
    sup.evaluate(&ticket_id, claimed_done, verified)
        .map_err(|e| e.to_string())
}

#[tauri::command]
pub fn steward_start_ticket(session_id: String, title: String) -> Result<bool, String> {
    let Some(rt) = runtime() else {
        return Err("steward store unavailable".into());
    };
    rt.store
        .ensure_ticket(&session_id, &session_id, &title)
        .map(|_| true)
        .map_err(|e| e.to_string())
}

#[tauri::command]
pub fn steward_save_contract(
    ticket_id: String,
    goal: String,
    acceptance: Vec<String>,
    allowed_paths: Vec<String>,
    risk: String,
    confirmed: bool,
) -> Result<u32, String> {
    let Some(rt) = runtime() else {
        return Err("steward store unavailable".into());
    };
    let risk = match risk.as_str() {
        "high" => RiskLevel::High,
        "medium" => RiskLevel::Medium,
        _ => RiskLevel::Low,
    };
    let acceptance = acceptance
        .into_iter()
        .filter(|s| !s.trim().is_empty())
        .enumerate()
        .map(|(i, description)| Acceptance {
            id: format!("a{i}"),
            description,
        })
        .collect::<Vec<_>>();
    let mut contract = TaskContract::new(&ticket_id, goal, acceptance, risk, confirmed)
        .map_err(|e| e.to_string())?;
    contract.allowed_paths = allowed_paths;
    contract.budget = ContractBudget::default();
    rt.store
        .save_contract(&contract)
        .map_err(|e| e.to_string())?;
    Ok(contract.version)
}

#[tauri::command]
pub fn steward_latest_contract(ticket_id: String) -> Result<Option<TaskContract>, String> {
    let Some(rt) = runtime() else {
        return Ok(None);
    };
    rt.store
        .latest_contract(&ticket_id)
        .map_err(|e| e.to_string())
}

#[tauri::command]
pub fn steward_startlux_license_status() -> Value {
    json!({
        "acknowledged": startlux_acknowledged(),
        "path": startlux_ack_path().to_string_lossy(),
        "weights": "CC BY-NC 4.0",
        "commercial_supported_by_ack": false,
    })
}

#[tauri::command]
pub fn steward_ack_startlux_license(commercial: bool) -> Result<bool, String> {
    if commercial {
        return Err("commercial use requires a StartLux commercial license; this acknowledgement is non-commercial only".into());
    }
    let path = startlux_ack_path();
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    }
    let body = format!(
        "acknowledged_at={}\nweights=CC-BY-NC-4.0\n",
        chrono::Utc::now().to_rfc3339()
    );
    std::fs::write(&path, body).map_err(|e| e.to_string())?;
    Ok(true)
}

#[tauri::command]
pub async fn steward_startlux_health() -> Result<Value, String> {
    require_startlux_license()?;
    let cfg = startlux_config()?;
    let provider = StartLuxProvider::new(cfg).map_err(|e| e.to_string())?;
    provider.health().await.map_err(|e| e.to_string())
}

#[tauri::command]
pub async fn steward_semantic_evaluate(
    ticket_id: String,
    claimed_done: bool,
    verified: bool,
) -> Result<Option<oz_steward::DeterministicJudgment>, String> {
    require_startlux_license()?;
    let Some(rt) = runtime() else {
        return Ok(None);
    };
    let cfg = startlux_config()?;
    let provider = StartLuxProvider::new(cfg).map_err(|e| e.to_string())?;
    let sup = Supervisor::new(rt.store.clone());
    sup.semantic_evaluate(&ticket_id, &provider, claimed_done, verified)
        .await
        .map_err(|e| e.to_string())
}

#[tauri::command]
pub fn steward_license_check(use_case: String, acknowledged: bool, commercial: bool) -> String {
    let use_case = if use_case == "commercial" {
        license::UseCase::Commercial
    } else {
        license::UseCase::Personal
    };
    format!("{:?}", license::check(use_case, acknowledged, commercial))
}

#[tauri::command]
pub fn steward_compose_report(
    ticket_id: String,
    verified: bool,
    gates: Vec<GateReport>,
) -> Result<Report, String> {
    let Some(rt) = runtime() else {
        return Err("steward store unavailable".into());
    };
    ReportComposer::new(rt.store.clone())
        .compose(&ticket_id, verified, gates)
        .map_err(|e| e.to_string())
}

#[tauri::command]
pub fn steward_latest_report(ticket_id: String) -> Result<Option<Report>, String> {
    let Some(rt) = runtime() else {
        return Ok(None);
    };
    ReportComposer::new(rt.store.clone())
        .latest(&ticket_id)
        .map_err(|e| e.to_string())
}

#[tauri::command]
pub fn steward_propose_correction(
    ticket_id: String,
    claimed_done: bool,
    verified: bool,
) -> Result<Option<oz_steward::CorrectionPlan>, String> {
    let Some(rt) = runtime() else {
        return Ok(None);
    };
    let sup = Supervisor::new(rt.store.clone());
    let Some(judgment) = sup
        .evaluate(&ticket_id, claimed_done, verified)
        .map_err(|e| e.to_string())?
    else {
        return Ok(None);
    };
    let mut engine = rt
        .corrections
        .lock()
        .map_err(|_| "correction lock".to_string())?;
    let plan = engine.propose(
        &ticket_id,
        judgment.verdict,
        &judgment.findings,
        oz_core_types::supervision::now_ms(),
    );
    if let Some(plan) = &plan {
        engine.record(&rt.store, plan).map_err(|e| e.to_string())?;
    }
    Ok(plan)
}

#[tauri::command]
pub fn steward_snapshot_files(paths: Vec<String>) -> Result<(), String> {
    let Some(rt) = runtime() else {
        return Err("steward store unavailable".into());
    };
    rt.journal
        .lock()
        .map_err(|_| "journal lock".to_string())?
        .snapshot(&paths)
        .map_err(|e| e.to_string())
}

#[tauri::command]
pub fn steward_rollback_files(ticket_id: String) -> Result<oz_steward::RollbackReport, String> {
    let Some(rt) = runtime() else {
        return Err("steward store unavailable".into());
    };
    rt.journal
        .lock()
        .map_err(|_| "journal lock".to_string())?
        .rollback_ticket(&ticket_id)
        .map_err(|e| e.to_string())
}

#[tauri::command]
pub async fn steward_llm_judge(ticket_id: String) -> Result<Option<JudgeOutcome>, String> {
    require_startlux_license()?;
    let Some(rt) = runtime() else {
        return Ok(None);
    };
    let sup = Supervisor::new(rt.store.clone());
    let Some(det) = sup
        .evaluate(&ticket_id, false, false)
        .map_err(|e| e.to_string())?
    else {
        return Ok(None);
    };
    let contract = rt
        .store
        .latest_contract(&ticket_id)
        .map_err(|e| e.to_string())?;
    let mut steps = rt.store.steps(&ticket_id, 50).map_err(|e| e.to_string())?;
    steps.reverse();
    let cfg = startlux_config()?;
    let provider = StartLuxProvider::new(cfg).map_err(|e| e.to_string())?;
    let judge = LLMJudge::new(&provider);
    judge
        .judge(contract.as_ref(), &steps, &det.findings, det.verdict)
        .await
        .map(Some)
        .map_err(|e| e.to_string())
}

/// Ensure a Goal is represented as a Steward ticket + contract so the same
/// ledger/detectors/reports see it. Constraints are advisory strings here;
/// path allow-lists remain an explicit part of the later contract model.
pub fn store_handle() -> Option<Arc<Store>> {
    runtime().map(|r| r.store.clone())
}

pub fn ensure_goal_ticket(
    goal_id: &str,
    session_id: &str,
    title: &str,
    objective: &str,
    _constraints: &[String],
) {
    let Some(rt) = runtime() else {
        return;
    };
    if rt.store.ensure_ticket(goal_id, session_id, title).is_err() {
        return;
    }
    if rt.store.latest_contract(goal_id).ok().flatten().is_some() {
        return;
    }
    let contract = TaskContract::new(
        goal_id,
        objective,
        vec![Acceptance {
            id: "goal-gate".into(),
            description: "Goal runtime gates pass".into(),
        }],
        RiskLevel::Low,
        true,
    );
    if let Ok(contract) = contract {
        let _ = rt.store.save_contract(&contract);
    }
}
