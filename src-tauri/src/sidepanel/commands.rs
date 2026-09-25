//! Tauri IPC command handlers for Side Panel operations.

use std::io::Read;
use std::sync::Arc;
use tauri::{AppHandle, Emitter, Manager, State};

use crate::lock_poison_guard;
use crate::AppState;

use super::state::{ArtifactInfo, SidePanelState};
use super::terminal;

/// Toggle side panel visibility. Returns new state.
#[tauri::command]
pub fn toggle_sidepanel(app: AppHandle, state: State<'_, Arc<AppState>>) -> Result<bool, String> {
    let mut sp = lock_poison_guard(&state.sidepanel);
    sp.visible = !sp.visible;
    tracing::info!(
        "[sidepanel::commands] toggle_sidepanel: visible={}",
        sp.visible
    );
    app.emit("sidepanel:toggle", sp.visible)
        .map_err(|e| e.to_string())?;
    Ok(sp.visible)
}

/// Set side panel pixel width (clamped to 280..960).
#[tauri::command]
pub fn set_sidepanel_width(
    app: AppHandle,
    state: State<'_, Arc<AppState>>,
    width: u32,
) -> Result<(), String> {
    let mut sp = lock_poison_guard(&state.sidepanel);
    sp.width = width.clamp(280, 960);
    app.emit("sidepanel:width-changed", sp.width)
        .map_err(|e| e.to_string())?;
    Ok(())
}

/// Detect the artifact viewer type from a file extension — mirrors the
/// frontend detectType map (SidePanel.svelte).
fn detect_artifact_type(path: &std::path::Path) -> String {
    let ext = path
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("")
        .to_lowercase();
    match ext.as_str() {
        "html" | "htm" => "html",
        "pdf" => "pdf",
        "xlsx" | "xls" | "csv" | "tsv" => "spreadsheet",
        "py" | "rs" | "ts" | "js" | "go" | "svelte" | "json" | "yaml" | "yml" | "toml" | "sql"
        | "sh" | "css" | "scss" | "txt" | "sty" | "cls" | "bib" => "code",
        "md" | "rtf" => "markdown",
        "tex" | "lt" => "latex",
        "png" | "jpg" | "jpeg" | "gif" | "svg" | "webp" => "image",
        "doc" | "docx" | "ppt" | "pptx" => "office",
        _ => "code",
    }
    .to_string()
}

/// Point the `ozfile` root at whatever the panel is showing right now: the
/// active artifact's directory when that artifact is html, nothing otherwise.
/// The scheme serves exactly one directory at a time (scheme.rs), so a tab
/// that becomes active again later — switched back to, or restored on a
/// session switch — would otherwise lose its sibling css/js/img.
///
/// Call with the `sidepanel` lock held (order: sidepanel → html_roots).
fn sync_html_root(state: &Arc<AppState>, sp: &SidePanelState) {
    let active_dir = sp
        .active_id
        .as_ref()
        .and_then(|id| sp.artifacts.iter().find(|a| &a.id == id))
        .filter(|a| a.artifact_type == "html")
        .and_then(|a| {
            std::path::Path::new(&a.path)
                .parent()
                .map(std::path::Path::to_path_buf)
        });
    let mut html_roots = lock_poison_guard(&state.html_roots);
    match active_dir {
        Some(dir) => {
            if html_roots.first() != Some(&dir) {
                tracing::info!("[sidepanel::commands] ozfile root: {}", dir.display());
                html_roots.clear();
                html_roots.push(dir);
            }
        }
        None => html_roots.clear(),
    }
}

