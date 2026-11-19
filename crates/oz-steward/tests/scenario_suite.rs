//! S-layer complex scenarios for the butler task graph.
//!
//! These are *semantic* scenarios, not primitive unit cases: content flows
//! from one task into the next, failures must block dependents, a restart
//! must resume without duplicate work, and concurrency must stay bounded.
//!
//! The real desktop scheduler is exercised by the GUI suite; this file pins
//! the graph/verify/retry/approval semantics deterministically in CI.

use std::path::Path;

use oz_steward::butler::compose;
use oz_steward::butler::{verify, Approval, ButlerStore, Reversibility, Task, TaskStatus, Verdict};
use oz_steward::goal::spec::GoalGate;
use tempfile::tempdir;

fn cmd(c: &str) -> GoalGate {
    GoalGate::Command {
        command: c.into(),
        timeout_secs: 5,
        retries: 0,
    }
}

fn task(goal: &str, accept: Vec<GoalGate>, deps: Vec<String>) -> Task {
    let mut t = Task::new("p", "butler", goal);
    t.accept = accept;
    t.deps = deps;
    t.max_attempts = 3;
    t
}

async fn settle(
    store: &ButlerStore,
    dir: &Path,
    t: &Task,
    worker: impl FnOnce(&Task) -> Result<(), String>,
) -> TaskStatus {
    if let Some(cur) = store.get(&t.id).unwrap() {
        if cur.status == TaskStatus::Queued {
            store.set_status(&t.id, TaskStatus::Ready, "ready").unwrap();
        }
    }
    store.set_status(&t.id, TaskStatus::Running, "go").unwrap();
    let worker_err = worker(t).err();
    store
        .set_status(&t.id, TaskStatus::Verifying, "worker done")
        .unwrap();
    let report = verify(t, dir, None).await;
    let status = if worker_err.is_some() {
        TaskStatus::Failed
    } else {
        match report.verdict {
            Verdict::Verified => TaskStatus::Verified,
            Verdict::Unverified => TaskStatus::Unverified,
            Verdict::Failed => TaskStatus::Failed,
        }
    };
    store
        .finish(&t.id, status, &report.gates, &[], &[])
        .unwrap();
    status
}

fn init_rust_workspace(dir: &Path) {
    std::fs::write(
        dir.join("Cargo.toml"),
        "[package]\nname = \"scenario_probe\"\nversion = \"0.1.0\"\nedition = \"2021\"\n",
    )
    .unwrap();
    std::fs::create_dir_all(dir.join("src")).unwrap();
    std::fs::write(dir.join("src/lib.rs"), "pub fn ok() -> bool { true }\n").unwrap();
}

fn init_verify_tests(dir: &Path) {
    std::fs::create_dir_all(dir.join("tests")).unwrap();
    std::fs::write(
        dir.join("tests/verify.rs"),
        r#"
#[test]
fn a_ok() {
    let body = std::fs::read_to_string("a.json").unwrap();
    assert!(body.contains("\"sum\":31"));
}
#[test]
fn b_ok() {
    let body = std::fs::read_to_string("b.json").unwrap();
    assert!(body.contains("\"doubled\":62"));
}
#[test]
fn fail_case() {
    assert!(false, "intentional verification failure");
}
"#,
    )
    .unwrap();
}

fn write(dir: &Path, name: &str, body: &str) {
    std::fs::write(dir.join(name), body).unwrap();
}

async fn drive_chain(
    store: &ButlerStore,
    dir: &Path,
    phases: &[Task],
    limit: usize,
) -> Vec<TaskStatus> {
    let mut out = Vec::new();
    loop {
        let ready = store.promote_and_list_ready("p").unwrap();
        if ready.is_empty() {
            break;
        }
        for t in ready.into_iter().take(limit) {
            let idx = phases.iter().position(|p| p.id == t.id).unwrap();
            let status = settle(store, dir, &t, |_| {
                match idx {
                    0 => write(dir, "a.json", r#"{"sum":31,"values":[3,1,4,1,5,9,2,6]}"#),
                    1 => {
                        let a: serde_json::Value = serde_json::from_str(
                            &std::fs::read_to_string(dir.join("a.json")).unwrap(),
                        )
                        .unwrap();
                        let sum = a["sum"].as_i64().unwrap();
                        write(
                            dir,
                            "b.json",
                            &format!(r#"{{"source_sum":{sum},"doubled":{}}}"#, sum * 2),
                        );
                    }
                    2 => {
                        let b: serde_json::Value = serde_json::from_str(
                            &std::fs::read_to_string(dir.join("b.json")).unwrap(),
                        )
                        .unwrap();
                        let doubled = b["doubled"].as_i64().unwrap();
                        write(
                            dir,
                            "c.md",
                            &format!(
                                "# 结论\n\ntotal={}\n\n{}",
                                doubled * 2,
                                "证据。\n".repeat(200)
                            ),
                        );
                    }
                    _ => {
                        let c = std::fs::read_to_string(dir.join("c.md")).unwrap();
                        let total = c
                            .lines()
                            .find_map(|l| l.strip_prefix("total="))
                            .unwrap()
                            .trim();
                        write(dir, "d.txt", &format!("validated={total}\n"));
                    }
                }
                Ok(())
            })
            .await;
            out.push(status);
        }
    }
    out
}

#[tokio::test]
async fn content_chain_a_to_d_propagates_values() {
    let dir = tempdir().unwrap();
    init_rust_workspace(dir.path());
    let store = ButlerStore::open_memory().unwrap();
    let a = task("A 写 a.json", vec![cmd("cargo check --quiet")], vec![]);
    let b = task(
        "B 读 A 写 b.json",
        vec![cmd("cargo check --quiet")],
        vec![a.id.clone()],
    );
    let c = task(
        "C 读 B 写 c.md",
        vec![cmd("cargo check --quiet")],
        vec![b.id.clone()],
    );
    let d = task(
        "D 读 C 写 d.txt",
        vec![cmd("cargo check --quiet")],
        vec![c.id.clone()],
    );
    for t in [&a, &b, &c, &d] {
        store.create_task(t).unwrap();
    }
    let statuses = drive_chain(&store, dir.path(), &[a, b, c, d], 1).await;
    assert_eq!(statuses, vec![TaskStatus::Verified; 4], "{statuses:?}");
    let a_json: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(dir.path().join("a.json")).unwrap()).unwrap();
    let b_json: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(dir.path().join("b.json")).unwrap()).unwrap();
    assert_eq!(a_json["sum"], 31);
    assert_eq!(b_json["source_sum"], 31);
    assert_eq!(b_json["doubled"], 62);
    assert!(std::fs::read_to_string(dir.path().join("c.md"))
        .unwrap()
        .contains("total=124"));
    assert!(std::fs::read_to_string(dir.path().join("d.txt"))
        .unwrap()
        .contains("validated=124"));
}

