//! Tauri IPC command handlers for Side Panel operations.

use std::sync::Arc;
use tauri::{AppHandle, Emitter, Manager, State};

use crate::lock_poison_guard;
use crate::AppState;

use super::state::ArtifactInfo;
use super::terminal;

/// Toggle side panel visibility. Returns new state.
#[tauri::command]
pub fn toggle_sidepanel(
    app: AppHandle,
    state: State<'_, Arc<AppState>>,
) -> Result<bool, String> {
    let mut sp = lock_poison_guard(&state.sidepanel);
    sp.visible = !sp.visible;
    eprintln!("[sidepanel::commands] toggle_sidepanel: visible={}", sp.visible);
    app.emit("sidepanel:toggle", sp.visible)
        .map_err(|e| e.to_string())?;
    Ok(sp.visible)
}

/// Set side panel pixel width (clamped to 280..800).
#[tauri::command]
pub fn set_sidepanel_width(
    app: AppHandle,
    state: State<'_, Arc<AppState>>,
    width: u32,
) -> Result<(), String> {
    let mut sp = lock_poison_guard(&state.sidepanel);
    sp.width = width.clamp(280, 800);
    app.emit("sidepanel:width-changed", sp.width)
        .map_err(|e| e.to_string())?;
    Ok(())
}

/// Open an artifact in the side panel. Auto-expands the panel.
/// Path is validated to be within the working directory.
#[tauri::command]
pub fn open_artifact(
    app: AppHandle,
    state: State<'_, Arc<AppState>>,
    artifact_type: String,
    artifact_path: String,
    artifact_label: Option<String>,
) -> Result<serde_json::Value, String> {
    eprintln!("[sidepanel::commands] open_artifact called: type={}, path={}, label={:?}",
        artifact_type, artifact_path, artifact_label);
    let mut sp = lock_poison_guard(&state.sidepanel);

    // For non-terminal artifacts, verify the path exists and resolve to absolute.
    // Working-directory restriction removed: the user explicitly picks files via
    // a native file dialog, and the agent's open_side_panel tool validates paths
    // at the tool level before calling this command.
    let resolved_path = if artifact_type != "terminal" {
        let p = std::fs::canonicalize(&artifact_path)
            .map_err(|e| format!("Cannot open file: {e}"))?;
        if artifact_type == "html" {
            if let Some(parent) = p.parent() {
                if let Err(e) = app
                    .state::<tauri::scope::Scopes>()
                    .allow_directory(parent, true)
                {
                    eprintln!("[sidepanel] allow_directory failed: {e}");
                }
                // Whitelist for the `ozfile://` custom scheme (real-slash URL
                // serving for relative resources). Replace, not append: the
                // latest artifact's directory is the only one being displayed.
                let canonical_parent = parent.to_path_buf();
                let mut roots = state
                    .html_roots
                    .lock()
                    .unwrap_or_else(|e| e.into_inner());
                roots.clear();
                roots.push(canonical_parent.clone());
                eprintln!(
                    "[sidepanel::commands] ozfile root: {}",
                    canonical_parent.display()
                );
            }
        }
        p.to_string_lossy().to_string()
    } else {
        artifact_path.clone()
    };

    // Resolve label from file name from the resolved path
    let label = artifact_label.unwrap_or_else(|| {
        std::path::Path::new(&resolved_path)
            .file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_else(|| "unnamed".into())
    });

    let artifact = ArtifactInfo {
        id: uuid::Uuid::new_v4().to_string(),
        artifact_type,
        path: resolved_path,
        label,
    };

    sp.artifacts.push(artifact.clone());
    sp.active_id = Some(artifact.id.clone());
    sp.visible = true;

    let payload = serde_json::to_value(&artifact).map_err(|e| e.to_string())?;
    eprintln!("[sidepanel::commands] open_artifact emit payload: {}", payload);
    app.emit("sidepanel:artifact-opened", payload.clone())
        .map_err(|e| e.to_string())?;
    Ok(payload)
}

/// Close the side panel.
#[tauri::command]
pub fn close_sidepanel(
    app: AppHandle,
    state: State<'_, Arc<AppState>>,
) -> Result<(), String> {
    let mut sp = lock_poison_guard(&state.sidepanel);
    sp.visible = false;
    app.emit("sidepanel:toggle", false)
        .map_err(|e| e.to_string())?;
    Ok(())
}

/// Get current side panel state (for frontend initialization).
#[tauri::command]
pub fn get_sidepanel_state(
    state: State<'_, Arc<AppState>>,
) -> Result<serde_json::Value, String> {
    let sp = lock_poison_guard(&state.sidepanel);
    let current = serde_json::json!({
        "visible": sp.visible,
        "width": sp.width,
        "artifacts": sp.artifacts.iter().map(|a| serde_json::json!({
            "id": a.id,
            "type": a.artifact_type,
            "path": a.path,
            "label": a.label,
        })).collect::<Vec<_>>(),
        "active_id": sp.active_id,
    });
    Ok(current)
}

