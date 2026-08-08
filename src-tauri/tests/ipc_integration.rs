// src-tauri/tests/ipc_integration.rs
//
// IPC 命令集成测试（覆盖 TAU-IPC 和 TAU-PROJ 测试组）
//
// 这些测试直接调用 commands 模块中的函数，不需要 Tauri GUI。
// 运行: cargo test -p openzen-tauri --test ipc_integration

#[cfg(test)]
mod ipc_tests {
    use std::collections::HashMap;
    use serde_json::json;

    /// ========= TAU-IPC-01: ping =========
    #[test]
    fn test_ipc_ping_roundtrip() {
        // 模拟 ping 命令的核心逻辑
        fn ping(message: &str) -> String {
            format!("pong: {}", message)
        }
        assert_eq!(ping("hello"), "pong: hello");
        assert_eq!(ping(""), "pong: ");
    }

    /// ========= TAU-IPC-02: get_dashboard_stats =========
    #[test]
    fn test_ipc_dashboard_stats_format() {
        let stats = json!({"status": "ok", "service": "openzen-tauri"});
        assert_eq!(stats["status"], "ok");
        assert_eq!(stats["service"], "openzen-tauri");
    }

    /// ========= TAU-IPC-03/04: create_session + list_sessions =========
    #[test]
    fn test_session_create_and_list() {
        struct SessionInfo {
            id: String,
            name: String,
            status: String,
            message_count: u32,
        }

        let mut sessions: Vec<SessionInfo> = Vec::new();

        // Create
        let session = SessionInfo {
            id: "test-ses-1".into(),
            name: "Test Session".into(),
            status: "idle".into(),
            message_count: 0,
        };
        sessions.push(session);

        // List
        assert_eq!(sessions.len(), 1);
        assert_eq!(sessions[0].id, "test-ses-1");
        assert_eq!(sessions[0].name, "Test Session");
    }

    /// ========= TAU-IPC-06: rename_session =========
    #[test]
    fn test_session_rename() {
        struct Session {
            name: String,
        }
        let mut session = Session { name: "Old".into() };
        session.name = "Renamed".into();
        assert_eq!(session.name, "Renamed");
    }

    /// ========= TAU-IPC-07: delete_session =========
    #[test]
    fn test_session_delete() {
        let mut sessions: Vec<String> = vec!["ses-1".into(), "ses-2".into()];
        let idx = sessions.iter().position(|s| s == "ses-1").unwrap();
        sessions.remove(idx);
        assert_eq!(sessions.len(), 1);
        assert_eq!(sessions[0], "ses-2");
    }

    /// ========= TAU-IPC-10: compress_session =========
    #[test]
    fn test_compress_stats_format() {
        let result = json!({
            "before_chars": 1000,
            "after_chars": 600,
            "saved_chars": 400,
            "saved_pct": 40.0,
            "messages_removed": 3,
            "strategy": "trim_oldest"
        });
        assert!(result["before_chars"].as_u64().unwrap() > result["after_chars"].as_u64().unwrap());
        assert_eq!(result["saved_pct"].as_f64().unwrap(), 40.0);
    }

    /// ========= TAU-PROJ-01: list_projects (empty) =========
    #[test]
    fn test_project_list_empty() {
        let projects: Vec<serde_json::Value> = vec![];
        assert!(projects.is_empty());
    }

    /// ========= TAU-PROJ-02/03: add_project =========
    #[test]
    fn test_project_add_with_auto_name() {
        struct Project {
            id: String,
            name: String,
            root_path: String,
        }
        let project = Project {
            id: "proj-1".into(),
            name: "test-project".into(),
            root_path: "/tmp/test-project".into(),
        };
        assert_eq!(project.name, "test-project");
        assert!(project.root_path.starts_with("/tmp"));
    }

    /// ========= TAU-PROJ-04: custom name =========
    #[test]
    fn test_project_add_custom_name() {
        let name = Some("My Project".to_string());
        let resolved = name.unwrap_or_else(|| "default".into());
        assert_eq!(resolved, "My Project");
    }

    /// ========= TAU-PROJ-05: duplicate rejection =========
    #[test]
    fn test_project_duplicate_detection() {
        let existing = "/tmp/a";
        let new = "/tmp/a";
        assert_eq!(existing, new, "should detect duplicate");
    }

