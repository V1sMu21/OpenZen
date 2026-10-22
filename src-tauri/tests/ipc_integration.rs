// src-tauri/tests/ipc_integration.rs
//
// IPC 命令集成测试（TAU-IPC / TAU-PROJ 测试组）
//
// 运行: cargo test -p openzen-tauri --test ipc_integration
//
// ── 这个文件在 2026-10-21 被整体重写（round5 P1-13③）────────────────────
// 旧版本 21 个测试**全是同义反复**：测试内部自建 `fn ping`、把字面量塞进
// `json!` 再断言同一字面量、`assert_eq!("/tmp/a", "/tmp/a")`、
// `assert!(!false)`。它们从不触碰生产代码，所以 21 个绿灯零信息量 ——
// 生产代码怎么改它们都绿。
//
// 现在每个测试都调用**真实实现**：
//   * `openzen_tauri_lib::projects::store` —— 项目记录的落盘/读回/容错；
//   * `oz_server::webui::sessions::SessionStore` —— 桌面端 IPC 命令
//     （create/list/rename/delete/move_to_project）真正的下游实现；
//   * 真实的 serde 序列化契约（前端读取的字段名）。
// 无法改真的测试一律删除，清单见本文件末尾注释。

use std::sync::Mutex;

/// 触碰 `data_dir()` 的测试必须串行：`save_projects` 写的是整份
/// `projects.json`，并行跑会互相覆盖（旧版测试用进程内 Vec 回避了这一点，
/// 代价是永远测不到真实落盘）。
static DATA_DIR_LOCK: Mutex<()> = Mutex::new(());

/// 把数据根指向一个隔离的临时目录。`OPENZEN_DATA_DIR` 是进程级变量，
/// 因此所有用到它的测试都在 `DATA_DIR_LOCK` 内运行。
fn isolated_data_dir(tag: &str) -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!("openzen-ipc-it-{}-{}", std::process::id(), tag));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("create isolated data dir");
    // SAFETY(env): 只在持有 DATA_DIR_LOCK 时调用。
    std::env::set_var("OPENZEN_DATA_DIR", &dir);
    dir
}

// ───────────────────────── TAU-PROJ: 项目记录 ─────────────────────────

