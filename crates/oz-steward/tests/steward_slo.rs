//! Local SLO smoke: detector and store write throughput on a fixed fixture.
//! This is not a substitute for a 30-minute real-data run; it catches
//! pathological regressions in the hot path.

use std::sync::Arc;
use std::time::Instant;

use oz_core_types::supervision::{Phase, StepRecord, StepStatus};
use oz_steward::detect::{detect_all, Observation};
use oz_steward::Store;

fn step(i: usize) -> StepRecord {
    StepRecord {
        id: format!("s{i}"),
        ticket_id: "t".into(),
        session_id: "s".into(),
        turn: i as u32,
        phase: Phase::Acting,
        at: i as i64,
        tool: None,
        status: StepStatus::Succeeded,
        artifacts: vec![],
        targets: vec![],
        plan_hash: None,
        git_sha: None,
        tokens_in: 1,
        tokens_out: 1,
    }
}

#[test]
fn detector_and_store_slo_smoke() {
    let steps = (0..50).map(step).collect::<Vec<_>>();
    let obs = Observation::new(&steps, 100);
    let t = Instant::now();
    for _ in 0..2_000 {
        let _ = detect_all(&obs);
    }
    let detector_ms = t.elapsed().as_secs_f64() * 1000.0;
    assert!(
        detector_ms < 2_000.0,
        "detector 2000 runs took {detector_ms}ms"
    );

    let store = Arc::new(Store::open_memory().unwrap());
    store.ensure_ticket("t", "s", "title").unwrap();
    let t = Instant::now();
    for i in 0..1_000 {
        store.record_step(&step(i)).unwrap();
    }
    let store_ms = t.elapsed().as_secs_f64() * 1000.0;
    assert!(store_ms < 2_000.0, "1000 store writes took {store_ms}ms");
    println!("[steward-slo] detector_2000_ms={detector_ms:.2} store_1000_ms={store_ms:.2}");
}
