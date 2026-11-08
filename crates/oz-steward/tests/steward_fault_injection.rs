//! Ten deterministic fault-injection scenarios for the steward.
//!
//! This is the library-level e2e gate: it exercises the same paths the Tauri
//! app uses (observation → detectors → verdict → correction/report/watchdog/
//! rollback) without requiring a model or a running app.

use std::sync::Arc;

use oz_core_types::supervision::{
    Phase, StepRecord, StepStatus, TicketStatus, ToolRecord, Verdict,
};
use oz_steward::contract::{Acceptance, RiskLevel, TaskContract};
use oz_steward::detect::{detect_all, FindingKind, Observation, Severity};
use oz_steward::intervene::{CorrectionEngine, CorrectionLevel};
use oz_steward::judge::deterministic;
use oz_steward::report::{GateReport, ReportComposer, ReportStatus};
use oz_steward::rollback::FileJournal;
use oz_steward::watchdog::{Watchdog, WatchdogAction};
use oz_steward::Store;

fn step(id: &str, at: i64, tool: &str, target: &str) -> StepRecord {
    StepRecord {
        id: id.into(),
        ticket_id: "t".into(),
        session_id: "s".into(),
        turn: 1,
        phase: Phase::Acting,
        at,
        tool: Some(ToolRecord {
            name: tool.into(),
            args_hash: format!("{tool}-{target}-hash"),
            output_hash: None,
            duration_ms: Some(1),
            status: StepStatus::Succeeded,
        }),
        status: StepStatus::Succeeded,
        artifacts: if target.is_empty() {
            vec![]
        } else {
            vec![target.into()]
        },
        targets: if target.is_empty() {
            vec![]
        } else {
            vec![target.into()]
        },
        plan_hash: None,
        git_sha: None,
        tokens_in: 1,
        tokens_out: 1,
    }
}

fn contract() -> TaskContract {
    let mut c = TaskContract::new(
        "t",
        "only touch src",
        vec![Acceptance {
            id: "a".into(),
            description: "src changed".into(),
        }],
        RiskLevel::Low,
        true,
    )
    .unwrap();
    c.allowed_paths = vec!["src".into()];
    c
}

#[test]
fn scenario_01_stall_is_detected() {
    let steps = vec![step("s1", 1, "read", "src/a.rs")];
    let mut obs = Observation::new(&steps, 100_000);
    obs.heartbeat_at = Some(1);
    assert!(detect_all(&obs)
        .iter()
        .any(|f| f.kind == FindingKind::Stall));
}

#[test]
fn scenario_02_loop_is_detected() {
    let steps = vec![
        step("s1", 1, "read", "src/a.rs"),
        step("s2", 2, "read", "src/a.rs"),
        step("s3", 3, "read", "src/a.rs"),
    ];
    assert!(detect_all(&Observation::new(&steps, 4))
        .iter()
        .any(|f| f.kind == FindingKind::Loop));
}

#[test]
fn scenario_03_out_of_scope_write_is_critical() {
    let c = contract();
    let steps = vec![step("s1", 1, "write", "Cargo.toml")];
    let mut obs = Observation::new(&steps, 2);
    obs.contract = Some(&c);
    assert!(detect_all(&obs)
        .iter()
        .any(|f| f.kind == FindingKind::Scope && f.severity == Severity::Critical));
}

#[test]
fn scenario_04_code_run_scope_unknown_is_visible() {
    let c = contract();
    let steps = vec![step("s1", 1, "code_run", "")];
    let mut obs = Observation::new(&steps, 2);
    obs.contract = Some(&c);
    assert!(detect_all(&obs)
        .iter()
        .any(|f| f.kind == FindingKind::Scope && f.severity == Severity::Warn));
}