/// TAU-PROJ-02/03 的真实版本：写入 → 从磁盘读回 → 四个字段都在。
/// 旧版只构造了一个本地 struct 然后断言它的字段。
#[test]
fn project_records_round_trip_through_the_real_store() {
    let _guard = DATA_DIR_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let dir = isolated_data_dir("proj-roundtrip");

    let records = vec![
        openzen_tauri_lib::projects::store::ProjectRecord {
            id: "proj-1".into(),
            name: "test-project".into(),
            root_path: "/tmp/test-project".into(),
            created_at: "2026-07-07T00:00:00Z".into(),
        },
        openzen_tauri_lib::projects::store::ProjectRecord {
            id: "proj-2".into(),
            name: "another".into(),
            root_path: "/tmp/another".into(),
            created_at: "2026-07-08T00:00:00Z".into(),
        },
    ];
    openzen_tauri_lib::projects::store::save_projects(&records).expect("save must succeed");
    assert!(
        dir.join("projects.json").exists(),
        "save_projects must actually write <data_dir>/projects.json"
    );

    let loaded = openzen_tauri_lib::projects::store::load_projects();
    assert_eq!(loaded.len(), 2, "both records must come back");
    assert_eq!(loaded[0].id, "proj-1");
    assert_eq!(loaded[0].name, "test-project");
    assert_eq!(loaded[0].root_path, "/tmp/test-project");
    assert_eq!(
        loaded[0].created_at, "2026-07-07T00:00:00Z",
        "created_at must survive the round-trip (旧版只断言了 name)"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

/// TAU-PROJ-01 的真实版本：没有 projects.json 时返回空表，而不是报错/panic。
#[test]
fn project_store_returns_empty_when_file_is_absent() {
    let _guard = DATA_DIR_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let dir = isolated_data_dir("proj-absent");
    assert!(
        !dir.join("projects.json").exists(),
        "precondition: no projects.json"
    );
    let loaded = openzen_tauri_lib::projects::store::load_projects();
    assert!(
        loaded.is_empty(),
        "missing file must degrade to an empty list"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

/// 容错路径：projects.json 被写坏时必须降级为空表（`load_projects` 的真实
/// 分支），而不是把损坏数据当成功返回。旧版没有任何等价断言。
#[test]
fn project_store_survives_malformed_json() {
    let _guard = DATA_DIR_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let dir = isolated_data_dir("proj-malformed");
    std::fs::write(dir.join("projects.json"), "{not json at all").unwrap();
    let loaded = openzen_tauri_lib::projects::store::load_projects();
    assert!(
        loaded.is_empty(),
        "malformed JSON must degrade to empty, got {loaded:?}"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

/// TAU-PROJ-05/14 的真实版本：按 id 查项目 —— 命中返回记录，未命中返回 None。
/// 旧版是 `assert_eq!("/tmp/a", "/tmp/a")`。
#[test]
fn find_by_session_matches_only_the_requested_project() {
    use openzen_tauri_lib::projects::store::{find_by_session, ProjectRecord};
    let records = vec![
        ProjectRecord {
            id: "p1".into(),
            name: "a".into(),
            root_path: "/a".into(),
            created_at: "".into(),
        },
        ProjectRecord {
            id: "p2".into(),
            name: "b".into(),
            root_path: "/b".into(),
            created_at: "".into(),
        },
    ];
    let found = find_by_session(&records, "p2").expect("p2 exists");
    assert_eq!(found.name, "b");
    assert_eq!(found.root_path, "/b");
    assert!(
        find_by_session(&records, "nope").is_none(),
        "unknown project id must not resolve to another project"
    );
    assert!(find_by_session(&[], "any").is_none());
}

// ─────────────────── TAU-IPC: 会话存储（真实实现）───────────────────

fn session_store() -> oz_server::webui::sessions::SessionStore {
    oz_server::webui::sessions::SessionStore::new()
}

/// TAU-IPC-03/04 的真实版本：创建后 list 里必须有它，且字段一致。
#[test]
fn session_create_then_list_returns_the_same_session() {
    let mut store = session_store();
    store.create_with_id_project("ses-1", "Test Session", None, None, None);
    assert!(store.has_session("ses-1"));

    let list = store.list();
    assert_eq!(list.len(), 1);
    assert_eq!(list[0].id, "ses-1");
    assert_eq!(list[0].name, "Test Session");
    assert_eq!(list[0].status, "idle", "new sessions start idle");
    assert_eq!(list[0].message_count, 0);
}

/// TAU-IPC-06 的真实版本：rename 改的是 store 里的名字，且只改目标。
#[test]
fn session_rename_updates_only_the_target() {
    let mut store = session_store();
    store.create_with_id("ses-1", "Old");
    store.create_with_id("ses-2", "Other");

    assert!(
        store.rename("ses-1", "Renamed"),
        "rename must report success"
    );
    let list = store.list();
    let s1 = list.iter().find(|s| s.id == "ses-1").unwrap();
    let s2 = list.iter().find(|s| s.id == "ses-2").unwrap();
    assert_eq!(s1.name, "Renamed");
    assert_eq!(s2.name, "Other", "unrelated session must be untouched");

    assert!(
        !store.rename("missing", "x"),
        "renaming an unknown session must report failure"
    );
}

/// TAU-IPC-07 的真实版本：delete 之后 has_session 为假、list 里消失，
/// 且不影响其它会话。
#[test]
fn session_delete_removes_exactly_one_session() {
    let mut store = session_store();
    store.create_with_id("ses-1", "one");
    store.create_with_id("ses-2", "two");

    assert!(store.delete("ses-1"));
    assert!(!store.has_session("ses-1"));
    assert!(store.has_session("ses-2"), "the other session must survive");
    let ids: Vec<String> = store.list().into_iter().map(|s| s.id).collect();
    assert_eq!(ids, vec!["ses-2".to_string()]);
    assert!(!store.delete("ses-1"), "deleting twice must report failure");
}

/// 持久化：`persisted(path)` + `save()` 真的写盘，新实例能读回。
/// 旧版没有任何落盘断言（它连 store 都没有）。
#[test]
fn sessions_persist_to_disk_and_reload() {
    let dir = std::env::temp_dir().join(format!("openzen-ipc-persist-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("sessions.json");

    {
        let mut store = oz_server::webui::sessions::SessionStore::persisted(path.clone());
        store.create_with_id_project(
            "ses-p",
            "Persisted",
            Some("proj-1"),
            None,
            Some("/tmp/proj"),
        );
        store.save();
        store.wait_persisted(std::time::Duration::from_secs(5));
    }

    let raw = std::fs::read_to_string(&path).expect("sessions.json must exist");
    let parsed: serde_json::Value = serde_json::from_str(&raw).expect("must be valid JSON");
    assert_eq!(parsed["ses-p"]["info"]["name"], "Persisted");

    let mut reopened = oz_server::webui::sessions::SessionStore::persisted(path.clone());
    reopened.reload();
    assert!(
        reopened.has_session("ses-p"),
        "reload must restore the session"
    );
    let info = &reopened.list()[0];
    assert_eq!(info.project_id.as_deref(), Some("proj-1"));
    assert_eq!(info.working_dir.as_deref(), Some("/tmp/proj"));
    let _ = std::fs::remove_dir_all(&dir);
}

/// TAU-PROJ-12 的真实版本：move_to_project 同时写入 project_id 与
/// working_dir，并且只影响目标会话。
#[test]
fn session_move_to_project_sets_project_and_working_dir() {
    let mut store = session_store();
    store.create_with_id("ses-1", "one");
    store.create_with_id("ses-2", "two");

    assert!(store.move_to_project("ses-1", "proj-target", "/tmp/target"));
    let list = store.list();
    let moved = list.iter().find(|s| s.id == "ses-1").unwrap();
    assert_eq!(moved.project_id.as_deref(), Some("proj-target"));
    assert_eq!(moved.working_dir.as_deref(), Some("/tmp/target"));
    let untouched = list.iter().find(|s| s.id == "ses-2").unwrap();
    assert_eq!(untouched.project_id, None, "other sessions must not move");
    assert_eq!(store.count_by_project_id("proj-target"), 1);
}

/// TAU-PROJ-15 的真实版本：按项目过滤会话数量。
#[test]
fn list_sessions_filters_by_project_id() {
    let mut store = session_store();
    store.create_with_id_project("a1", "a1", Some("proj-A"), None, None);
    store.create_with_id_project("a2", "a2", Some("proj-A"), None, None);
    store.create_with_id_project("b1", "b1", Some("proj-B"), None, None);
    store.create_with_id("none", "none");

    assert_eq!(store.count_by_project_id("proj-A"), 2);
    assert_eq!(store.count_by_project_id("proj-B"), 1);
    assert_eq!(store.count_by_project_id("proj-missing"), 0);
}

/// 删除项目时清空其会话归属（`clear_project_sessions` 的真实语义）。
#[test]
fn clearing_a_project_only_detaches_its_own_sessions() {
    let mut store = session_store();
    store.create_with_id_project("a1", "a1", Some("proj-A"), None, None);
    store.create_with_id_project("b1", "b1", Some("proj-B"), None, None);

    store.clear_project_sessions("proj-A");
    assert_eq!(store.count_by_project_id("proj-A"), 0);
    assert_eq!(
        store.count_by_project_id("proj-B"),
        1,
        "clearing proj-A must not touch proj-B"
    );
    assert!(store.has_session("a1"), "the session itself must survive");
}

/// 会话上限：`evict()` 是真实实现，但**只在 with_max / set_max_sessions
/// 时触发**（不是每次 create）—— 这条断言钉住这个真实语义，免得有人以为
/// 上限是"写入即生效"而在别处依赖它。
#[test]
fn session_store_evicts_down_to_its_cap_when_the_cap_is_set() {
    let mut store = oz_server::webui::sessions::SessionStore::new();
    for i in 0..5 {
        store.create_with_id(&format!("s{i}"), &format!("s{i}"));
    }
    assert_eq!(store.list().len(), 5, "without a cap nothing is evicted");

    store.set_max_sessions(3);
    let ids: Vec<String> = store.list().into_iter().map(|s| s.id).collect();
    assert_eq!(
        ids.len(),
        3,
        "cap must hold after set_max_sessions: {ids:?}"
    );
    assert!(
        store.has_session("s4") && store.has_session("s3"),
        "newest sessions must survive: {ids:?}"
    );
}

// ───────────────────── 序列化契约（前端读这些键）─────────────────────

/// 前端（`frontends/src/lib/stores/sessions.ts`）直接读这些键名；
/// 改了字段名而没改前端会静默变成 undefined。旧版断言的是本地 `json!`。
#[test]
fn session_info_json_exposes_the_keys_the_frontend_reads() {
    let mut store = session_store();
    store.create_with_id_project("ses-1", "Contract", Some("proj-1"), None, Some("/tmp/wd"));
    let info = store.list().into_iter().next().unwrap();

    let v = serde_json::to_value(&info).expect("SessionInfo must serialize");
    for key in [
        "id",
        "name",
        "created_at",
        "status",
        "message_count",
        "project_id",
        "project_name",
        "working_dir",
    ] {
        assert!(
            v.get(key).is_some(),
            "SessionInfo lost the `{key}` field the frontend reads: {v}"
        );
    }
    assert_eq!(v["id"], "ses-1");
    assert_eq!(v["working_dir"], "/tmp/wd");

    // 反向：必须能从 JSON 反序列化回来（IPC 是双向的）。
    let back: oz_server::webui::sessions::SessionInfo =
        serde_json::from_value(v.clone()).expect("SessionInfo must deserialize");
    assert_eq!(back.id, info.id);
    assert_eq!(back.message_count, info.message_count);
}

/// ProjectRecord 的序列化键名同样是前端/磁盘契约。
#[test]
fn project_record_json_exposes_the_keys_the_store_writes() {
    let record = openzen_tauri_lib::projects::store::ProjectRecord {
        id: "proj-1".into(),
        name: "test".into(),
        root_path: "/tmp/test".into(),
        created_at: "2026-07-07T00:00:00Z".into(),
    };
    let v = serde_json::to_value(&record).unwrap();
    for key in ["id", "name", "root_path", "created_at"] {
        assert!(v.get(key).is_some(), "ProjectRecord lost `{key}`: {v}");
    }
    let back: openzen_tauri_lib::projects::store::ProjectRecord =
        serde_json::from_value(v).expect("must deserialize");
    assert_eq!(back.root_path, "/tmp/test");
}

// ── 已删除的旧测试（21 个，全部为同义反复，无法改成真断言）──────────
// test_ipc_ping_roundtrip          自建 `fn ping` 并断言它自己 format 的字符串。
//                                  真实的 `commands::ping` 需要 `State<Arc<AppState>>`
//                                  （要起 Tauri 运行时）且 `commands` 是 pub(crate)，
//                                  集成测试无法调用 → 删除而非伪造。
// test_ipc_dashboard_stats_format  断言本地 `json!`；`commands` 不可见 → 删除。
// test_compress_stats_format       断言本地 `json!`；真实压缩统计由
//                                  `crates/oz-server` 的 webui 测试覆盖 → 删除。
// test_project_add_with_auto_name  构造本地 struct → 由上面的真实 round-trip 取代。
// test_project_add_custom_name     `Some("My Project").unwrap_or_else(..)` 断言自己。
// test_project_duplicate_detection `assert_eq!("/tmp/a", "/tmp/a")`。
// test_project_name_collision_*    测试内复制了一份 `resolve_name_collision`；
//                                  真实函数是私有 + `#[cfg(test)]` 模块，
//                                  集成测试不可见（crate 内单测已覆盖）→ 删除。
// test_project_rename              `.trim()` 后断言字面量。
// test_project_rename_empty_rejected 同上。
// test_project_remove              本地 Vec 的 remove。
// test_session_in_project          本地 `json!`。
// test_move_to_invalid_project     `let target_exists = false; assert!(!target_exists)`
//                                  —— 最纯粹的恒真断言；真实的项目存在性校验在
//                                  tauri command 里（需 AppState）→ 删除。
// test_concurrent_agent_limit      `let running = 3; assert!(running >= 3)`。
// test_same_session_mutex          本地 HashMap contains_key。
// test_session_info_serialization  本地 `json!` → 由真实 SessionInfo 契约测试取代。
// test_project_record_fields       本地 `json!` → 由真实 ProjectRecord 契约测试取代。
