//! End-to-end lifecycle of the butler task graph (no Tauri, no model).
//!
//! Covers: dependency promotion, dispatch transitions, verification verdicts,
//! retry, approval gating and honest `unverified` outcomes.

use std::sync::Arc;

use oz_steward::butler::{
    compose, verify, Approval, ButlerStore, Reversibility, Task, TaskStatus, Verdict,
};
use oz_steward::goal::spec::GoalGate;
use tempfile::tempdir;

fn cmd(c: &str) -> GoalGate {
    GoalGate::Command {
        command: c.into(),
        timeout_secs: 5,
        retries: 0,
    }
}

#[tokio::test]
async fn full_lifecycle_promotes_dependencies_then_verifies() {
    let dir = tempdir().unwrap();
    std::fs::write(
        dir.path().join("Cargo.toml"),
        "[package]\nname = \"butler_e2e_probe\"\nversion = \"0.1.0\"\nedition = \"2021\"\n",
    )
    .unwrap();
    std::fs::create_dir_all(dir.path().join("src")).unwrap();
    std::fs::write(
        dir.path().join("src/lib.rs"),
        "pub fn ok() -> bool { true }\n",
    )
    .unwrap();
    let store = Arc::new(ButlerStore::open_memory().unwrap());

    let mut a = Task::new("p", "butler", "first");
    a.accept = compose(&a.goal, dir.path(), &[]);
    let mut b = Task::new("p", "butler", "second");
    b.deps = vec![a.id.clone()];
    b.accept = compose(&b.goal, dir.path(), &[]);
    store.create_task(&a).unwrap();
    store.create_task(&b).unwrap();

    // Only `a` is ready at first.
    let ready = store.promote_and_list_ready("p").unwrap();
    assert_eq!(ready.len(), 1);
    assert_eq!(ready[0].id, a.id);

    // Dispatch a: running -> verifying -> verified.
    store.set_status(&a.id, TaskStatus::Running, "go").unwrap();
    store
        .set_status(&a.id, TaskStatus::Verifying, "done")
        .unwrap();
    let report = verify(&a, dir.path(), None).await;
    assert_eq!(report.verdict, Verdict::Verified);
    store
        .finish(&a.id, TaskStatus::Verified, &report.gates, &[], &[])
        .unwrap();

    // Now b becomes ready.
    let ready = store.promote_and_list_ready("p").unwrap();
    assert_eq!(ready.len(), 1);
    assert_eq!(ready[0].id, b.id);
    assert!(!store.all_terminal("p").unwrap());
}

#[tokio::test]
async fn failed_verification_retries_then_settles() {
    let dir = tempdir().unwrap();
    let store = ButlerStore::open_memory().unwrap();
    let mut t = Task::new("p", "b", "must pass");
    t.accept = vec![cmd("false")];
    t.max_attempts = 2;
    store.create_task(&t).unwrap();
    store
        .set_status(&t.id, TaskStatus::Ready, "deps ok")
        .unwrap();
    store.set_status(&t.id, TaskStatus::Running, "go").unwrap();
    let _ = store.bump_attempt(&t.id).unwrap();
    store
        .set_status(&t.id, TaskStatus::Verifying, "done")
        .unwrap();

    let report = verify(&t, dir.path(), None).await;
    assert_eq!(report.verdict, Verdict::Failed);
    // Scheduler policy: retry while attempts remain.
    let attempt = store.get(&t.id).unwrap().unwrap().attempt;
    assert!(attempt < t.max_attempts);
    store.set_status(&t.id, TaskStatus::Ready, "retry").unwrap();
    assert_eq!(store.get(&t.id).unwrap().unwrap().status, TaskStatus::Ready);
}

#[tokio::test]
async fn weak_evidence_is_unverified_not_verified() {
    let dir = tempdir().unwrap();
    std::fs::write(dir.path().join("out.md"), "hello\n".repeat(200)).unwrap();
    let store = ButlerStore::open_memory().unwrap();
    let mut t = Task::new("p", "b", "write a doc");
    t.accept = compose(&t.goal, dir.path(), &["out.md".into()]);
    store.create_task(&t).unwrap();
    store
        .set_status(&t.id, TaskStatus::Ready, "deps ok")
        .unwrap();
    store.set_status(&t.id, TaskStatus::Running, "go").unwrap();
    store
        .set_status(&t.id, TaskStatus::Verifying, "done")
        .unwrap();
    let report = verify(&t, dir.path(), None).await;
    assert_eq!(report.verdict, Verdict::Unverified);
    store
        .finish(&t.id, TaskStatus::Unverified, &report.gates, &[], &[])
        .unwrap();
    assert!(store.get(&t.id).unwrap().unwrap().status.is_terminal());
}

#[test]
fn irreversible_task_waits_for_approval_then_unblocks() {
    let store = ButlerStore::open_memory().unwrap();
    let mut t = Task::new("p", "b", "publish");
    t.reversibility = Reversibility::ExternalEffect;
    store.create_task(&t).unwrap();
    store.set_status(&t.id, TaskStatus::Ready, "deps").unwrap();

    // Scheduler policy for an irreversible task: queue + block.
    assert!(t.reversibility.needs_approval());
    let mut approval = Approval::new(&t.id, "publish release");
    approval.reversibility = t.reversibility.as_str().into();
    store.create_approval(&approval).unwrap();
    store
        .set_status(&t.id, TaskStatus::Blocked, "awaiting approval")
        .unwrap();
    assert_eq!(
        store.get(&t.id).unwrap().unwrap().status,
        TaskStatus::Blocked
    );

    // Human approves; task returns to the pool.
    store.decide_approval(&approval.id, true).unwrap();
    store
        .set_status(&t.id, TaskStatus::Ready, "approval granted")
        .unwrap();
    assert_eq!(store.get(&t.id).unwrap().unwrap().status, TaskStatus::Ready);
}

#[test]
fn corrupt_task_json_degrades_instead_of_panicking() {
    let store = ButlerStore::open_memory().unwrap();
    let t = Task::new("p", "b", "x");
    store.create_task(&t).unwrap();
    {
        let conn = store.lock_for_test();
        conn.execute("UPDATE task SET accept_json='{' WHERE id=?1", [&t.id])
            .unwrap();
    }
    let loaded = store.get(&t.id).unwrap().unwrap();
    assert!(loaded.accept.is_empty());
}

#[test]
fn approval_expiry_is_bounded() {
    let store = ButlerStore::open_memory().unwrap();
    let t = Task::new("p", "b", "x");
    store.create_task(&t).unwrap();
    let mut a = Approval::new(&t.id, "action");
    a.created_at = 1;
    store.create_approval(&a).unwrap();
    assert_eq!(store.expire_approvals(1000).unwrap(), 1);
    assert!(store.pending_approval_for(&t.id).unwrap().is_none());
}
