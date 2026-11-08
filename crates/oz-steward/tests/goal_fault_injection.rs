//! Eight Goal Mode fault scenarios. These are the G4 contract: no fake
//! completion, bounded retries, human gate, budget, no-progress, lease and
//! duplicate-runner isolation.

use std::sync::Arc;
use std::time::Duration;

use oz_steward::goal::gate::GateRunner;
use oz_steward::goal::runtime::{GoalDecision, GoalRuntime};
use oz_steward::goal::spec::{GateResult, GoalGate, GoalSpec, GoalStatus};
use oz_steward::goal::store::GoalStore;

fn spec(id: &str) -> GoalSpec {
    let mut s = GoalSpec::new(id, "test goal", "make the gate pass");
    s.budget.max_attempts = 5;
    s.budget.max_consecutive_failures = 5;
    s.budget.max_no_progress_rounds = 5;
    s
}

fn gate(passed: bool) -> GateResult {
    GateResult {
        gate_index: 0,
        passed,
        pending: false,
        exit_code: Some(if passed { 0 } else { 1 }),
        output: if passed { "ok".into() } else { "failed".into() },
        duration_ms: 1,
        evidence_id: "gate:0:test".into(),
    }
}

#[tokio::test]
async fn scenario_01_command_gate_pass_achieves() {
    let dir = tempfile::tempdir().unwrap();
    let store = Arc::new(GoalStore::open_memory().unwrap());
    let mut s = spec("g1");
    s.gates = vec![GoalGate::Command {
        command: "true".into(),
        timeout_secs: 5,
        retries: 0,
    }];
    store.create(s).unwrap();
    let rt = GoalRuntime::new(store.clone(), "r1");
    rt.start("g1").unwrap();
    let gates = GateRunner::new(dir.path())
        .run_all(&store.get("g1").unwrap().unwrap().spec.gates)
        .await;
    let results = gates.into_iter().map(|g| g.result).collect::<Vec<_>>();
    assert_eq!(
        rt.complete_attempt("g1", "done", 1, None, results)
            .await
            .unwrap(),
        GoalDecision::Achieved
    );
    assert_eq!(
        store.get("g1").unwrap().unwrap().status,
        GoalStatus::Achieved
    );
}

#[tokio::test]
async fn scenario_02_three_failures_then_success() {
    let store = Arc::new(GoalStore::open_memory().unwrap());
    let mut s = spec("g2");
    s.gates = vec![GoalGate::Command {
        command: "true".into(),
        timeout_secs: 5,
        retries: 0,
    }];
    s.budget.max_consecutive_failures = 4;
    s.budget.max_no_progress_rounds = 4;
    store.create(s).unwrap();
    let rt = GoalRuntime::new(store, "r1");
    rt.start("g2").unwrap();
    for _ in 0..3 {
        assert_eq!(
            rt.complete_attempt("g2", "fail", 1, None, vec![gate(false)])
                .await
                .unwrap(),
            GoalDecision::Continue
        );
    }
    assert_eq!(
        rt.complete_attempt("g2", "ok", 1, None, vec![gate(true)])
            .await
            .unwrap(),
        GoalDecision::Achieved
    );
}

#[tokio::test]
async fn scenario_03_budget_exhausted() {
    let store = Arc::new(GoalStore::open_memory().unwrap());
    let mut s = spec("g3");
    s.budget.max_attempts = 1;
    store.create(s).unwrap();
    let rt = GoalRuntime::new(store.clone(), "r1");
    rt.start("g3").unwrap();
    assert_eq!(
        rt.complete_attempt("g3", "fail", 1, None, vec![gate(false)])
            .await
            .unwrap(),
        GoalDecision::Exhausted
    );
    assert_eq!(
        store.get("g3").unwrap().unwrap().status,
        GoalStatus::Exhausted
    );
}