#[test]
fn scenario_05_budget_uses_high_water_mark() {
    let c = contract();
    let mut s1 = step("s1", 1, "read", "src/a.rs");
    s1.tokens_in = 1_500_000;
    let mut s2 = step("s2", 2, "read", "src/b.rs");
    s2.tokens_in = 2_100_000;
    let steps = [s1, s2];
    let mut obs = Observation::new(&steps, 3);
    obs.contract = Some(&c);
    assert!(detect_all(&obs)
        .iter()
        .any(|f| f.kind == FindingKind::Budget));
}

#[test]
fn scenario_06_false_completion_is_off_track() {
    let c = contract();
    let steps = vec![step("s1", 1, "write", "src/a.rs")];
    let mut obs = Observation::new(&steps, 2);
    obs.contract = Some(&c);
    obs.claimed_done = true;
    obs.verified = false;
    let j = deterministic(detect_all(&obs));
    assert_eq!(j.verdict, Verdict::OffTrack);
}

#[test]
fn scenario_07_correction_escalates() {
    let mut engine = CorrectionEngine::default();
    let f = vec![oz_steward::Finding {
        kind: FindingKind::Loop,
        severity: Severity::Warn,
        evidence_ids: vec!["s1".into()],
        detail: "loop".into(),
    }];
    assert_eq!(
        engine
            .propose("t", Verdict::AtRisk, &f, 1_000)
            .unwrap()
            .level,
        CorrectionLevel::L1
    );
    assert_eq!(
        engine
            .propose("t", Verdict::AtRisk, &f, 61_000)
            .unwrap()
            .level,
        CorrectionLevel::L1
    );
    assert_eq!(
        engine
            .propose("t", Verdict::AtRisk, &f, 121_000)
            .unwrap()
            .level,
        CorrectionLevel::L2
    );
}

#[test]
fn scenario_08_report_requires_evidence() {
    let store = Arc::new(Store::open_memory().unwrap());
    store.ensure_ticket("t", "s", "title").unwrap();
    store
        .record_step(&step("s1", 1, "write", "src/a.rs"))
        .unwrap();
    let composer = ReportComposer::new(store.clone());
    let unverified = composer.compose("t", false, vec![]).unwrap();
    assert_eq!(unverified.status, ReportStatus::Unverified);
    let achieved = composer
        .compose(
            "t",
            true,
            vec![GateReport {
                name: "test".into(),
                passed: true,
                detail: "exit 0".into(),
            }],
        )
        .unwrap();
    assert_eq!(achieved.status, ReportStatus::Achieved);
}

#[test]
fn scenario_09_watchdog_stops_stale_worker() {
    let store = Arc::new(Store::open_memory().unwrap());
    store.ensure_ticket("t", "s", "title").unwrap();
    store
        .record_step(&step("s1", 1, "write", "src/a.rs"))
        .unwrap();
    store.set_times_for_test("t", 1, 1).unwrap();
    let actions = Watchdog::new(store.clone(), 90_000)
        .tick(1_000_000)
        .unwrap();
    assert!(actions
        .iter()
        .any(|a| matches!(a, WatchdogAction::StopWorker { .. })));
    assert_eq!(
        store.snapshot("t").unwrap().unwrap().status,
        TicketStatus::Stalled
    );
}

#[test]
fn scenario_10_rollback_restores_only_journaled_files() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("work");
    std::fs::create_dir_all(root.join("src")).unwrap();
    std::fs::write(root.join("src/a.rs"), "before").unwrap();
    std::fs::write(root.join("src/untouched.rs"), "keep").unwrap();
    let mut journal = FileJournal::new(&root, dir.path().join("backup")).unwrap();
    journal.snapshot(&["src/a.rs".into()]).unwrap();
    std::fs::write(root.join("src/a.rs"), "after").unwrap();
    journal.rollback().unwrap();
    assert_eq!(
        std::fs::read_to_string(root.join("src/a.rs")).unwrap(),
        "before"
    );
    assert_eq!(
        std::fs::read_to_string(root.join("src/untouched.rs")).unwrap(),
        "keep"
    );
}