/// Close a specific tab by artifact id.
#[tauri::command]
pub fn close_artifact_tab(
    app: AppHandle,
    state: State<'_, Arc<AppState>>,
    artifact_id: String,
) -> Result<(), String> {
    let mut sp = lock_poison_guard(&state.sidepanel);
    if let Some(idx) = sp.artifacts.iter().position(|a| a.id == artifact_id) {
        sp.remove_tab(idx);
    }
    let payload = serde_json::json!({
        "artifacts": sp.artifacts.iter().map(|a| serde_json::json!({
            "id": a.id, "type": a.artifact_type, "path": a.path, "label": a.label,
        })).collect::<Vec<_>>(),
        "active_id": sp.active_id,
    });
    app.emit("sidepanel:artifacts-changed", payload)
        .map_err(|e| e.to_string())?;
    Ok(())
}

/// Switch to a specific tab by artifact id.
#[tauri::command]
pub fn switch_artifact_tab(
    app: AppHandle,
    state: State<'_, Arc<AppState>>,
    artifact_id: String,
) -> Result<(), String> {
    let mut sp = lock_poison_guard(&state.sidepanel);
    if sp.artifacts.iter().any(|a| a.id == artifact_id) {
        sp.active_id = Some(artifact_id.clone());
        app.emit("sidepanel:tab-switched", &artifact_id)
            .map_err(|e| e.to_string())?;
    }
    Ok(())
}

/// Clear all artifacts (e.g., on session switch).
#[tauri::command]
pub fn clear_sidepanel_artifacts(
    app: AppHandle,
    state: State<'_, Arc<AppState>>,
) -> Result<(), String> {
    let mut sp = lock_poison_guard(&state.sidepanel);
    sp.clear();
    sp.visible = false;
    app.emit("sidepanel:cleared", ())
        .map_err(|e| e.to_string())?;
    Ok(())
}

// ─── Terminal commands (Phase 2) ──────────────────────────────────────────

/// Spawn a new terminal session.
#[tauri::command]
pub async fn spawn_terminal(
    app: AppHandle,
    state: State<'_, Arc<AppState>>,
    shell: Option<String>,
    cwd: Option<String>,
) -> Result<String, String> {
    eprintln!("[sidepanel::commands] spawn_terminal COMMAND called: shell={:?} cwd={:?}", shell, cwd);
    let session_id = uuid::Uuid::new_v4().to_string();
    let id = session_id.clone();
    let registry = state.terminal_registry.clone();
    let app_clone = app.clone();

    tokio::task::spawn_blocking(move || {
        terminal::spawn_terminal(app_clone, registry, id, shell, cwd)
    })
    .await
    .map_err(|e| e.to_string())?
}

/// Write data to a terminal session.
#[tauri::command]
pub async fn write_to_terminal(
    state: State<'_, Arc<AppState>>,
    session_id: String,
    data: String,
) -> Result<(), String> {
    let registry = state.terminal_registry.clone();
    let sid = session_id.clone();
    let bytes = data.into_bytes();
    tokio::task::spawn_blocking(move || {
        terminal::write_to_terminal(registry, &sid, &bytes)
    })
    .await
    .map_err(|e| e.to_string())?
}

/// Resize a terminal session.
#[tauri::command]
pub async fn resize_terminal(
    state: State<'_, Arc<AppState>>,
    session_id: String,
    cols: u16,
    rows: u16,
) -> Result<(), String> {
    let registry = state.terminal_registry.clone();
    let sid = session_id.clone();
    tokio::task::spawn_blocking(move || {
        terminal::resize_terminal(registry, &sid, cols, rows)
    })
    .await
    .map_err(|e| e.to_string())?
}

/// Close a terminal session.
#[tauri::command]
pub async fn close_terminal(
    state: State<'_, Arc<AppState>>,
    session_id: String,
) -> Result<(), String> {
    let registry = state.terminal_registry.clone();
    let sid = session_id.clone();
    tokio::task::spawn_blocking(move || {
        terminal::close_terminal(registry, &sid)
    })
    .await
    .map_err(|e| e.to_string())?
}

// ─── File content commands (Phase 3) ──────────────────────────────────────

/// Read a file's binary content as a byte array (for images).
#[tauri::command]
pub fn read_file_bytes(
    path: String,
) -> Result<Vec<u8>, String> {
    let resolved = std::fs::canonicalize(&path)
        .map_err(|e| format!("Cannot resolve path: {e}"))?;
    // Same relaxed boundary as open_artifact: files are explicitly opened by
    // the user via native dialog, or validated by the agent's open_side_panel
    // tool at the tool level.
    std::fs::read(&resolved)
        .map_err(|e| format!("Cannot read file: {e}"))
}

/// Read a text file's content for code view.
#[tauri::command]
pub fn read_file_content(
    _state: State<'_, Arc<AppState>>,
    path: String,
) -> Result<String, String> {
    eprintln!("[sidepanel::commands] read_file_content called: path={}", path);
    let resolved = std::fs::canonicalize(&path)
        .map_err(|e| format!("Cannot resolve path: {e}"))?;
    // SidePanel files are explicitly opened by the user via native dialog;
    // same relaxed boundary as open_artifact.
    std::fs::read_to_string(&resolved)
        .map_err(|e| format!("Cannot read file: {e}"))
}