#[tokio::test]
async fn scenario_04_no_progress_blocks() {
    let store = Arc::new(GoalStore::open_memory().unwrap());
    let mut s = spec("g4");
    s.budget.max_no_progress_rounds = 2;
    s.budget.max_consecutive_failures = 5;
    store.create(s).unwrap();
    let rt = GoalRuntime::new(store.clone(), "r1");
    rt.start("g4").unwrap();
    assert_eq!(
        rt.complete_attempt("g4", "fail", 1, None, vec![gate(false)])
            .await
            .unwrap(),
        GoalDecision::Continue
    );
    assert_eq!(
        rt.complete_attempt("g4", "fail", 1, None, vec![gate(false)])
            .await
            .unwrap(),
        GoalDecision::Blocked
    );
    assert_eq!(
        store.get("g4").unwrap().unwrap().status,
        GoalStatus::Blocked
    );
}

#[tokio::test]
async fn scenario_05_false_completion_never_achieves() {
    let store = Arc::new(GoalStore::open_memory().unwrap());
    let mut s = spec("g5");
    s.gates = vec![GoalGate::Command {
        command: "false".into(),
        timeout_secs: 5,
        retries: 0,
    }];
    store.create(s).unwrap();
    let rt = GoalRuntime::new(store.clone(), "r1");
    rt.start("g5").unwrap();
    let d = rt
        .complete_attempt("g5", "I am done", 1, None, vec![gate(false)])
        .await
        .unwrap();
    assert_ne!(d, GoalDecision::Achieved);
    assert_ne!(
        store.get("g5").unwrap().unwrap().status,
        GoalStatus::Achieved
    );
}

#[tokio::test]
async fn scenario_06_human_gate_stops_then_accepts() {
    let store = Arc::new(GoalStore::open_memory().unwrap());
    let mut s = spec("g6");
    s.gates = vec![GoalGate::Human {
        prompt: "accept".into(),
    }];
    store.create(s).unwrap();
    let rt = GoalRuntime::new(store.clone(), "r1");
    rt.start("g6").unwrap();
    let human = GateResult {
        gate_index: 0,
        passed: false,
        pending: true,
        exit_code: None,
        output: "accept".into(),
        duration_ms: 0,
        evidence_id: "gate:human".into(),
    };
    assert_eq!(
        rt.complete_attempt("g6", "wait", 1, None, vec![human])
            .await
            .unwrap(),
        GoalDecision::Verify
    );
    assert_eq!(
        store.get("g6").unwrap().unwrap().status,
        GoalStatus::Verifying
    );
    rt.accept("g6", "human approved").unwrap();
    assert_eq!(
        store.get("g6").unwrap().unwrap().status,
        GoalStatus::Achieved
    );
}

#[tokio::test]
async fn scenario_07_expired_lease_recovers_paused() {
    let store = Arc::new(GoalStore::open_memory().unwrap());
    let mut s = spec("g7");
    s.gates = vec![GoalGate::Human {
        prompt: "accept".into(),
    }];
    store.create(s).unwrap();
    store.set_status("g7", GoalStatus::Active, "start").unwrap();
    assert!(store.acquire_lease("g7", "r1", 1).unwrap());
    tokio::time::sleep(Duration::from_millis(5)).await;
    assert_eq!(store.sweep_expired_leases().unwrap(), vec!["g7"]);
    assert_eq!(store.get("g7").unwrap().unwrap().status, GoalStatus::Paused);
    assert!(store.acquire_lease("g7", "r2", 60_000).unwrap());
}

#[tokio::test]
async fn scenario_08_duplicate_runner_rejected_and_stop_wins() {
    let store = Arc::new(GoalStore::open_memory().unwrap());
    let mut s = spec("g8");
    s.gates = vec![GoalGate::Human {
        prompt: "accept".into(),
    }];
    store.create(s).unwrap();
    let a = GoalRuntime::new(store.clone(), "runner-a");
    let b = GoalRuntime::new(store.clone(), "runner-b");
    a.start("g8").unwrap();
    assert!(b.start("g8").is_err());
    assert_eq!(a.stop("g8", "user stop").unwrap(), GoalStatus::Abandoned);
    assert_eq!(
        store.get("g8").unwrap().unwrap().status,
        GoalStatus::Abandoned
    );
}
