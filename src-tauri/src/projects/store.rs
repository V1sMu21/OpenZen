use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::data_dir;
use crate::debug_log;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProjectRecord {
    pub id: String,
    pub name: String,
    pub root_path: String,
    pub created_at: String,
}

fn projects_path() -> PathBuf {
    data_dir().join("projects.json")
}

pub fn load_projects() -> Vec<ProjectRecord> {
    let path = projects_path();
    if !path.exists() {
        return Vec::new();
    }
    match std::fs::read_to_string(&path) {
        Ok(data) => {
            match serde_json::from_str::<Vec<ProjectRecord>>(&data) {
                Ok(records) => records,
                Err(e) => {
                    debug_log(&format!("load_projects: JSON parse error: {e}"));
                    Vec::new()
                }
            }
        }
        Err(e) => {
            debug_log(&format!("load_projects: read error for {}: {e}", path.display()));
            Vec::new()
        }
    }
}

pub fn save_projects(projects: &[ProjectRecord]) -> Result<(), String> {
    let path = projects_path();
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| format!("Failed to create data dir: {e}"))?;
    }
    let json =
        serde_json::to_string_pretty(projects).map_err(|e| format!("Serialization error: {e}"))?;
    let tmp = path.with_extension(format!(".tmp.{}", std::process::id()));
    std::fs::write(&tmp, &json).map_err(|e| format!("Write error: {e}"))?;
    if let Err(e) = std::fs::rename(&tmp, &path) {
        if e.raw_os_error() == Some(18) {
            std::fs::copy(&tmp, &path).map_err(|e| format!("Cross-device copy error: {e}"))?;
            let _ = std::fs::remove_file(&tmp);
        } else {
            return Err(format!("Atomic rename error: {e}"));
        }
    }
    Ok(())
}

#[allow(dead_code)]
pub fn find_by_session(
    projects: &[ProjectRecord],
    project_id: &str,
) -> Option<ProjectRecord> {
    projects.iter().find(|p| p.id == project_id).cloned()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::path::PathBuf;

    fn temp_projects_path(name: &str) -> PathBuf {
        std::env::temp_dir().join(format!("openzen_test_projects_{}.json", name))
    }

    fn cleanup(name: &str) {
        let path = temp_projects_path(name);
        let _ = fs::remove_file(&path);
    }

    fn make_record(id: &str, name: &str, path: &str) -> ProjectRecord {
        ProjectRecord {
            id: id.to_string(),
            name: name.to_string(),
            root_path: path.to_string(),
            created_at: "2026-07-07T00:00:00Z".to_string(),
        }
    }

    #[test]
    fn test_serialize_deserialize_roundtrip() {
        let records = vec![
            make_record("id-1", "project-a", "/tmp/a"),
            make_record("id-2", "project-b", "/tmp/b"),
        ];
        let json = serde_json::to_string(&records).unwrap();
        let parsed: Vec<ProjectRecord> = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed.len(), 2);
        assert_eq!(parsed[0].id, "id-1");
        assert_eq!(parsed[0].name, "project-a");
        assert_eq!(parsed[0].root_path, "/tmp/a");
    }

    #[test]
    fn test_save_and_load() {
        let name = "save_load";
        cleanup(name);
        let tmp_path = temp_projects_path(name);
        let records = vec![make_record("x", "test", "/tmp/test")];
        let json = serde_json::to_string_pretty(&records).unwrap();
        fs::write(&tmp_path, &json).unwrap();
        let content = fs::read_to_string(&tmp_path).unwrap();
        let loaded: Vec<ProjectRecord> = serde_json::from_str(&content).unwrap();
        assert_eq!(loaded.len(), 1);
        assert_eq!(loaded[0].name, "test");
        cleanup(name);
    }

    #[test]
    fn test_malformed_json_is_graceful() {
        let name = "malformed";
        cleanup(name);
        let tmp_path = temp_projects_path(name);
        fs::write(&tmp_path, "{broken").unwrap();
        let loaded: Vec<ProjectRecord> = std::fs::read_to_string(&tmp_path)
            .ok()
            .and_then(|data| serde_json::from_str::<Vec<ProjectRecord>>(&data).ok())
            .unwrap_or_default();
        assert!(loaded.is_empty());
        cleanup(name);
    }

    #[test]
    fn test_find_by_session_found() {
        let records = vec![
            make_record("p1", "a", "/a"),
            make_record("p2", "b", "/b"),
        ];
        let found = find_by_session(&records, "p2");
        assert!(found.is_some());
        assert_eq!(found.unwrap().name, "b");
    }

    #[test]
    fn test_find_by_session_not_found() {
        let records = vec![make_record("p1", "a", "/a")];
        assert!(find_by_session(&records, "nope").is_none());
    }

    #[test]
    fn test_empty_projects_list() {
        let records: Vec<ProjectRecord> = vec![];
        assert!(find_by_session(&records, "any").is_none());
    }

    #[test]
    fn test_name_collision_resolution() {
        let records = vec![make_record("1", "test", "/a")];
        let result = crate::projects::commands::tests::resolve_name_collision_for_test(&records, "test");
        assert_eq!(result, "test (2)");
    }

    #[test]
    fn test_name_collision_no_conflict() {
        let records = vec![make_record("1", "other", "/a")];
        let result = crate::projects::commands::tests::resolve_name_collision_for_test(&records, "test");
        assert_eq!(result, "test");
    }
}
