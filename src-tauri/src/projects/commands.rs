use std::path::Path;
use std::sync::Arc;

use tauri::Emitter;
use tauri::State;

use super::store::{self, ProjectRecord};
use crate::{debug_log, lock_poison_guard, AppState};

#[tauri::command]
pub fn add_project(
    root_path: String,
    name: Option<String>,
    state: State<'_, Arc<AppState>>,
    app_handle: tauri::AppHandle,
) -> Result<ProjectRecord, String> {
    let canonical = Path::new(&root_path)
        .canonicalize()
        .map_err(|e| format!("Cannot access path: {e}"))?
        .to_string_lossy()
        .to_string();

    if !Path::new(&canonical).is_dir() {
        return Err("Path is not a directory".to_string());
    }

    let mut projects = lock_poison_guard(&state.projects);

    if projects.iter().any(|p| p.root_path == canonical) {
        return Err("Project already exists at this path".to_string());
    }

    let project_name = name.filter(|n| !n.trim().is_empty()).unwrap_or_else(|| {
        Path::new(&canonical)
            .file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_else(|| "Untitled".to_string())
    });

    let record = ProjectRecord {
        id: uuid::Uuid::new_v4().to_string(),
        name: resolve_name_collision(&projects, &project_name),
        root_path: canonical.clone(),
        created_at: chrono::Utc::now().to_rfc3339(),
    };

    projects.push(record.clone());
    let projects_snapshot = projects.clone();
    drop(projects);

    store::save_projects(&projects_snapshot)?;

    let _ = app_handle.emit(
        "project:added",
        serde_json::to_value(&record).unwrap_or_default(),
    );

    debug_log(&format!(
        "add_project: id={} name={} path={}",
        record.id, record.name, root_path
    ));
    Ok(record)
}

#[tauri::command]
pub fn list_projects(state: State<'_, Arc<AppState>>) -> Result<Vec<serde_json::Value>, String> {
    let projects = lock_poison_guard(&state.projects).clone();
    let sessions = lock_poison_guard(&state.sessions);

    let list: Vec<serde_json::Value> = projects
        .iter()
        .map(|p| {
            let session_count = sessions.count_by_project_id(&p.id);
            let broken = !Path::new(&p.root_path).is_dir();
            serde_json::json!({
                "id": p.id,
                "name": p.name,
                "root_path": p.root_path,
                "created_at": p.created_at,
                "session_count": session_count,
                "broken": broken,
            })
        })
        .collect();

    Ok(list)
}

#[tauri::command]
pub fn remove_project(
    project_id: String,
    state: State<'_, Arc<AppState>>,
    app_handle: tauri::AppHandle,
) -> Result<(), String> {
    let mut projects = lock_poison_guard(&state.projects);
    let idx = projects
        .iter()
        .position(|p| p.id == project_id)
        .ok_or_else(|| "Project not found".to_string())?;

    projects.remove(idx);
    let projects_snapshot = projects.clone();
    drop(projects);

    store::save_projects(&projects_snapshot)?;

    lock_poison_guard(&state.sessions).clear_project_sessions(&project_id);

    let _ = app_handle.emit(
        "project:removed",
        serde_json::json!({ "project_id": project_id }),
    );

    debug_log(&format!("remove_project: id={}", project_id));
    Ok(())
}

#[tauri::command]
pub fn rename_project(
    project_id: String,
    new_name: String,
    state: State<'_, Arc<AppState>>,
    app_handle: tauri::AppHandle,
) -> Result<(), String> {
    let new_name = new_name.trim().to_string();
    if new_name.is_empty() {
        return Err("Name cannot be empty".to_string());
    }

    let mut projects = lock_poison_guard(&state.projects);
    let project = projects
        .iter_mut()
        .find(|p| p.id == project_id)
        .ok_or_else(|| "Project not found".to_string())?;

    project.name = new_name.clone();
    let projects_snapshot = projects.clone();
    drop(projects);

    store::save_projects(&projects_snapshot)?;

    let _ = app_handle.emit(
        "project:renamed",
        serde_json::json!({ "project_id": project_id, "new_name": new_name }),
    );

    debug_log(&format!(
        "rename_project: id={} new_name={}",
        project_id, new_name
    ));
    Ok(())
}