/// Show `path` as the active artifact: focus the tab already displaying it, or
/// append a new one.
///
/// A path already on the tab bar is FOCUSED, not duplicated. The agent normally
/// opens its own deliverable (`open_side_panel`) before the user clicks 打开 on
/// the turn's changed-files card, and two tabs showing one file help nobody —
/// the second click would also stack a third. Reusing the id keeps the frontend
/// listener, the per-session parking and its `{#key}` in sync, since all key
/// on it (`openSeq` in frontends/src/lib/stores/sidepanel.svelte.ts forces the
/// viewer to re-read the file, which a same-id focus would otherwise skip).
///
/// Split out of `register_and_show` so this rule is testable without an
/// AppHandle.
fn focus_or_push(
    artifacts: &mut Vec<ArtifactInfo>,
    artifact_type: String,
    path: String,
    label: String,
) -> ArtifactInfo {
    // A terminal tab is a SESSION, not a file, and its path is the constant
    // "." — path identity would silently collapse every shell the user opens
    // into one. Each 终端 click must mint a fresh tab so the viewer remounts
    // and spawns a new PTY (ArtifactTerminal spawns in onMount only).
    let existing_idx = if artifact_type == "terminal" {
        None
    } else {
        artifacts.iter().position(|a| a.path == path)
    };
    match existing_idx {
        Some(idx) => {
            // Focus only: the tab keeps the viewer and title it was opened
            // with. Rewriting them would let a caller holding a coarser guess
            // — the extension map, or a bare file-name label — downgrade what
            // another caller deliberately declared (the agent can call a
            // `.json` a `spreadsheet`). The agent announces a deliverable
            // before the user can click 打开 on it, so the declared viewer is
            // already the one sitting on the tab; the caller's label/type only
            // decide what a NEW tab looks like.
            artifacts[idx].clone()
        }
        None => {
            let fresh = ArtifactInfo {
                id: uuid::Uuid::new_v4().to_string(),
                artifact_type,
                path,
                label,
            };
            artifacts.push(fresh.clone());
            fresh
        }
    }
}

/// Register a resolved path in the artifact whitelist (and the ozfile html
/// root when needed), push the artifact tab and emit the open event.
/// Shared by the agent-facing `open_artifact` and the user-dialog path.
fn register_and_show(
    app: &AppHandle,
    state: &Arc<AppState>,
    artifact_type: String,
    resolved_path: String,
    label: String,
) -> Result<serde_json::Value, String> {
    if artifact_type != "terminal" {
        // Register the file (and, for html artifacts, its parent dir for
        // relative resources) in the artifact whitelist so the read_file_*
        // commands can serve it.
        let p = std::path::PathBuf::from(&resolved_path);
        let mut roots = lock_poison_guard(&state.artifact_roots);
        if !roots.contains(&p) {
            roots.push(p.clone());
        }
        if artifact_type == "html" {
            if let Some(parent) = p.parent() {
                if let Err(e) = app
                    .state::<tauri::scope::Scopes>()
                    .allow_directory(parent, true)
                {
                    tracing::warn!("[sidepanel] allow_directory failed: {e}");
                }
                let canonical_parent = parent.to_path_buf();
                if !roots.contains(&canonical_parent) {
                    roots.push(canonical_parent);
                }
            }
        }
    }

    let mut sp = lock_poison_guard(&state.sidepanel);

    let artifact = focus_or_push(&mut sp.artifacts, artifact_type, resolved_path, label);
    sp.active_id = Some(artifact.id.clone());
    sp.visible = true;
    sync_html_root(state, &sp);

    let payload = serde_json::to_value(&artifact).map_err(|e| e.to_string())?;
    tracing::info!(
        "[sidepanel::commands] open_artifact emit payload: {}",
        payload
    );
    app.emit("sidepanel:artifact-opened", payload.clone())
        .map_err(|e| e.to_string())?;
    Ok(payload)
}

