#[cfg(test)]
mod project_integration {
    use openzen_tauri_lib::projects::store::ProjectRecord;

    #[test]
    fn test_project_record_serialization_roundtrip() {
        let record = ProjectRecord {
            id: "test-id-1".to_string(),
            name: "test-project".to_string(),
            root_path: "/tmp/test-project".to_string(),
            created_at: "2026-07-07T00:00:00Z".to_string(),
        };
        let json = serde_json::to_string(&record).unwrap();
        let parsed: ProjectRecord = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed.id, "test-id-1");
        assert_eq!(parsed.name, "test-project");
        assert_eq!(parsed.root_path, "/tmp/test-project");
    }

    #[test]
    fn test_session_info_project_id_serialization() {
        let info = serde_json::json!({
            "id": "ses-1",
            "name": "Test Session",
            "created_at": "2026-07-07T00:00:00Z",
            "status": "idle",
            "message_count": 0,
            "project_id": "proj-1",
            "project_name": "my-project"
        });
        assert_eq!(info["project_id"].as_str().unwrap(), "proj-1");
        assert_eq!(info["project_name"].as_str().unwrap(), "my-project");
    }

    #[test]
    fn test_session_info_no_project_id_serialization() {
        let info = serde_json::json!({
            "id": "ses-1",
            "name": "Test Session",
            "created_at": "2026-07-07T00:00:00Z",
            "status": "idle",
            "message_count": 0
        });
        assert!(info.get("project_id").is_none());
    }

    #[test]
    fn test_projects_json_array_format() {
        let projects = vec![
            serde_json::json!({
                "id": "p1",
                "name": "project-one",
                "root_path": "/tmp/p1",
                "created_at": "2026-07-07T00:00:00Z",
                "session_count": 3,
                "broken": false
            }),
            serde_json::json!({
                "id": "p2",
                "name": "project-two",
                "root_path": "/tmp/p2",
                "created_at": "2026-07-07T00:00:00Z",
                "session_count": 0,
                "broken": true
            }),
        ];
        let json = serde_json::to_string(&projects).unwrap();
        let parsed: Vec<serde_json::Value> = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed.len(), 2);
        assert_eq!(parsed[0]["name"], "project-one");
        assert_eq!(parsed[0]["session_count"], 3);
        assert!(!parsed[0]["broken"].as_bool().unwrap());
        assert_eq!(parsed[1]["broken"].as_bool().unwrap(), true);
    }

    #[test]
    fn test_empty_projects_json() {
        let empty: Vec<serde_json::Value> = vec![];
        let json = serde_json::to_string(&empty).unwrap();
        assert_eq!(json, "[]");
    }
}