/// Reveal a folder in the system file manager. Implemented in Rust so it
/// bypasses the shell plugin's JS-side open scope (which requires a
/// configured validation regex and would reject arbitrary local paths).
#[tauri::command]
pub fn reveal_in_finder(path: String) -> Result<(), String> {
    reveal_in_finder_impl(&path)
}

#[cfg(target_os = "macos")]
fn reveal_in_finder_impl(path: &str) -> Result<(), String> {
    let p = std::path::Path::new(path);
    let mut cmd = std::process::Command::new("open");
    if p.is_dir() {
        cmd.arg(path);
    } else {
        cmd.arg("-R").arg(path);
    }
    cmd.spawn()
        .map_err(|e| format!("Failed to reveal in Finder: {e}"))?;
    Ok(())
}

#[cfg(target_os = "windows")]
fn reveal_in_finder_impl(path: &str) -> Result<(), String> {
    std::process::Command::new("explorer")
        .arg(path)
        .spawn()
        .map_err(|e| format!("Failed to open in Explorer: {e}"))?;
    Ok(())
}

#[cfg(all(not(target_os = "macos"), not(target_os = "windows")))]
fn reveal_in_finder_impl(path: &str) -> Result<(), String> {
    std::process::Command::new("xdg-open")
        .arg(path)
        .spawn()
        .map_err(|e| format!("Failed to open file manager: {e}"))?;
    Ok(())
}

fn resolve_name_collision(existing: &[ProjectRecord], base_name: &str) -> String {
    let collides = existing.iter().any(|p| p.name == base_name);
    if !collides {
        return base_name.to_string();
    }
    for i in 2..100 {
        let candidate = format!("{} ({})", base_name, i);
        if !existing.iter().any(|p| p.name == candidate) {
            return candidate;
        }
    }
    format!("{} ({})", base_name, uuid::Uuid::new_v4())
}

#[cfg(test)]
pub mod tests {
    use super::*;

    pub fn resolve_name_collision_for_test(existing: &[ProjectRecord], base_name: &str) -> String {
        resolve_name_collision(existing, base_name)
    }

    #[test]
    fn test_resolve_name_collision_no_conflict() {
        let records = vec![ProjectRecord {
            id: "1".into(),
            name: "other".into(),
            root_path: "/other".into(),
            created_at: "".into(),
        }];
        assert_eq!(resolve_name_collision_for_test(&records, "test"), "test");
    }

    #[test]
    fn test_resolve_name_collision_first_collision() {
        let records = vec![ProjectRecord {
            id: "1".into(),
            name: "test".into(),
            root_path: "/a".into(),
            created_at: "".into(),
        }];
        assert_eq!(
            resolve_name_collision_for_test(&records, "test"),
            "test (2)"
        );
    }

    #[test]
    fn test_resolve_name_collision_multiple_collisions() {
        let mut records = vec![
            ProjectRecord {
                id: "1".into(),
                name: "test".into(),
                root_path: "/a".into(),
                created_at: "".into(),
            },
            ProjectRecord {
                id: "2".into(),
                name: "test (2)".into(),
                root_path: "/b".into(),
                created_at: "".into(),
            },
        ];
        assert_eq!(
            resolve_name_collision_for_test(&records, "test"),
            "test (3)"
        );

        records.push(ProjectRecord {
            id: "3".into(),
            name: "test (3)".into(),
            root_path: "/c".into(),
            created_at: "".into(),
        });
        assert_eq!(
            resolve_name_collision_for_test(&records, "test"),
            "test (4)"
        );
    }
}