#[tokio::test]
async fn failure_blocks_dependents() {
    let dir = tempdir().unwrap();
    let store = ButlerStore::open_memory().unwrap();
    let a = task("A 必须失败", vec![cmd("test -f a.json")], vec![]);
    let b = task(
        "B 依赖 A",
        vec![cmd("cargo check --quiet")],
        vec![a.id.clone()],
    );
    store.create_task(&a).unwrap();
    store.create_task(&b).unwrap();

    let status = settle(&store, dir.path(), &a, |_| Err("boom".into())).await;
    assert_eq!(status, TaskStatus::Failed);
    let ready = store.promote_and_list_ready("p").unwrap();
    assert!(ready.is_empty(), "B must not become ready after A failed");
    assert_eq!(
        store.get(&b.id).unwrap().unwrap().status,
        TaskStatus::Failed
    );
}

#[tokio::test]
async fn restart_resumes_without_duplicate_work() {
    let dir = tempdir().unwrap();
    init_rust_workspace(dir.path());
    let db = dir.path().join("butler.db");
    let a = task("A 写 a.json", vec![cmd("cargo check --quiet")], vec![]);
    let b = task(
        "B 读 A 写 b.json",
        vec![cmd("cargo check --quiet")],
        vec![a.id.clone()],
    );
    let c = task(
        "C 读 B 写 c.md",
        vec![cmd("cargo check --quiet")],
        vec![b.id.clone()],
    );
    let d = task(
        "D 读 C 写 d.txt",
        vec![cmd("cargo check --quiet")],
        vec![c.id.clone()],
    );
    {
        let store = ButlerStore::open(&db).unwrap();
        for t in [&a, &b, &c, &d] {
            store.create_task(t).unwrap();
        }
        assert_eq!(
            settle(&store, dir.path(), &a, |_| {
                write(dir.path(), "a.json", r#"{"sum":31}"#);
                Ok(())
            })
            .await,
            TaskStatus::Verified
        );
        let ready = store.promote_and_list_ready("p").unwrap();
        assert_eq!(ready[0].id, b.id);
        assert_eq!(
            settle(&store, dir.path(), &b, |_| {
                write(dir.path(), "b.json", r#"{"source_sum":31,"doubled":62}"#);
                Ok(())
            })
            .await,
            TaskStatus::Verified
        );
    }
    // Process restart: reopen the same DB.
    let store = ButlerStore::open(&db).unwrap();
    let ready = store.promote_and_list_ready("p").unwrap();
    assert_eq!(ready.len(), 1);
    assert_eq!(ready[0].id, c.id);
    let status = settle(&store, dir.path(), &c, |_| {
        write(
            dir.path(),
            "c.md",
            &format!("# 结论\n\ntotal=124\n\n{}", "证据。\n".repeat(200)),
        );
        Ok(())
    })
    .await;
    assert_eq!(status, TaskStatus::Verified);
    let ready = store.promote_and_list_ready("p").unwrap();
    assert_eq!(ready[0].id, d.id);
    assert_eq!(
        settle(&store, dir.path(), &d, |_| {
            write(dir.path(), "d.txt", "validated=124\n");
            Ok(())
        })
        .await,
        TaskStatus::Verified
    );
}

#[tokio::test]
async fn retry_then_succeeds_after_transient_failure() {
    let dir = tempdir().unwrap();
    init_rust_workspace(dir.path());
    let store = ButlerStore::open_memory().unwrap();
    let mut t = task(
        "A 写 a.json",
        vec![GoalGate::FileAssertion {
            path: "a.json".into(),
            must_exist: true,
            contains: None,
        }],
        vec![],
    );
    t.max_attempts = 2;
    store.create_task(&t).unwrap();
    store.set_status(&t.id, TaskStatus::Ready, "ready").unwrap();
    store
        .set_status(&t.id, TaskStatus::Running, "attempt 1")
        .unwrap();
    store
        .set_status(&t.id, TaskStatus::Verifying, "no output")
        .unwrap();
    let report = verify(&t, dir.path(), None).await;
    assert_eq!(report.verdict, Verdict::Failed);
    store.set_status(&t.id, TaskStatus::Ready, "retry").unwrap();
    store.bump_attempt(&t.id).unwrap();
    store
        .set_status(&t.id, TaskStatus::Running, "attempt 2")
        .unwrap();
    write(dir.path(), "a.json", "{}");
    store
        .set_status(&t.id, TaskStatus::Verifying, "done")
        .unwrap();
    let report = verify(&t, dir.path(), None).await;
    assert_eq!(report.verdict, Verdict::Unverified);
    store
        .finish(&t.id, TaskStatus::Unverified, &report.gates, &[], &[])
        .unwrap();
    assert_eq!(
        store.get(&t.id).unwrap().unwrap().status,
        TaskStatus::Unverified
    );
    assert_eq!(store.get(&t.id).unwrap().unwrap().attempt, 1);
}

#[tokio::test]
async fn concurrency_is_bounded_and_each_task_runs_once() {
    let dir = tempdir().unwrap();
    init_rust_workspace(dir.path());
    let store = ButlerStore::open_memory().unwrap();
    let mut tasks = Vec::new();
    for i in 0..8 {
        let t = task(
            &format!("并行 {i}"),
            vec![cmd("cargo check --quiet")],
            vec![],
        );
        store.create_task(&t).unwrap();
        tasks.push(t);
    }
    let mut runs = std::collections::HashMap::new();
    loop {
        let ready = store.promote_and_list_ready("p").unwrap();
        if ready.is_empty() {
            break;
        }
        for t in ready.into_iter().take(2) {
            *runs.entry(t.id.clone()).or_insert(0) += 1;
            let status = settle(&store, dir.path(), &t, |_| {
                write(dir.path(), "out.json", "{}");
                Ok(())
            })
            .await;
            assert_eq!(status, TaskStatus::Verified);
        }
    }
    assert_eq!(runs.len(), 8);
    assert!(runs.values().all(|n| *n == 1), "{runs:?}");
    assert!(store.all_terminal("p").unwrap());
}

#[tokio::test]
async fn approval_blocks_then_unblocks_dependents() {
    let store = ButlerStore::open_memory().unwrap();
    let mut a = task("发布版本", vec![], vec![]);
    a.reversibility = Reversibility::ExternalEffect;
    let b = task("B 依赖发布", vec![], vec![a.id.clone()]);
    store.create_task(&a).unwrap();
    store.create_task(&b).unwrap();
    store
        .set_status(&a.id, TaskStatus::Blocked, "awaiting approval")
        .unwrap();
    assert!(store.promote_and_list_ready("p").unwrap().is_empty());

    let approval = Approval::new(&a.id, "publish");
    store.create_approval(&approval).unwrap();
    store.decide_approval(&approval.id, true).unwrap();
    store
        .set_status(&a.id, TaskStatus::Ready, "approved")
        .unwrap();
    store.set_status(&a.id, TaskStatus::Running, "go").unwrap();
    store
        .set_status(&a.id, TaskStatus::Verifying, "done")
        .unwrap();
    store
        .set_status(&a.id, TaskStatus::Verified, "verified")
        .unwrap();
    let ready = store.promote_and_list_ready("p").unwrap();
    assert_eq!(ready.len(), 1);
    assert_eq!(ready[0].id, b.id);
}

#[tokio::test]
async fn cancelled_task_blocks_dependents() {
    let store = ButlerStore::open_memory().unwrap();
    let a = task("A", vec![], vec![]);
    let b = task("B", vec![], vec![a.id.clone()]);
    store.create_task(&a).unwrap();
    store.create_task(&b).unwrap();
    // Stop/fail is the lifecycle verb: cancelled work is recorded as failed.
    store
        .fail_tasks_for_butler("butler", "user cancelled")
        .unwrap();
    assert_eq!(
        store.get(&a.id).unwrap().unwrap().status,
        TaskStatus::Failed
    );
    assert!(store.promote_and_list_ready("p").unwrap().is_empty());
    assert_eq!(
        store.get(&b.id).unwrap().unwrap().status,
        TaskStatus::Failed
    );
}

#[tokio::test]
async fn attempts_are_exhausted_and_downstream_stays_blocked() {
    let dir = tempdir().unwrap();
    let store = ButlerStore::open_memory().unwrap();
    let mut a = task("A 永远失败", vec![cmd("false")], vec![]);
    a.max_attempts = 2;
    let b = task("B 依赖 A", vec![], vec![a.id.clone()]);
    store.create_task(&a).unwrap();
    store.create_task(&b).unwrap();
    store.set_status(&a.id, TaskStatus::Ready, "ready").unwrap();
    for attempt in 0..2 {
        store.set_status(&a.id, TaskStatus::Running, "go").unwrap();
        store
            .set_status(&a.id, TaskStatus::Verifying, "done")
            .unwrap();
        let report = verify(&a, dir.path(), None).await;
        assert_eq!(report.verdict, Verdict::Failed);
        store.bump_attempt(&a.id).unwrap();
        if attempt == 0 {
            store.set_status(&a.id, TaskStatus::Ready, "retry").unwrap();
        } else {
            store
                .finish(&a.id, TaskStatus::Failed, &report.gates, &[], &[])
                .unwrap();
        }
    }
    assert_eq!(
        store.get(&a.id).unwrap().unwrap().status,
        TaskStatus::Failed
    );
    assert_eq!(store.get(&a.id).unwrap().unwrap().attempt, 2);
    assert!(store.promote_and_list_ready("p").unwrap().is_empty());
    assert_eq!(
        store.get(&b.id).unwrap().unwrap().status,
        TaskStatus::Failed
    );
}

#[tokio::test]
async fn unverified_upstream_is_visible_not_silently_verified() {
    // Current policy: an Unverified dependency is allowed to unblock the
    // dependent task. This scenario pins that behavior so a future change to
    // "require verified deps" is a deliberate decision, and proves the status
    // is not silently upgraded to Verified.
    let dir = tempdir().unwrap();
    let store = ButlerStore::open_memory().unwrap();
    let a = task("A 只产弱证据", vec![cmd("test -f a.txt")], vec![]);
    let b = task("B 依赖 A", vec![], vec![a.id.clone()]);
    store.create_task(&a).unwrap();
    store.create_task(&b).unwrap();
    let status = settle(&store, dir.path(), &a, |_| {
        write(dir.path(), "a.txt", "x");
        Ok(())
    })
    .await;
    assert_eq!(status, TaskStatus::Unverified);
    assert_eq!(
        store.get(&a.id).unwrap().unwrap().status,
        TaskStatus::Unverified
    );
    let ready = store.promote_and_list_ready("p").unwrap();
    assert_eq!(ready.len(), 1, "current policy releases the dependent");
    assert_eq!(ready[0].id, b.id);
}

#[tokio::test]
async fn approval_denied_cancels_dependents() {
    let store = ButlerStore::open_memory().unwrap();
    let a = task("危险发布", vec![], vec![]);
    let b = task("B 依赖危险发布", vec![], vec![a.id.clone()]);
    store.create_task(&a).unwrap();
    store.create_task(&b).unwrap();
    store
        .set_status(&a.id, TaskStatus::Blocked, "awaiting")
        .unwrap();
    let approval = Approval::new(&a.id, "publish");
    store.create_approval(&approval).unwrap();
    store.decide_approval(&approval.id, false).unwrap();
    store
        .set_status(&a.id, TaskStatus::Failed, "denied")
        .unwrap();
    assert!(store.promote_and_list_ready("p").unwrap().is_empty());
    assert_eq!(
        store.get(&b.id).unwrap().unwrap().status,
        TaskStatus::Failed
    );
}

#[tokio::test]
async fn projects_do_not_leak_ready_tasks() {
    let store = ButlerStore::open_memory().unwrap();
    let a = Task::new("p1", "butler", "p1 task");
    let b = Task::new("p2", "butler", "p2 task");
    store.create_task(&a).unwrap();
    store.create_task(&b).unwrap();
    let ready = store.promote_and_list_ready("p1").unwrap();
    assert_eq!(ready.len(), 1);
    assert_eq!(ready[0].id, a.id);
    assert_eq!(
        store.get(&b.id).unwrap().unwrap().status,
        TaskStatus::Queued
    );
    assert!(store.promote_and_list_ready("p2").unwrap().len() == 1);
}

#[tokio::test]
async fn fanout_of_twenty_runs_each_task_once() {
    let dir = tempdir().unwrap();
    init_rust_workspace(dir.path());
    let store = ButlerStore::open_memory().unwrap();
    for i in 0..20 {
        let t = task(
            &format!("fanout {i}"),
            vec![cmd("cargo check --quiet")],
            vec![],
        );
        store.create_task(&t).unwrap();
    }
    let mut runs = std::collections::HashMap::new();
    loop {
        let ready = store.promote_and_list_ready("p").unwrap();
        if ready.is_empty() {
            break;
        }
        for t in ready.into_iter().take(4) {
            *runs.entry(t.id.clone()).or_insert(0) += 1;
            assert_eq!(
                settle(&store, dir.path(), &t, |_| Ok(())).await,
                TaskStatus::Verified
            );
        }
    }
    assert_eq!(runs.len(), 20);
    assert!(runs.values().all(|n| *n == 1));
}

#[tokio::test]
async fn large_deliverable_is_written_and_verified() {
    let dir = tempdir().unwrap();
    let store = ButlerStore::open_memory().unwrap();
    let mut t = task(
        "写 5MB 报告",
        vec![GoalGate::FileAssertion {
            path: "big.md".into(),
            must_exist: true,
            contains: None,
        }],
        vec![],
    );
    t.max_attempts = 1;
    store.create_task(&t).unwrap();
    let status = settle(&store, dir.path(), &t, |_| {
        write(dir.path(), "big.md", &"x".repeat(5 * 1024 * 1024));
        Ok(())
    })
    .await;
    assert_eq!(status, TaskStatus::Unverified);
    let len = std::fs::metadata(dir.path().join("big.md")).unwrap().len();
    assert_eq!(len, 5 * 1024 * 1024);
}

#[tokio::test]
async fn diamond_merges_two_branches() {
    let dir = tempdir().unwrap();
    init_rust_workspace(dir.path());
    let store = ButlerStore::open_memory().unwrap();
    let a = task("A 写 a.json", vec![cmd("cargo check --quiet")], vec![]);
    let b = task(
        "B 依赖 A 写 b.json",
        vec![cmd("cargo check --quiet")],
        vec![a.id.clone()],
    );
    let c = task(
        "C 依赖 A 写 c.json",
        vec![cmd("cargo check --quiet")],
        vec![a.id.clone()],
    );
    let d = task(
        "D 合并 B+C 写 d.json",
        vec![cmd("cargo check --quiet")],
        vec![b.id.clone(), c.id.clone()],
    );
    for t in [&a, &b, &c, &d] {
        store.create_task(t).unwrap();
    }
    assert_eq!(
        settle(&store, dir.path(), &a, |_| {
            write(dir.path(), "a.json", "{}");
            Ok(())
        })
        .await,
        TaskStatus::Verified
    );
    let ready = store.promote_and_list_ready("p").unwrap();
    assert_eq!(ready.len(), 2, "both branches must be ready");
    for t in ready {
        let name = if t.id == b.id { "b.json" } else { "c.json" };
        assert_eq!(
            settle(&store, dir.path(), &t, |_| {
                write(dir.path(), name, "{}");
                Ok(())
            })
            .await,
            TaskStatus::Verified
        );
    }
    let ready = store.promote_and_list_ready("p").unwrap();
    assert_eq!(ready.len(), 1);
    assert_eq!(ready[0].id, d.id);
    assert_eq!(
        settle(&store, dir.path(), &d, |_| {
            write(dir.path(), "d.json", "{}");
            Ok(())
        })
        .await,
        TaskStatus::Verified
    );
}

#[tokio::test]
async fn deep_chain_of_ten_runs_in_order() {
    let dir = tempdir().unwrap();
    init_rust_workspace(dir.path());
    let store = ButlerStore::open_memory().unwrap();
    let mut prev: Option<String> = None;
    let mut tasks = Vec::new();
    for i in 0..10 {
        let deps = prev.iter().cloned().collect();
        let t = task(
            &format!("chain {i}"),
            vec![cmd("cargo check --quiet")],
            deps,
        );
        store.create_task(&t).unwrap();
        prev = Some(t.id.clone());
        tasks.push(t);
    }
    for t in &tasks {
        let ready = store.promote_and_list_ready("p").unwrap();
        assert_eq!(ready.len(), 1, "only one task runs at a time");
        assert_eq!(ready[0].id, t.id);
        assert_eq!(
            settle(&store, dir.path(), t, |_| Ok(())).await,
            TaskStatus::Verified
        );
    }
    assert!(store.all_terminal("p").unwrap());
}

#[tokio::test]
async fn partial_fanout_failures_do_not_block_successes() {
    let dir = tempdir().unwrap();
    init_rust_workspace(dir.path());
    let store = ButlerStore::open_memory().unwrap();
    let mut tasks = Vec::new();
    for i in 0..6 {
        let gate = if i % 3 == 0 {
            cmd("false")
        } else {
            cmd("cargo check --quiet")
        };
        let t = task(&format!("fan {i}"), vec![gate], vec![]);
        store.create_task(&t).unwrap();
        tasks.push(t);
    }
    loop {
        let ready = store.promote_and_list_ready("p").unwrap();
        if ready.is_empty() {
            break;
        }
        for t in ready {
            let _ = settle(&store, dir.path(), &t, |_| Ok(())).await;
        }
    }
    let failed = tasks
        .iter()
        .filter(|t| store.get(&t.id).unwrap().unwrap().status == TaskStatus::Failed)
        .count();
    assert_eq!(failed, 2);
    assert!(store.all_terminal("p").unwrap());
}

#[tokio::test]
async fn digest_is_sent_at_most_once_per_project() {
    let store = ButlerStore::open_memory().unwrap();
    assert!(!store.digest_sent("p").unwrap());
    store.mark_digest_failed("p", "offline").unwrap();
    assert!(
        !store.digest_sent("p").unwrap(),
        "failure must not mark sent"
    );
    store.mark_digest_sent("p").unwrap();
    assert!(store.digest_sent("p").unwrap());
    store.mark_digest_sent("p").unwrap();
    assert!(store.digest_sent("p").unwrap());
}

#[tokio::test]
async fn running_task_is_not_dispatched_twice() {
    let store = ButlerStore::open_memory().unwrap();
    let t = task("once", vec![], vec![]);
    store.create_task(&t).unwrap();
    let ready = store.promote_and_list_ready("p").unwrap();
    assert_eq!(ready.len(), 1);
    store.set_status(&t.id, TaskStatus::Running, "go").unwrap();
    let ready = store.promote_and_list_ready("p").unwrap();
    assert!(ready.is_empty(), "running task must not be re-dispatched");
}

#[tokio::test]
async fn restart_while_verifying_requeues_cleanly() {
    let dir = tempdir().unwrap();
    let db = dir.path().join("butler.db");
    let t = task("verify me", vec![cmd("cargo check --quiet")], vec![]);
    {
        let store = ButlerStore::open(&db).unwrap();
        store.create_task(&t).unwrap();
        store.set_status(&t.id, TaskStatus::Ready, "ready").unwrap();
        store.set_status(&t.id, TaskStatus::Running, "go").unwrap();
        store
            .set_status(&t.id, TaskStatus::Verifying, "verifying")
            .unwrap();
    }
    let store = ButlerStore::open(&db).unwrap();
    assert_eq!(
        store.get(&t.id).unwrap().unwrap().status,
        TaskStatus::Verifying
    );
    // Recovery policy: a verifying task is put back into the ready pool.
    store
        .set_status(&t.id, TaskStatus::Ready, "restart recovery")
        .unwrap();
    let ready = store.promote_and_list_ready("p").unwrap();
    assert_eq!(ready.len(), 1);
    assert_eq!(ready[0].id, t.id);
}

#[tokio::test]
async fn hundred_task_graph_completes_once_each() {
    let dir = tempdir().unwrap();
    let store = ButlerStore::open_memory().unwrap();
    for i in 0..100 {
        let t = task(&format!("bulk {i}"), vec![cmd("true")], vec![]);
        store.create_task(&t).unwrap();
    }
    let mut seen = std::collections::HashSet::new();
    loop {
        let ready = store.promote_and_list_ready("p").unwrap();
        if ready.is_empty() {
            break;
        }
        for t in ready {
            assert!(seen.insert(t.id.clone()), "task dispatched twice");
            let _ = settle(&store, dir.path(), &t, |_| Ok(())).await;
        }
    }
    assert_eq!(seen.len(), 100);
    assert!(store.all_terminal("p").unwrap());
}

#[tokio::test]
async fn approval_expiry_blocks_dependents() {
    let store = ButlerStore::open_memory().unwrap();
    let a = task("等待审批", vec![], vec![]);
    let b = task("B 依赖审批", vec![], vec![a.id.clone()]);
    store.create_task(&a).unwrap();
    store.create_task(&b).unwrap();
    store
        .set_status(&a.id, TaskStatus::Blocked, "awaiting")
        .unwrap();
    let mut approval = Approval::new(&a.id, "publish");
    approval.created_at = 1;
    store.create_approval(&approval).unwrap();
    assert_eq!(store.expire_approvals(1000).unwrap(), 1);
    assert!(store.pending_approval_for(&a.id).unwrap().is_none());
    store
        .set_status(&a.id, TaskStatus::Failed, "approval expired")
        .unwrap();
    assert!(store.promote_and_list_ready("p").unwrap().is_empty());
    assert_eq!(
        store.get(&b.id).unwrap().unwrap().status,
        TaskStatus::Failed
    );
}

#[tokio::test]
async fn restart_preserves_pending_approval() {
    let dir = tempdir().unwrap();
    let db = dir.path().join("butler.db");
    let a = task("危险发布", vec![], vec![]);
    let b = task("B 依赖发布", vec![], vec![a.id.clone()]);
    let approval_id;
    {
        let store = ButlerStore::open(&db).unwrap();
        store.create_task(&a).unwrap();
        store.create_task(&b).unwrap();
        store
            .set_status(&a.id, TaskStatus::Blocked, "awaiting")
            .unwrap();
        let approval = Approval::new(&a.id, "publish");
        approval_id = approval.id.clone();
        store.create_approval(&approval).unwrap();
    }
    let store = ButlerStore::open(&db).unwrap();
    let pending = store
        .pending_approval_for(&a.id)
        .unwrap()
        .expect("approval survives");
    assert_eq!(pending.id, approval_id);
    assert_eq!(
        store.get(&a.id).unwrap().unwrap().status,
        TaskStatus::Blocked
    );
    assert!(store.promote_and_list_ready("p").unwrap().is_empty());
    store.decide_approval(&approval_id, true).unwrap();
    store
        .set_status(&a.id, TaskStatus::Ready, "approved")
        .unwrap();
    store.set_status(&a.id, TaskStatus::Running, "go").unwrap();
    store
        .set_status(&a.id, TaskStatus::Verifying, "done")
        .unwrap();
    store
        .set_status(&a.id, TaskStatus::Verified, "verified")
        .unwrap();
    let ready = store.promote_and_list_ready("p").unwrap();
    assert_eq!(ready.len(), 1);
    assert_eq!(ready[0].id, b.id);
}

#[tokio::test]
async fn dependency_cycle_fails_instead_of_hanging() {
    let store = ButlerStore::open_memory().unwrap();
    let a = task("A", vec![], vec![]);
    let b = task("B", vec![], vec![a.id.clone()]);
    store.create_task(&a).unwrap();
    store.create_task(&b).unwrap();
    {
        let conn = store.lock_for_test();
        conn.execute(
            "UPDATE task SET deps_json=?1 WHERE id=?2",
            rusqlite::params![serde_json::json!([b.id]).to_string(), a.id],
        )
        .unwrap();
    }
    let _ = store.promote_and_list_ready("p").unwrap();
    assert_eq!(
        store.get(&a.id).unwrap().unwrap().status,
        TaskStatus::Failed
    );
    assert_eq!(
        store.get(&b.id).unwrap().unwrap().status,
        TaskStatus::Failed
    );
}

#[tokio::test]
async fn missing_dependency_fails_the_dependent() {
    let store = ButlerStore::open_memory().unwrap();
    let t = task("孤儿任务", vec![], vec!["does-not-exist".into()]);
    store.create_task(&t).unwrap();
    let _ = store.promote_and_list_ready("p").unwrap();
    assert_eq!(
        store.get(&t.id).unwrap().unwrap().status,
        TaskStatus::Failed
    );
}

#[tokio::test]
async fn cross_project_dependency_is_rejected_at_promotion() {
    let store = ButlerStore::open_memory().unwrap();
    let a = Task::new("p1", "butler", "p1");
    let mut b = Task::new("p2", "butler", "p2");
    b.deps = vec![a.id.clone()];
    store.create_task(&a).unwrap();
    store.create_task(&b).unwrap();
    let _ = store.promote_and_list_ready("p2").unwrap();
    assert_eq!(
        store.get(&b.id).unwrap().unwrap().status,
        TaskStatus::Failed
    );
}

#[tokio::test]
async fn blocked_dependency_waits_then_releases_after_approval() {
    let store = ButlerStore::open_memory().unwrap();
    let mut a = task("危险", vec![], vec![]);
    a.reversibility = Reversibility::ExternalEffect;
    let b = task("B 依赖危险", vec![], vec![a.id.clone()]);
    store.create_task(&a).unwrap();
    store.create_task(&b).unwrap();
    store
        .set_status(&a.id, TaskStatus::Blocked, "awaiting")
        .unwrap();
    let approval = Approval::new(&a.id, "publish");
    store.create_approval(&approval).unwrap();
    assert!(store.promote_and_list_ready("p").unwrap().is_empty());
    assert_eq!(
        store.get(&b.id).unwrap().unwrap().status,
        TaskStatus::Queued
    );
    store.decide_approval(&approval.id, true).unwrap();
    store
        .set_status(&a.id, TaskStatus::Ready, "approved")
        .unwrap();
    store.set_status(&a.id, TaskStatus::Running, "go").unwrap();
    store
        .set_status(&a.id, TaskStatus::Verifying, "done")
        .unwrap();
    store
        .set_status(&a.id, TaskStatus::Verified, "verified")
        .unwrap();
    let ready = store.promote_and_list_ready("p").unwrap();
    assert_eq!(ready.len(), 1);
    assert_eq!(ready[0].id, b.id);
}

#[tokio::test]
async fn join_failure_blocks_only_the_join() {
    let dir = tempdir().unwrap();
    init_rust_workspace(dir.path());
    let store = ButlerStore::open_memory().unwrap();
    let a = task("A", vec![cmd("cargo check --quiet")], vec![]);
    let b = task("B 失败", vec![cmd("false")], vec![a.id.clone()]);
    let c = task(
        "C 成功",
        vec![cmd("cargo check --quiet")],
        vec![a.id.clone()],
    );
    let d = task(
        "D join",
        vec![cmd("cargo check --quiet")],
        vec![b.id.clone(), c.id.clone()],
    );
    for t in [&a, &b, &c, &d] {
        store.create_task(t).unwrap();
    }
    assert_eq!(
        settle(&store, dir.path(), &a, |_| Ok(())).await,
        TaskStatus::Verified
    );
    let ready = store.promote_and_list_ready("p").unwrap();
    assert_eq!(ready.len(), 2);
    for t in ready {
        let _ = settle(&store, dir.path(), &t, |_| Ok(())).await;
    }
    assert_eq!(
        store.get(&b.id).unwrap().unwrap().status,
        TaskStatus::Failed
    );
    assert_eq!(
        store.get(&c.id).unwrap().unwrap().status,
        TaskStatus::Verified
    );
    let _ = store.promote_and_list_ready("p").unwrap();
    assert_eq!(
        store.get(&d.id).unwrap().unwrap().status,
        TaskStatus::Failed
    );
}

#[tokio::test]
async fn new_task_clears_a_previously_sent_digest() {
    let store = ButlerStore::open_memory().unwrap();
    store.mark_digest_sent("p").unwrap();
    assert!(store.digest_sent("p").unwrap());
    store.clear_digest("p").unwrap();
    assert!(!store.digest_sent("p").unwrap());
    store.mark_digest_sent("p").unwrap();
    let t = task("new work", vec![], vec![]);
    store.create_task(&t).unwrap();
    store.clear_digest("p").unwrap();
    assert!(
        !store.digest_sent("p").unwrap(),
        "new work must re-open digest"
    );
}

#[tokio::test]
async fn strict_dependency_does_not_consume_unverified_upstream() {
    let dir = tempdir().unwrap();
    let store = ButlerStore::open_memory().unwrap();
    let a = task(
        "A 只产弱证据",
        vec![GoalGate::FileAssertion {
            path: "a.txt".into(),
            must_exist: true,
            contains: None,
        }],
        vec![],
    );
    let mut b = task("B 严格依赖 A", vec![], vec![a.id.clone()]);
    b.require_verified_deps = true;
    store.create_task(&a).unwrap();
    store.create_task(&b).unwrap();
    assert_eq!(
        settle(&store, dir.path(), &a, |_| {
            write(dir.path(), "a.txt", "x");
            Ok(())
        })
        .await,
        TaskStatus::Unverified
    );
    assert!(
        store.promote_and_list_ready("p").unwrap().is_empty(),
        "strict dependent must not start on unverified output"
    );
    assert_eq!(
        store.get(&b.id).unwrap().unwrap().status,
        TaskStatus::Queued
    );
    // A later re-verification (human or stronger gate) releases B.
    store
        .lock_for_test()
        .execute("UPDATE task SET status='verified' WHERE id=?1", [&a.id])
        .unwrap();
    let ready = store.promote_and_list_ready("p").unwrap();
    assert_eq!(ready.len(), 1);
    assert_eq!(ready[0].id, b.id);
}

#[tokio::test]
async fn worker_wall_budget_times_out_and_blocks_downstream() {
    use oz_steward::butler::budget::run_with_timeout;
    use std::time::Duration;

    let timeout = run_with_timeout(Duration::from_millis(30), std::future::pending::<()>()).await;
    let err = timeout.expect_err("pending future must hit the wall budget");
    assert!(err.contains("wall budget"), "{err}");
    let ok = run_with_timeout(Duration::from_secs(1), async { 7 })
        .await
        .unwrap();
    assert_eq!(ok, 7);

    let store = ButlerStore::open_memory().unwrap();
    let mut a = task("慢工人", vec![], vec![]);
    a.budget.max_wall_secs = 1;
    let b = task("B 依赖慢工人", vec![], vec![a.id.clone()]);
    store.create_task(&a).unwrap();
    store.create_task(&b).unwrap();
    store.set_status(&a.id, TaskStatus::Ready, "ready").unwrap();
    store.set_status(&a.id, TaskStatus::Running, "go").unwrap();
    let budget = run_with_timeout(Duration::from_millis(20), std::future::pending::<()>()).await;
    assert!(budget.is_err());
    store
        .finish(&a.id, TaskStatus::Failed, &[], &[], &[])
        .expect("worker timeout must settle the task");
    let _ = store.promote_and_list_ready("p").unwrap();
    assert_eq!(
        store.get(&a.id).unwrap().unwrap().status,
        TaskStatus::Failed
    );
    assert_eq!(
        store.get(&b.id).unwrap().unwrap().status,
        TaskStatus::Failed
    );
}

#[tokio::test]
async fn strong_verify_command_makes_content_task_verified() {
    let dir = tempdir().unwrap();
    init_rust_workspace(dir.path());
    init_verify_tests(dir.path());
    let store = ButlerStore::open_memory().unwrap();
    let mut a = task(
        "写 a.json\n[verify] cargo test --quiet a_ok",
        vec![],
        vec![],
    );
    a.accept = compose(&a.goal, dir.path(), &[]);
    let mut b = task(
        "写 b.json\n[verify] cargo test --quiet b_ok",
        vec![],
        vec![a.id.clone()],
    );
    b.accept = compose(&b.goal, dir.path(), &[]);
    b.require_verified_deps = true;
    store.create_task(&a).unwrap();
    store.create_task(&b).unwrap();

    assert_eq!(
        settle(&store, dir.path(), &a, |_| {
            write(dir.path(), "a.json", r#"{"sum":31}"#);
            Ok(())
        })
        .await,
        TaskStatus::Verified
    );
    let ready = store.promote_and_list_ready("p").unwrap();
    assert_eq!(ready.len(), 1);
    assert_eq!(ready[0].id, b.id);
    assert_eq!(
        settle(&store, dir.path(), &b, |_| {
            write(dir.path(), "b.json", r#"{"source_sum":31,"doubled":62}"#);
            Ok(())
        })
        .await,
        TaskStatus::Verified
    );
}

#[tokio::test]
async fn failing_verify_command_marks_the_task_failed() {
    let dir = tempdir().unwrap();
    init_rust_workspace(dir.path());
    init_verify_tests(dir.path());
    let store = ButlerStore::open_memory().unwrap();
    let mut t = task(
        "写一个不合格的交付\n[verify] cargo test --quiet fail_case",
        vec![],
        vec![],
    );
    t.accept = compose(&t.goal, dir.path(), &[]);
    store.create_task(&t).unwrap();
    let status = settle(&store, dir.path(), &t, |_| {
        write(dir.path(), "out.txt", "looks fine");
        Ok(())
    })
    .await;
    assert_eq!(status, TaskStatus::Failed);
    assert_eq!(
        store.get(&t.id).unwrap().unwrap().status,
        TaskStatus::Failed
    );
}

#[tokio::test]
async fn custom_verify_command_is_refused_without_approval() {
    if std::env::var("OPENZEN_ALLOW_CUSTOM_VERIFY").is_ok() {
        return; // explicit opt-in is covered by the environment contract
    }
    let dir = tempdir().unwrap();
    let marker = dir.path().join("must-not-exist");
    let runner = oz_steward::goal::gate::GateRunner::new(dir.path());
    let outcome = runner
        .run_gate(&GoalGate::StrongCommand {
            command: format!("touch {}", marker.display()),
            timeout_secs: 5,
        })
        .await;
    assert!(!outcome.passed);
    assert!(
        outcome.pending,
        "refusal must be pending, not a task failure"
    );
    assert!(
        outcome.output.contains("requires approval"),
        "{}",
        outcome.output
    );
    assert!(!marker.exists(), "custom command must not execute");
}

#[tokio::test]
async fn shell_chained_verify_is_refused() {
    let dir = tempdir().unwrap();
    let marker = dir.path().join("must-not-exist");
    let runner = oz_steward::goal::gate::GateRunner::new(dir.path());
    let outcome = runner
        .run_gate(&GoalGate::StrongCommand {
            command: format!("cargo test --quiet; touch {}", marker.display()),
            timeout_secs: 5,
        })
        .await;
    assert!(!outcome.passed);
    assert!(outcome.pending);
    assert!(
        outcome.output.contains("single command"),
        "{}",
        outcome.output
    );
    assert!(!marker.exists(), "chained command must not execute");
}