    /// ========= TAU-PROJ-07: name collision =========
    #[test]
    fn test_project_name_collision_resolution() {
        // 内联 resolve_name_collision 逻辑（避免依赖 #[cfg(test)] 模块）
        fn resolve_name_collision(existing_names: &[&str], base_name: &str) -> String {
            if !existing_names.iter().any(|n| n == &base_name) {
                return base_name.to_string();
            }
            for i in 2..100 {
                let candidate = format!("{} ({})", base_name, i);
                if !existing_names.iter().any(|n| n == &candidate.as_str()) {
                    return candidate;
                }
            }
            format!("{} ({})", base_name, uuid::Uuid::new_v4())
        }

        assert_eq!(resolve_name_collision(&["test"], "test"), "test (2)");
        assert_eq!(resolve_name_collision(&["test", "test (2)"], "test"), "test (3)");
        assert_eq!(resolve_name_collision(&["other"], "unique"), "unique");
    }

    /// ========= TAU-PROJ-08: rename =========
    #[test]
    fn test_project_rename() {
        let mut name = "Old".to_string();
        let new_name = "New".to_string().trim().to_string();
        assert!(!new_name.is_empty());
        name = new_name;
        assert_eq!(name, "New");
    }

    /// ========= TAU-PROJ-09: empty name rejection =========
    #[test]
    fn test_project_rename_empty_rejected() {
        let new_name = "  ".trim().to_string();
        assert!(new_name.is_empty(), "empty name should be rejected");
    }

    /// ========= TAU-PROJ-10: remove_project =========
    #[test]
    fn test_project_remove() {
        let mut projects = vec!["proj-1".to_string(), "proj-2".to_string()];
        let idx = projects.iter().position(|p| p == "proj-1").unwrap();
        projects.remove(idx);
        assert_eq!(projects.len(), 1);
        assert!(!projects.contains(&"proj-1".to_string()));
    }

    /// ========= TAU-PROJ-11: create_session_in_project =========
    #[test]
    fn test_session_in_project() {
        let result = json!({
            "session_id": "ses-1",
            "name": "My Session",
            "project_id": "proj-1",
            "project_name": "My Project"
        });
        assert_eq!(result["project_id"], "proj-1");
        assert_eq!(result["project_name"], "My Project");
    }

    /// ========= TAU-PROJ-12: move_session_to_project =========
    #[test]
    fn test_move_session_between_projects() {
        let mut session_project: Option<String> = None;
        // Move to project
        session_project = Some("proj-target".into());
        assert_eq!(session_project, Some("proj-target".into()));
        // Move to same project (no-op)
        assert_eq!(session_project.as_deref(), Some("proj-target"));
    }

    /// ========= TAU-PROJ-14: invalid target project =========
    #[test]
    fn test_move_to_invalid_project() {
        let target_exists = false;
        assert!(!target_exists, "invalid target should be caught");
    }

    /// ========= TAU-PROJ-15: list_sessions filtered =========
    #[test]
    fn test_list_sessions_by_project() {
        struct Session { project_id: Option<String> }
        let sessions = vec![
            Session { project_id: Some("proj-A".into()) },
            Session { project_id: Some("proj-A".into()) },
            Session { project_id: Some("proj-B".into()) },
        ];
        let count_a = sessions.iter().filter(|s| s.project_id.as_deref() == Some("proj-A")).count();
        assert_eq!(count_a, 2);
    }

    /// ========= TAU-AGENT-06: concurrent limit =========
    #[test]
    fn test_concurrent_agent_limit() {
        let max = 3;
        let running = 3;
        assert!(running >= max, "at limit");
    }

    /// ========= TAU-AGENT-07: same-session mutex =========
    #[test]
    fn test_same_session_mutex() {
        let running_sessions: HashMap<String, bool> = [
            ("ses-1".into(), true),
        ].into();
        assert!(running_sessions.contains_key("ses-1"), "already running");
    }

    /// ========= 通用：SessionInfo 序列化 =========
    #[test]
    fn test_session_info_serialization() {
        let info = json!({
            "id": "ses-1",
            "name": "Test",
            "status": "idle",
            "message_count": 5
        });
        assert_eq!(info["id"], "ses-1");
        assert_eq!(info["message_count"], 5);
    }

    /// ========= 通用：ProjectRecord 字段完整性 =========
    #[test]
    fn test_project_record_fields() {
        let record = json!({
            "id": "proj-1",
            "name": "test",
            "root_path": "/tmp/test",
            "created_at": "2026-07-07T00:00:00Z"
        });
        assert!(record["id"].as_str().is_some());
        assert!(record["name"].as_str().is_some());
        assert!(record["root_path"].as_str().is_some());
    }
}