/// Parse an Excel (.xlsx/.xls/.xlsb) or CSV/TSV file into a 2D string array.
#[tauri::command]
pub fn parse_excel(
    path: String,
) -> Result<Vec<Vec<String>>, String> {
    use calamine::{open_workbook_auto, Reader};
    let resolved = std::fs::canonicalize(&path)
        .map_err(|e| format!("Cannot resolve path: {e}"))?;
    // Same relaxed boundary as open_artifact: files are explicitly opened by
    // the user via native dialog, or validated by the agent's open_side_panel
    // tool at the tool level.

    let ext = resolved.extension()
        .and_then(|e| e.to_str())
        .unwrap_or("")
        .to_lowercase();

    let mut rows: Vec<Vec<String>> = Vec::new();

    if ext == "csv" || ext == "tsv" {
        let content = std::fs::read_to_string(&resolved)
            .map_err(|e| format!("Cannot read CSV: {e}"))?;
        let sep = if ext == "tsv" { '\t' } else { ',' };
        for line in content.lines() {
            if line.trim().is_empty() { continue; }
            rows.push(line.split(sep).map(|c| c.trim().trim_matches('"').to_string()).collect());
        }
        return Ok(rows);
    }

    // `open_workbook_auto` picks the correct reader by file extension, so
    // legacy .xls (BIFF8) is supported in addition to .xlsx/.xlsb.
    let mut wb = open_workbook_auto(&resolved)
        .map_err(|e| format!("Cannot open workbook: {e}"))?;
    let sheet_names = wb.sheet_names().to_vec();
    if sheet_names.is_empty() {
        return Err("No sheets found".into());
    }
    let range = wb.worksheet_range(&sheet_names[0])
        .map_err(|e| format!("Cannot read sheet: {e}"))?;

    for r in range.rows() {
        let row: Vec<String> = r.iter().map(|cell| cell.to_string()).collect();
        rows.push(row);
    }

    Ok(rows)
}

// ─── Git diff command (Phase 4) ──────────────────────────────────────────

/// Run `git diff` for a specific file and return the output.
#[tauri::command]
pub fn get_git_diff(
    state: State<'_, Arc<AppState>>,
    path: String,
) -> Result<String, String> {
    let resolved = std::fs::canonicalize(&path)
        .map_err(|e| format!("Cannot resolve path: {e}"))?;
    // Same relaxed boundary as open_artifact (user dialog / agent tool layer
    // validation); workdir is used only as the git invocation cwd.
    let workdir = std::fs::canonicalize(&state.working_dir)
        .unwrap_or_else(|_| std::path::PathBuf::from(&state.working_dir));

    let output = std::process::Command::new("git")
        .args(["diff", "--", &resolved.to_string_lossy()])
        .current_dir(&workdir)
        .output()
        .map_err(|e| format!("git command failed: {e}"))?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        if stderr.contains("not a git repository") {
            return Err("Not a git repository. Use `git init` to initialize one.".into());
        }
        return Err(format!("git diff failed: {stderr}"));
    }

    let diff = String::from_utf8_lossy(&output.stdout).to_string();
    if diff.is_empty() {
        return Ok("No changes detected.".into());
    }
    Ok(diff)
}

/// Get basic file metadata for display in the side panel.
#[tauri::command]
pub fn get_file_info(path: String) -> Result<serde_json::Value, String> {
    // Same relaxed boundary as open_artifact (user dialog / agent tool layer).
    let resolved = std::fs::canonicalize(&path)
        .map_err(|e| e.to_string())?;
    let meta = std::fs::metadata(&resolved).map_err(|e| e.to_string())?;
    let modified = meta.modified()
        .map(|t| {
            let d = t.duration_since(std::time::UNIX_EPOCH).unwrap_or_default();
            chrono::DateTime::from_timestamp(d.as_secs() as i64, 0)
                .map(|dt| dt.to_rfc3339())
                .unwrap_or_default()
        })
        .unwrap_or_default();
    Ok(serde_json::json!({
        "size": meta.len(),
        "modified": modified,
    }))
}

/// Open a file with the system default application.
#[tauri::command]
pub fn open_external_file(path: String) -> Result<(), String> {
    // Same relaxed boundary as open_artifact (user dialog / agent tool layer).
    let resolved = std::fs::canonicalize(&path)
        .map_err(|e| format!("Cannot resolve path: {e}"))?;
    #[cfg(target_os = "macos")]
    {
        std::process::Command::new("open")
            .arg(&resolved)
            .spawn()
            .map_err(|e| e.to_string())?;
    }
    #[cfg(target_os = "linux")]
    {
        std::process::Command::new("xdg-open")
            .arg(&resolved)
            .spawn()
            .map_err(|e| e.to_string())?;
    }
    #[cfg(target_os = "windows")]
    {
        std::process::Command::new("cmd")
            .args(["/c", "start", "", &resolved])
            .spawn()
            .map_err(|e| e.to_string())?;
    }
    Ok(())
}
