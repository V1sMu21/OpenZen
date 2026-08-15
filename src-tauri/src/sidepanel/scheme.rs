//! Custom URI scheme `ozfile://` for rendering HTML artifacts in the side panel.
//!
//! Why not the built-in `asset://` protocol? `convertFileSrc` percent-encodes
//! the *entire* path (slashes become `%2F`), so relative resources inside the
//! HTML (`css/style.css`, `js/main.js`) resolve to the URL root instead of
//! next to the document and the asset protocol rejects them. The asset protocol
//! cannot serve a multi-file HTML app.
//!
//! This scheme keeps real `/` separators in the URL (`ozfile://localhost/<abs path>`),
//! so the document's relative URLs resolve correctly. The handler maps the URL
//! path onto the filesystem, restricted to directories explicitly allowed by
//! `open_artifact` (via `AppState.html_roots`).

use std::path::{Path, PathBuf};
use std::sync::Arc;

use tauri::http::{Response, StatusCode};
use tauri::{AppHandle, Manager, UriSchemeContext};

use crate::AppState;

/// Register the `ozfile` URI scheme handler on the app builder.
pub fn register(builder: tauri::Builder<tauri::Wry>) -> tauri::Builder<tauri::Wry> {
    builder.register_uri_scheme_protocol("ozfile", |ctx, request| handle_request(ctx, request))
}

fn handle_request(
    ctx: UriSchemeContext<'_, tauri::Wry>,
    request: tauri::http::Request<Vec<u8>>,
) -> Response<Vec<u8>> {
    let Some(path) = decode_path(request.uri().path()) else {
        return error_response(StatusCode::BAD_REQUEST, "bad path");
    };
    let path = PathBuf::from(path);

    // Resolve symlinks/`..` so the whitelist check below cannot be bypassed.
    let Ok(canonical) = std::fs::canonicalize(&path) else {
        return error_response(StatusCode::NOT_FOUND, "not found");
    };

    if !is_allowed(ctx.app_handle(), &canonical) {
        eprintln!("[sidepanel::scheme] ozfile denied: {}", canonical.display());
        return error_response(StatusCode::FORBIDDEN, "not allowed");
    }

    if !canonical.is_file() {
        return error_response(StatusCode::NOT_FOUND, "not a file");
    }

    let Ok(content) = std::fs::read(&canonical) else {
        return error_response(StatusCode::INTERNAL_SERVER_ERROR, "read failed");
    };

    let mime = tauri::utils::mime_type::MimeType::parse(&content, &canonical.to_string_lossy());
    Response::builder()
        .status(StatusCode::OK)
        .header("Content-Type", mime)
        .header("Cache-Control", "no-cache")
        .body(content)
        .unwrap_or_else(|e| error_response(StatusCode::INTERNAL_SERVER_ERROR, &e.to_string()))
}

/// `request.uri().path()` is percent-encoded; decode to a filesystem path.
/// Keeps the leading `/` so the result is an absolute path.
fn decode_path(encoded: &str) -> Option<String> {
    let decoded = percent_encoding::percent_decode_str(encoded)
        .decode_utf8()
        .ok()?;
    Some(decoded.into_owned())
}

/// Check that the canonical file path is inside one of the whitelisted roots.
fn is_allowed(app: &AppHandle<tauri::Wry>, canonical: &Path) -> bool {
    let state = app.state::<Arc<AppState>>();
    let roots = state.html_roots.lock().unwrap_or_else(|e| e.into_inner());
    roots.iter().any(|root| canonical.starts_with(root))
}

fn error_response(status: StatusCode, msg: &str) -> Response<Vec<u8>> {
    Response::builder()
        .status(status)
        .header("Content-Type", "text/plain")
        .body(msg.as_bytes().to_vec())
        .unwrap_or_else(|_| Response::new(Vec::new()))
}