/// Open an artifact in the side panel. Auto-expands the panel.
/// Agent-facing channel: the path must resolve inside the app working dir
/// or a registered project root (P2-3) — the agent's open_side_panel tool
/// already validates this, and this is the second, IPC-level fence.
#[tauri::command]
pub fn open_artifact(
    app: AppHandle,
    state: State<'_, Arc<AppState>>,
    artifact_type: String,
    artifact_path: String,
    artifact_label: Option<String>,
) -> Result<serde_json::Value, String> {
    tracing::info!(
        "[sidepanel::commands] open_artifact called: type={}, path={}, label={:?}",
        artifact_type,
        artifact_path,
        artifact_label
    );

    let resolved_path = if artifact_type != "terminal" {
        let p =
            std::fs::canonicalize(&artifact_path).map_err(|e| format!("Cannot open file: {e}"))?;
        if !is_within_allowed_roots(&state, &p) {
            return Err(format!(
                "Access denied: {} is outside the working directory and project roots",
                p.display()
            ));
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

    register_and_show(&app, &state, artifact_type, resolved_path, label)
}

/// Open a file the USER picks in a native dialog. The dialog runs in Rust,
/// so the path never crosses the webview boundary — no JS (benign or
/// compromised) can claim "the user picked /etc/passwd". Dialog picks bypass
/// the working-dir restriction (explicit user consent) but are still
/// canonicalised and registered in the whitelist.
///
/// The picker is awaited on the blocking pool, never inline: a sync command
/// runs on the main thread (wry's `WKScriptMessageHandler` delegate is
/// `MainThreadOnly`), and `blocking_pick_file` parks the thread it is called
/// on. rfd presents the panel from that same main thread, so calling it
/// inline froze the app with the sheet stuck on screen — its completion
/// handler could never run. As an async command the main thread stays free.
#[tauri::command]
pub async fn open_artifact_dialog(
    app: AppHandle,
    state: State<'_, Arc<AppState>>,
) -> Result<serde_json::Value, String> {
    let state = state.inner().clone();
    tokio::task::spawn_blocking(move || {
        use tauri_plugin_dialog::DialogExt;
        let picked = app
            .dialog()
            .file()
            .add_filter(
                "Documents",
                &[
                    "md", "html", "htm", "pdf", "doc", "docx", "txt", "rtf", "ppt", "pptx",
                    "xls", "tex",
                ],
            )
            .add_filter(
                "Images",
                &["png", "jpg", "jpeg", "gif", "webp", "bmp", "ico", "svg"],
            )
            .add_filter("Spreadsheets", &["xlsx", "xls", "csv", "tsv"])
            .add_filter(
                "Code",
                &[
                    "py", "rs", "ts", "js", "go", "sh", "css", "scss", "sql", "txt", "svelte",
                ],
            )
            .add_filter("Data", &["json", "yaml", "toml"])
            .add_filter("All files", &["*"])
            .blocking_pick_file();
        let Some(picked) = picked else {
            return Err("cancelled".to_string());
        };
        let path = picked
            .into_path()
            .map_err(|e| format!("Invalid picked path: {e}"))?;
        let canonical =
            std::fs::canonicalize(&path).map_err(|e| format!("Cannot open file: {e}"))?;
        let artifact_type = detect_artifact_type(&canonical);
        let label = canonical
            .file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_else(|| "unnamed".into());
        register_and_show(
            &app,
            &state,
            artifact_type,
            canonical.to_string_lossy().to_string(),
            label,
        )
    })
    .await
    .map_err(|e| format!("dialog task failed: {e}"))?
}

/// Close the side panel.
#[tauri::command]
pub fn close_sidepanel(app: AppHandle, state: State<'_, Arc<AppState>>) -> Result<(), String> {
    let mut sp = lock_poison_guard(&state.sidepanel);
    sp.visible = false;
    app.emit("sidepanel:toggle", false)
        .map_err(|e| e.to_string())?;
    Ok(())
}

/// Get current side panel state (for frontend initialization).
#[tauri::command]
pub fn get_sidepanel_state(state: State<'_, Arc<AppState>>) -> Result<serde_json::Value, String> {
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
    sync_html_root(&state, &sp);
    app.emit("sidepanel:artifacts-changed", tabs_payload(&sp))
        .map_err(|e| e.to_string())?;
    Ok(())
}

/// The artifact list + active tab, in the shape the `sidepanel:artifacts-changed`
/// listener expects.
fn tabs_payload(sp: &SidePanelState) -> serde_json::Value {
    serde_json::json!({
        "artifacts": sp.artifacts.iter().map(|a| serde_json::json!({
            "id": a.id, "type": a.artifact_type, "path": a.path, "label": a.label,
        })).collect::<Vec<_>>(),
        "active_id": sp.active_id,
    })
}

/// Bind the side panel's tabs to a conversation — called on every session
/// switch and once at startup. Tabs are per-session (A's artifacts must not
/// linger over B, P2-26), but the panel itself is window layout: it stays
/// open across the switch, and returning to a session restores its tabs.
#[tauri::command]
pub fn set_sidepanel_session(
    app: AppHandle,
    state: State<'_, Arc<AppState>>,
    session_id: String,
) -> Result<(), String> {
    // Lock order (sidepanel → sidepanel_sessions) is shared with
    // clear_sidepanel_artifacts; keep it consistent to stay deadlock-free.
    let mut sp = lock_poison_guard(&state.sidepanel);
    {
        let mut parked = lock_poison_guard(&state.sidepanel_sessions);
        sp.bind_session(&session_id, &mut parked);
    }
    tracing::info!(
        "[sidepanel::commands] set_sidepanel_session: session={} tabs={} visible={}",
        session_id,
        sp.artifacts.len(),
        sp.visible
    );
    sync_html_root(&state, &sp);
    app.emit("sidepanel:artifacts-changed", tabs_payload(&sp))
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
        sync_html_root(&state, &sp);
        app.emit("sidepanel:tab-switched", &artifact_id)
            .map_err(|e| e.to_string())?;
    }
    Ok(())
}

/// Clear all artifacts (hard reset; the session-switch path is
/// `set_sidepanel_session`, which parks instead of dropping them).
#[tauri::command]
pub fn clear_sidepanel_artifacts(
    app: AppHandle,
    state: State<'_, Arc<AppState>>,
) -> Result<(), String> {
    let mut sp = lock_poison_guard(&state.sidepanel);
    sp.clear();
    sp.visible = false;
    sync_html_root(&state, &sp);
    // Drop the parked copy too, or switching away and back would resurrect
    // the tabs the user just cleared.
    if let Some(session_id) = sp.session_id.clone() {
        lock_poison_guard(&state.sidepanel_sessions).remove(&session_id);
    }
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
    tracing::info!(
        "[sidepanel::commands] spawn_terminal COMMAND called: shell={:?} cwd={:?}",
        shell,
        cwd
    );
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
    tokio::task::spawn_blocking(move || terminal::write_to_terminal(registry, &sid, &bytes))
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
    tokio::task::spawn_blocking(move || terminal::resize_terminal(registry, &sid, cols, rows))
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
    tokio::task::spawn_blocking(move || terminal::close_terminal(registry, &sid))
        .await
        .map_err(|e| e.to_string())?
}

// ─── File content commands (Phase 3) ──────────────────────────────────────

/// Only files explicitly opened in the side panel (via `open_artifact`, be
/// it through the native dialog or the agent's open_side_panel tool) may be
/// read. Canonicalise first so symlink tricks cannot bypass the whitelist.
fn is_artifact_allowed(state: &AppState, canonical: &std::path::Path) -> bool {
    let roots = lock_poison_guard(&state.artifact_roots);
    roots
        .iter()
        .any(|r| canonical == r || canonical.starts_with(r))
}

/// Roots an artifact may live under: the app working dir plus every
/// registered project root (both canonicalised, so `..`/symlink prefix
/// tricks can't escape them).
fn is_within_allowed_roots(state: &AppState, canonical: &std::path::Path) -> bool {
    let mut roots = Vec::new();
    let mut push_root = |s: &str| match std::fs::canonicalize(s) {
        Ok(c) => roots.push(c),
        Err(_) => roots.push(std::path::PathBuf::from(s)),
    };
    push_root(&state.working_dir);
    let projects = lock_poison_guard(&state.projects);
    for p in projects.iter() {
        push_root(&p.root_path);
    }
    roots.iter().any(|r| canonical.starts_with(r))
}

/// Hard cap for files returned over IPC (images and office docs are served
/// as raw bytes; a multi-GB file must not be materialised in memory).
const MAX_BYTES_FILE: u64 = 32 * 1024 * 1024;
/// Hard cap for text files returned as strings.
const MAX_TEXT_FILE: u64 = 8 * 1024 * 1024;

/// Canonicalise → whitelist-check → OPEN ONCE. The returned `File` pins the
/// inode, so a rename between the check and the read cannot swap in a
/// different file (TOCTOU). Size is verified via `fstat` on the same handle.
fn open_whitelisted(path: &str, state: &AppState, max_bytes: u64) -> Result<std::fs::File, String> {
    let resolved = std::fs::canonicalize(path).map_err(|e| format!("Cannot resolve path: {e}"))?;
    if !is_artifact_allowed(state, &resolved) {
        return Err("Access denied: only files opened in the side panel can be read".into());
    }
    let file = std::fs::File::open(&resolved).map_err(|e| format!("Cannot open file: {e}"))?;
    let meta = file
        .metadata()
        .map_err(|e| format!("Cannot stat file: {e}"))?;
    if meta.len() > max_bytes {
        return Err(format!(
            "File too large to preview (max {} bytes)",
            max_bytes
        ));
    }
    Ok(file)
}

/// Read a file's binary content. Returns raw bytes via the IPC response
/// channel — the previous Vec<u8> JSON encoding serialized every byte as a
/// number (~3-4x size and an extra copy) for multi-MB PDFs.
#[tauri::command]
pub fn read_file_bytes(
    path: String,
    state: State<'_, Arc<AppState>>,
) -> Result<tauri::ipc::Response, String> {
    let mut file = open_whitelisted(&path, &state, MAX_BYTES_FILE)?;
    let mut buf = Vec::new();
    file.read_to_end(&mut buf)
        .map_err(|e| format!("Cannot read file: {e}"))?;
    Ok(tauri::ipc::Response::new(buf))
}

/// Read a text file's content for code view.
#[tauri::command]
pub fn read_file_content(state: State<'_, Arc<AppState>>, path: String) -> Result<String, String> {
    tracing::info!(
        "[sidepanel::commands] read_file_content called: path={}",
        path
    );
    let mut file = open_whitelisted(&path, &state, MAX_TEXT_FILE)?;
    let mut buf = String::new();
    file.read_to_string(&mut buf)
        .map_err(|e| format!("Cannot read file: {e}"))?;
    Ok(buf)
}

/// Parse an Excel (.xlsx/.xls/.xlsb) or CSV/TSV file into a 2D string array.
/// Rows/cells are truncated at MAX_EXCEL_ROWS / MAX_EXCEL_CELLS_PER_ROW so a
/// hostile spreadsheet can't blow up the IPC payload or the frontend grid
/// (P3/A8).
#[tauri::command]
pub async fn parse_excel(
    path: String,
    state: State<'_, Arc<AppState>>,
) -> Result<Vec<Vec<String>>, String> {
    // Phase 1 (async side): whitelist check + bounded read of the pinned
    // fd. 50k-row workbooks then parse on the blocking pool — the parse
    // is CPU-heavy and used to occupy the command path for seconds.
    let resolved = std::fs::canonicalize(&path).map_err(|e| format!("Cannot resolve path: {e}"))?;
    if !is_artifact_allowed(&state, &resolved) {
        return Err("Access denied: only files opened in the side panel can be read".into());
    }
    let ext = resolved
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("")
        .to_lowercase();

    let payload = match ext.as_str() {
        "csv" | "tsv" => {
            let mut file = open_whitelisted(&path, &state, MAX_BYTES_FILE)?;
            let mut content = String::new();
            file.read_to_string(&mut content)
                .map_err(|e| format!("Cannot read CSV: {e}"))?;
            ExcelPayload::Text(content)
        }
        "xls" | "xlsx" => {
            let mut file = open_whitelisted(&path, &state, MAX_BYTES_FILE)?;
            let mut data = Vec::new();
            file.read_to_end(&mut data)
                .map_err(|e| format!("Cannot read workbook: {e}"))?;
            ExcelPayload::Bytes(data)
        }
        _ => {
            let meta =
                std::fs::metadata(&resolved).map_err(|e| format!("Cannot stat file: {e}"))?;
            if meta.len() > MAX_BYTES_FILE {
                return Err("File too large to preview (max 32 MB)".into());
            }
            ExcelPayload::Path(resolved)
        }
    };

    tokio::task::spawn_blocking(move || parse_excel_payload(&ext, payload))
        .await
        .map_err(|e| format!("excel parse task failed: {e}"))?
}

enum ExcelPayload {
    Text(String),
    Bytes(Vec<u8>),
    Path(std::path::PathBuf),
}

fn parse_excel_payload(ext: &str, payload: ExcelPayload) -> Result<Vec<Vec<String>>, String> {
    use calamine::{open_workbook_auto, open_workbook_auto_from_rs, Reader};
    const MAX_EXCEL_ROWS: usize = 50_000;
    const MAX_EXCEL_CELLS_PER_ROW: usize = 2_000;

    let mut rows: Vec<Vec<String>> = Vec::new();
    match (ext, payload) {
        ("csv" | "tsv", ExcelPayload::Text(content)) => {
            let sep = if ext == "tsv" { '\t' } else { ',' };
            for line in content.lines() {
                if line.trim().is_empty() {
                    continue;
                }
                let mut row: Vec<String> = line
                    .split(sep)
                    .map(|c| c.trim().trim_matches('"').to_string())
                    .collect();
                row.truncate(MAX_EXCEL_CELLS_PER_ROW);
                rows.push(row);
                if rows.len() >= MAX_EXCEL_ROWS {
                    break;
                }
            }
            Ok(rows)
        }
        ("xls" | "xlsx", ExcelPayload::Bytes(data)) => {
            let mut wb = open_workbook_auto_from_rs(std::io::Cursor::new(data))
                .map_err(|e| format!("Cannot open workbook: {e}"))?;
            let sheet_names = wb.sheet_names().to_vec();
            if sheet_names.is_empty() {
                return Err("No sheets found".into());
            }
            let range = wb
                .worksheet_range(&sheet_names[0])
                .map_err(|e| format!("Cannot read sheet: {e}"))?;
            for r in range.rows() {
                let mut row: Vec<String> = r.iter().map(|cell| cell.to_string()).collect();
                row.truncate(MAX_EXCEL_CELLS_PER_ROW);
                rows.push(row);
                if rows.len() >= MAX_EXCEL_ROWS {
                    break;
                }
            }
            Ok(rows)
        }
        (_, ExcelPayload::Path(p)) => {
            let mut wb =
                open_workbook_auto(&p).map_err(|e| format!("Cannot open workbook: {e}"))?;
            let sheet_names = wb.sheet_names().to_vec();
            if sheet_names.is_empty() {
                return Err("No sheets found".into());
            }
            let range = wb
                .worksheet_range(&sheet_names[0])
                .map_err(|e| format!("Cannot read sheet: {e}"))?;
            for r in range.rows() {
                let mut row: Vec<String> = r.iter().map(|cell| cell.to_string()).collect();
                row.truncate(MAX_EXCEL_CELLS_PER_ROW);
                rows.push(row);
                if rows.len() >= MAX_EXCEL_ROWS {
                    break;
                }
            }
            Ok(rows)
        }
        _ => Err("Unsupported workbook".into()),
    }
}

// ─── Git diff command (Phase 4) ──────────────────────────────────────────

/// Run `git diff` for a specific file and return the output.
#[tauri::command]
pub fn get_git_diff(state: State<'_, Arc<AppState>>, path: String) -> Result<String, String> {
    let resolved = std::fs::canonicalize(&path).map_err(|e| format!("Cannot resolve path: {e}"))?;
    if !is_artifact_allowed(&state, &resolved) {
        return Err("Access denied: only files opened in the side panel can be read".into());
    }
    // workdir is used only as the git invocation cwd.
    let workdir = std::fs::canonicalize(&state.working_dir)
        .unwrap_or_else(|_| std::path::PathBuf::from(&state.working_dir));

    // Bounded git invocation: an external diff tool (diff.external /
    // textconv) or a huge repo could otherwise hang the command forever.
    let mut child = std::process::Command::new("git")
        .args([
            "--no-pager",
            "-c",
            "core.pager=cat",
            "diff",
            "--no-ext-diff",
            "--no-color",
            "--",
            &resolved.to_string_lossy(),
        ])
        .current_dir(&workdir)
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .map_err(|e| format!("git command failed: {e}"))?;
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(15);
    loop {
        match child.try_wait() {
            Ok(Some(_)) => break,
            Ok(None) if std::time::Instant::now() < deadline => {
                std::thread::sleep(std::time::Duration::from_millis(50));
            }
            Ok(None) => {
                let _ = child.kill();
                let _ = child.wait();
                return Err("git diff timed out after 15s".into());
            }
            Err(e) => return Err(format!("git diff wait failed: {e}")),
        }
    }
    let output = child
        .wait_with_output()
        .map_err(|e| format!("git output failed: {e}"))?;

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
pub fn get_file_info(
    path: String,
    state: State<'_, Arc<AppState>>,
) -> Result<serde_json::Value, String> {
    let resolved = std::fs::canonicalize(&path).map_err(|e| e.to_string())?;
    if !is_artifact_allowed(&state, &resolved) {
        return Err("Access denied: only files opened in the side panel can be read".into());
    }
    let meta = std::fs::metadata(&resolved).map_err(|e| e.to_string())?;
    let modified = meta
        .modified()
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
pub fn open_external_file(path: String, state: State<'_, Arc<AppState>>) -> Result<(), String> {
    let resolved = std::fs::canonicalize(&path).map_err(|e| format!("Cannot resolve path: {e}"))?;
    if !is_artifact_allowed(&state, &resolved) {
        return Err("Access denied: only files opened in the side panel can be opened".into());
    }
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

#[cfg(test)]
mod tests {
    use super::*;

    fn tab(id: &str, artifact_type: &str, path: &str, label: &str) -> ArtifactInfo {
        ArtifactInfo {
            id: id.into(),
            artifact_type: artifact_type.into(),
            path: path.into(),
            label: label.into(),
        }
    }

    /// The reported bug: the agent opens its deliverable, the user clicks 打开
    /// on the card for that same file, and a second tab appears.
    #[test]
    fn focus_or_push_never_duplicates_an_open_path() {
        let mut tabs = vec![tab("a", "html", "/w/report.html", "报告")];
        let got = focus_or_push(
            &mut tabs,
            "html".into(),
            "/w/report.html".into(),
            "报告 修订版".into(),
        );
        assert_eq!(tabs.len(), 1, "one file must stay one tab");
        assert_eq!(got.id, "a", "the existing id is reused so the panel focuses it");
        assert_eq!(got.label, "报告", "the tab keeps the title it was opened with");
    }

    /// The viewer must survive a re-open: a caller whose type is only an
    /// extension guess (code) must not downgrade the agent's declared one.
    #[test]
    fn focus_or_push_keeps_an_open_tabs_viewer_and_title() {
        let mut tabs = vec![
            tab("a", "html", "/w/x.html", "X"),
            tab("b", "spreadsheet", "/w/data.json", "数据表"),
        ];
        let got = focus_or_push(&mut tabs, "code".into(), "/w/data.json".into(), "data.json".into());
        assert_eq!(tabs.len(), 2);
        assert_eq!(tabs[1].id, "b", "position is preserved — no reordering");
        assert_eq!(got.artifact_type, "spreadsheet", "the declared viewer stays");
        assert_eq!(got.label, "数据表", "the tab is not renamed by a coarser caller");
        assert_eq!(tabs[0].artifact_type, "html", "other tabs are untouched");
    }

    /// A terminal tab is a session, not a file: its path is always ".", so
    /// deduping on it would make the 终端 button a no-op after the first click
    /// (the viewer remounts — and spawns the PTY — on id change only).
    #[test]
    fn focus_or_push_always_mints_a_fresh_terminal_tab() {
        let mut tabs: Vec<ArtifactInfo> = Vec::new();
        let first = focus_or_push(&mut tabs, "terminal".into(), ".".into(), "Terminal".into());
        let second = focus_or_push(&mut tabs, "terminal".into(), ".".into(), "Terminal".into());
        assert_eq!(tabs.len(), 2, "every shell needs its own tab");
        assert_ne!(first.id, second.id, "a fresh id makes the PTY view remount");
    }

    #[test]
    fn focus_or_push_appends_a_new_path_with_a_fresh_id() {
        let mut tabs = vec![tab("a", "html", "/w/x.html", "X")];
        let got = focus_or_push(&mut tabs, "markdown".into(), "/w/y.md".into(), "Y".into());
        assert_eq!(tabs.len(), 2);
        assert_eq!(tabs[1].id, got.id);
        assert_eq!(got.path, "/w/y.md");
        assert_eq!(got.artifact_type, "markdown", "a new tab uses the caller's type");
        assert_eq!(got.label, "Y");
        assert!(!got.id.is_empty() && got.id != "a", "a new tab needs its own id");
    }

    #[test]
    fn focus_or_push_keeps_distinct_paths_apart() {
        let mut tabs: Vec<ArtifactInfo> = Vec::new();
        focus_or_push(&mut tabs, "code".into(), "/w/a.py".into(), "a".into());
        focus_or_push(&mut tabs, "code".into(), "/w/b.py".into(), "b".into());
        assert_eq!(tabs.len(), 2);
        assert_ne!(tabs[0].id, tabs[1].id);
    }
}
