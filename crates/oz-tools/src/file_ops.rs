use std::collections::HashMap;
use std::path::Path;

use async_trait::async_trait;
use oz_core_types::{ToolContext, ToolDefinition, ToolError, ToolFunction, ToolOutput};

use crate::registry::ToolHandler;

/// Maximum file size for read operations (10 MB).
const MAX_READ_SIZE: u64 = 10 * 1024 * 1024;

/// Absolute system locations — blocked everywhere except resolved temp
/// roots (macOS temp dirs live under /private/var, which is why these are
/// checked separately from the always-sensitive names below).
const SENSITIVE_SYSTEM_PATHS: &[&str] = &[
    "/etc/", "/usr/", "/bin/", "/sbin/", "/var/", "/boot/", "/dev/", "/proc/", "/sys/", "/root/",
];

/// Names that are sensitive regardless of location: even inside /tmp, a
/// path aiming at .ssh/.aws/credentials-style targets is rejected.
///
/// Read-side counterpart to the runtime-state fence below: with `read` and
/// `web_fetch` both unapproved, any credential file the agent can read can
/// be exfiltrated in a URL (round5 P1-8d). Kept to things that are *always*
/// credentials — a blanket "dotfiles" rule would break ordinary work.
const SENSITIVE_ALWAYS: &[&str] = &[
    ".ssh/",
    ".aws/",
    ".gnupg/",
    ".docker/",
    ".kube/",
    ".config/gh/",
    ".config/gcloud/",
    ".password-store/",
    ".terraform.d/",
    "mykey.toml",
    ".env",
    "credentials",
    "id_rsa",
    "id_ed25519",
    ".npmrc",
    ".netrc",
    ".pypirc",
    ".git-credentials",
    ".htpasswd",
];

fn contains_any(haystack_lower: &str, needles: &[&str]) -> bool {
    needles
        .iter()
        .any(|n| haystack_lower.contains(&n.to_lowercase()))
}

/// Runtime state that must never be rewritten through the file tools, even
/// though it lives inside the working directory.
///
/// `{working_dir}/openzen/trust.json` is the trust store the safety guard
/// reloads on every run: an unapproved `write` to it would let the agent
/// grant itself permission for arbitrary commands from the next message on.
/// The rest are the other injection surfaces — `.git/hooks/*` (executed by
/// the user's next commit), `servers.toml` (spawned as a command by the
/// server form), and the memory/facts text files that are pasted into every
/// system prompt. Matching is done on canonicalized paths relative to the
/// canonical working dir, so a project legitimately named `openzen` is not
/// affected — only the runtime sub-directory inside it.
fn is_protected_runtime_path(real: &Path, real_wd: &Path, for_write: bool) -> bool {
    const PROTECTED_DIRS: &[&str] = &["openzen", ".openzen", "memory", "facts", "checkpoints"];
    const PROTECTED_FILES: &[&str] = &["servers.toml", "trust.json"];
    // Reads are allowed into the runtime directories on purpose: the system
    // prompt tells the agent to read `{working_dir}/.openzen/reflections.jsonl`
    // (the Reflexion failure log), and `memory/`+`facts/` are injected into
    // the prompt anyway. Blocking them made that instruction impossible to
    // follow — the agent burned a tool call every turn on a path the fence
    // rejected (review findings B-2/D-4/E-3).
    if !for_write {
        // `servers.toml` can carry MCP server credentials in `env = {...}`.
        return real == real_wd.join("servers.toml");
    }
    for d in PROTECTED_DIRS {
        if real.starts_with(real_wd.join(d)) {
            return true;
        }
    }
    for f in PROTECTED_FILES {
        if real == real_wd.join(f) {
            return true;
        }
    }
    let git = real_wd.join(".git");
    if real.starts_with(git.join("hooks")) || real == git.join("config") {
        return true;
    }
    false
}

/// Read-side fence: working-dir containment, credentials, system paths.
pub(crate) fn is_in_working_dir(path: &str, working_dir: &str) -> bool {
    is_in_working_dir_inner(path, working_dir, false)
}

/// Write-side fence: everything the read side blocks, plus the runtime state
/// and injection surfaces (trust store, git hooks, servers.toml, the agent's
/// own memory files).
pub(crate) fn is_in_working_dir_for_write(path: &str, working_dir: &str) -> bool {
    is_in_working_dir_inner(path, working_dir, true)
}

/// May this mutating call write `path`?
///
/// Inside the workspace the fence decides. Outside it the user's own approval
/// does: the agent loop asks before dispatching an out-of-workspace
/// `write`/`edit`/`patch` (see `oz_core_types::path_trust`) and records a
/// positive answer for that session, which is the only way the registry ever
/// gets an entry. A denial records nothing, so the boundary still holds.
pub(crate) fn write_path_permitted(path: &str, ctx: &oz_core_types::ToolContext) -> bool {
    is_in_working_dir_for_write(path, &ctx.working_dir)
        || oz_core_types::path_trust::is_path_approved(&ctx.session_id, Path::new(path))
}

/// Shared rejection text for the mutating tools: state what happened, that
/// retrying is pointless, and how the call can legitimately succeed.
fn outside_write_message(tool: &str, path: &str, working_dir: &str) -> String {
    let hint = if !Path::new(path).is_absolute() {
        let suggested = Path::new(working_dir).join(path);
        format!(" Try `{}` instead.", suggested.display())
    } else {
        String::new()
    };
    format!(
        "{tool}: path `{path}` is outside the workspace `{working_dir}` (or in a protected \
         location). This is a permission boundary, not an argument error — retrying `{tool}` on \
         the SAME path fails the same way. The user is asked to approve out-of-workspace writes \
         automatically; this one was not approved, so either work inside the workspace.{hint}"
    )
}

fn is_in_working_dir_inner(path: &str, working_dir: &str, for_write: bool) -> bool {
    let wd = Path::new(working_dir);
    let p = Path::new(path);

    // Resolve relative paths against the working dir BEFORE
    // canonicalizing — otherwise new files (which don't exist yet
    // on disk) silently fail canonicalize() and get rejected with
    // a confusing "outside working directory" error that prompts
    // the model to re-read the source 4-5 times.
    let resolved: std::path::PathBuf = if p.is_absolute() {
        p.to_path_buf()
    } else {
        wd.join(p)
    };

    // Canonicalize FIRST. The old code returned "allowed" for any path
    // whose *components* started with /tmp before resolving anything:
    // `/tmp/../etc/passwd`, `/tmp/x/../../Users/<u>/.ssh/id_rsa` and
    // symlinks living under /tmp all passed the fence. Canonicalization
    // resolves `..` and symlinks, so every later check sees the real
    // target. New files resolve the deepest EXISTING ancestor and re-attach
    // the missing tail (a non-existent component cannot be a symlink, so
    // this adds no traversal surface).
    //
    // One level of parent lookup was not enough: for
    // `work/e2e/hello.py`, where `work/e2e/` does not exist yet, nothing
    // canonicalized and the caller reported "outside the workspace or in a
    // protected location" — a permission story for a missing directory. The
    // agent retried the same write three times in the 2026-10-23 isolated run
    // before falling back to `mkdir -p`. Now the path is allowed and the
    // write tool's own actionable ENOENT message ("parent directory does not
    // exist — mkdir -p") is what the model sees.
    let real_p = match std::fs::canonicalize(&resolved) {
        Ok(p) => Some(p),
        Err(_) => {
            let mut missing: Vec<std::ffi::OsString> = Vec::new();
            if let Some(name) = resolved.file_name() {
                missing.push(name.to_os_string());
            }
            let mut found = None;
            let mut cursor = resolved.parent();
            while let Some(dir) = cursor {
                if let Ok(real_dir) = std::fs::canonicalize(dir) {
                    let mut rebuilt = real_dir;
                    for part in missing.iter().rev() {
                        rebuilt.push(part);
                    }
                    found = Some(rebuilt);
                    break;
                }
                if let Some(name) = dir.file_name() {
                    missing.push(name.to_os_string());
                }
                cursor = dir.parent();
            }
            found
        }
    };

    // Sensitive-name check runs on the canonical path so traversal cannot
    // smuggle sensitive targets past the substring match. The
    // always-sensitive list applies even inside temp (a /tmp symlink to
    // ~/.ssh resolves out of the temp roots anyway and is caught here).
    let check = real_p.as_deref().unwrap_or(&resolved);
    let lower = check.to_string_lossy().to_lowercase();
    if contains_any(&lower, SENSITIVE_ALWAYS) {
        return false;
    }

    // Runtime state / injection surfaces inside the working directory.
    // Checked before the temp early-return so a working dir under a temp
    // root cannot bypass it.
    let real_wd = std::fs::canonicalize(wd).ok();
    if let (Some(rp), Some(rw)) = (real_p.as_deref(), real_wd.as_deref()) {
        if is_protected_runtime_path(rp, rw, for_write) {
            tracing::warn!(
                "[safety] rejected runtime-state path (injection surface): {}",
                rp.display()
            );
            return false;
        }
    }

    // Temp dirs are allowed — but only on the canonical path, and only
    // after `..`/symlink resolution.
    if let Some(rp) = real_p.as_deref() {
        if rp.starts_with("/tmp")
            || rp.starts_with("/private/tmp")
            || rp.starts_with("/var/tmp")
            || rp.starts_with("/private/var/tmp")
            || rp.starts_with("/var/folders")
            || rp.starts_with("/private/var/folders")
        {
            return true;
        }
    }

    // System locations: blocked outside temp roots.
    if contains_any(&lower, SENSITIVE_SYSTEM_PATHS) {
        return false;
    }

    match (real_p, real_wd) {
        (Some(rp), Some(rw)) => rp.starts_with(&rw),
        _ => {
            // If canonicalize fails (broken symlink, etc.), reject
            tracing::warn!("[safety] cannot canonicalize path, rejecting: {path}");
            false
        }
    }
}

// ── FileReadTool ──

pub struct FileReadTool;

#[async_trait]
impl ToolHandler for FileReadTool {
    fn name(&self) -> String {
        "read".to_string()
    }
    fn description(&self) -> String {
        "Read a file (txt, pdf, docx, pptx, xlsx) with optional line range. Parameter names are fixed: `file_path` (NOT `path`/`file`/`filename`), optional `start` and `count`. Do NOT re-read files you just wrote/edited — failures report themselves and the harness tracks file state. Use start/count for long files.".to_string()
    }
    fn parameters(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "file_path": { "type": "string", "description": "Path to file" },
                "start": { "type": "integer", "description": "Start line (1-based)" },
                "count": { "type": "integer", "description": "Number of lines (default 200)" }
            },
            "required": ["file_path"]
        })
    }

    async fn execute(
        &self,
        args: serde_json::Value,
        ctx: &ToolContext,
    ) -> Result<ToolOutput, ToolError> {
        let path = args["file_path"].as_str().unwrap_or("");
        if path.is_empty() {
            return Ok(ToolOutput::bad_json(oz_core_types::missing_arg_error(
                "read",
                &["file_path"],
                &args,
            )));
        }
        if let Ok(meta) = tokio::fs::metadata(path).await {
            if meta.len() > MAX_READ_SIZE {
                return Ok(ToolOutput::bad_json(format!(
                    "read: file too large ({} bytes, max {})",
                    meta.len(),
                    MAX_READ_SIZE
                )));
            }
        }
        if !is_in_working_dir(path, &ctx.working_dir) {
            let hint = if !Path::new(path).is_absolute() {
                let suggested = Path::new(&ctx.working_dir).join(path);
                format!(" Try the absolute path `{}` instead.", suggested.display())
            } else {
                String::new()
            };
            return Ok(ToolOutput::bad_json(
                format!("read: path `{path}` is outside working directory `{}` or in a protected location.{}", ctx.working_dir, hint)
            ));
        }
        let start = args.get("start").and_then(|v| v.as_u64()).unwrap_or(1) as usize;
        let count = args.get("count").and_then(|v| v.as_u64()).unwrap_or(200) as usize;

        if crate::doc_reader::is_supported_doc(path) {
            match crate::doc_reader::read_document(path, start, count) {
                Ok(result) => return Ok(ToolOutput::success(result)),
                Err(e) => return Ok(ToolOutput::bad_json(format!("read: {e}"))),
            }
        }

        if crate::doc_reader::is_image(path) {
            match crate::doc_reader::read_image_base64(path) {
                Ok(data_url) => {
                    let ext = Path::new(path)
                        .extension()
                        .and_then(|e| e.to_str())
                        .unwrap_or("")
                        .to_lowercase();
                    let mime = crate::doc_reader::media_type_for(&ext);
                    return Ok(ToolOutput {
                        data: serde_json::json!({
                            "content": format!("[image: {path}]"),
                            "type": "image",
                            "media_type": mime,
                        }),
                        next_prompt: Some("[image attached above]".into()),
                        should_exit: false,
                        images: vec![oz_core_types::ImageRef {
                            url: data_url,
                            media_type: mime.to_string(),
                        }],
                    });
                }
                Err(e) => return Ok(ToolOutput::bad_json(format!("read: {e}"))),
            }
        }

        match tokio::fs::read_to_string(path).await {
            Ok(content) => {
                let lines: Vec<&str> = content.lines().collect();
                let from = start.saturating_sub(1).min(lines.len());
                let to = (from + count).min(lines.len());
                let excerpt = lines[from..to].join("\n");
                let meta = serde_json::json!({
                    "content": excerpt,
                    "total_lines": lines.len(),
                    "start_line": from + 1,
                    "end_line": to,
                });
                Ok(ToolOutput::success(meta))
            }
            Err(e) => Ok(ToolOutput::bad_json(format!(
                "read failed: {e}. Use 'ls' or 'glob' to check the file exists, then retry with the correct path."
            ))),
        }
    }
}

// ── FileWriteTool ──

pub struct FileWriteTool;

#[async_trait]
impl ToolHandler for FileWriteTool {
    fn name(&self) -> String {
        "write".to_string()
    }
    fn description(&self) -> String {
        "Write content to a file (creates parent dirs). Parameter names are fixed: `file_path` (NOT `path`/`file`/`filename`) and `content` (NOT `text`/`data`/`body`). No confirmation re-read after writing — failures report themselves.".to_string()
    }
    fn parameters(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "file_path": { "type": "string", "description": "Path to write to" },
                "content": { "type": "string", "description": "Content to write" }
            },
            "required": ["file_path", "content"]
        })
    }

    async fn execute(
        &self,
        args: serde_json::Value,
        ctx: &ToolContext,
    ) -> Result<ToolOutput, ToolError> {
        let path = args["file_path"].as_str().unwrap_or("");
        let content = args["content"].as_str().unwrap_or("");
        if path.is_empty() {
            return Ok(ToolOutput::bad_json(oz_core_types::missing_arg_error(
                "write",
                &["file_path"],
                &args,
            )));
        }
        // A *missing* `content` key used to silently write an empty file; an
        // explicitly empty string stays legal (`touch`-style create).
        if args.get("content").and_then(|v| v.as_str()).is_none() {
            return Ok(ToolOutput::bad_json(oz_core_types::missing_arg_error(
                "write",
                &["content"],
                &args,
            )));
        }
        if !write_path_permitted(path, ctx) {
            return Ok(ToolOutput::bad_json(outside_write_message(
                "write",
                path,
                &ctx.working_dir,
            )));
        }
        if let Some(parent) = Path::new(path).parent() {
            let _ = tokio::fs::create_dir_all(parent).await;
        }
        match tokio::fs::write(path, content).await {
            Ok(()) => Ok(ToolOutput::success(serde_json::json!({"status": "written"}))),
            Err(e) => Ok(ToolOutput::bad_json(format!(
                "write failed: {e}. Check that the parent directory exists — use 'code_run: mkdir -p <parent>' if needed."
            ))),
        }
    }
}

// ── FilePatchTool ──

pub struct FilePatchTool;

#[async_trait]
impl ToolHandler for FilePatchTool {
    fn name(&self) -> String {
        "patch".to_string()
    }
    fn description(&self) -> String {
        "Apply a search-and-replace edit to a file. No confirmation re-read after editing — failures report themselves.".to_string()
    }
    fn parameters(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "file_path": { "type": "string", "description": "Path to file" },
                "old_string": { "type": "string", "description": "Text to find (must be unique)" },
                "new_string": { "type": "string", "description": "Replacement text" }
            },
            "required": ["file_path", "old_string", "new_string"]
        })
    }

    async fn execute(
        &self,
        args: serde_json::Value,
        ctx: &ToolContext,
    ) -> Result<ToolOutput, ToolError> {
        let path = args["file_path"].as_str().unwrap_or("");
        let old = args["old_string"].as_str().unwrap_or("");
        let new = args["new_string"].as_str().unwrap_or("");
        if path.is_empty() || old.is_empty() {
            let mut missing: Vec<&str> = Vec::new();
            if path.is_empty() {
                missing.push("file_path");
            }
            if old.is_empty() {
                missing.push("old_string");
            }
            return Ok(ToolOutput::bad_json(oz_core_types::missing_arg_error(
                "patch", &missing, &args,
            )));
        }
        if !write_path_permitted(path, ctx) {
            return Ok(ToolOutput::bad_json(outside_write_message(
                "patch",
                path,
                &ctx.working_dir,
            )));
        }

        let content = match tokio::fs::read_to_string(path).await {
            Ok(c) => c,
            Err(e) => return Ok(ToolOutput::bad_json(format!("patch read failed: {e}"))),
        };

        if !content.contains(old) {
            return Ok(ToolOutput::bad_json(
                "patch: old_string not found in file. Re-read the latest file content and use the exact text (including whitespace) as old_string."
            ));
        }

        let count = content.matches(old).count();
        if count > 1 {
            return Ok(ToolOutput::bad_json(format!(
                "patch: old_string appears {count} times — must be unique"
            )));
        }

        let new_content = content.replace(old, new);
        match tokio::fs::write(path, &new_content).await {
            Ok(()) => Ok(ToolOutput::success(
                serde_json::json!({"status": "patched"}),
            )),
            Err(e) => Ok(ToolOutput::bad_json(format!("patch write failed: {e}"))),
        }
    }
}

pub struct FileEditTool;

#[async_trait]
impl ToolHandler for FileEditTool {
    fn name(&self) -> String {
        "edit".to_string()
    }
    fn description(&self) -> String {
        "Make a focused edit to a file by replacing an exact text match. The old_string must be unique in the file. Prefer this over `write` for code changes — it shows a clean +/- diff to the user and avoids touching lines you don't intend to change.".to_string()
    }
    fn parameters(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "file_path": { "type": "string", "description": "File path" },
                "old_string": { "type": "string", "description": "Text to find and replace" },
                "new_string": { "type": "string", "description": "Replacement text" }
            },
            "required": ["file_path", "old_string", "new_string"]
        })
    }

    async fn execute(
        &self,
        args: serde_json::Value,
        ctx: &ToolContext,
    ) -> Result<ToolOutput, ToolError> {
        let path = args["file_path"].as_str().unwrap_or("");
        let old = args["old_string"].as_str().unwrap_or("");
        let new = args["new_string"].as_str().unwrap_or("");
        if path.is_empty() || old.is_empty() {
            let mut missing: Vec<&str> = Vec::new();
            if path.is_empty() {
                missing.push("file_path");
            }
            if old.is_empty() {
                missing.push("old_string");
            }
            return Ok(ToolOutput::bad_json(oz_core_types::missing_arg_error(
                "edit", &missing, &args,
            )));
        }
        if !write_path_permitted(path, ctx) {
            return Ok(ToolOutput::bad_json(outside_write_message(
                "edit",
                path,
                &ctx.working_dir,
            )));
        }

        let content = match tokio::fs::read_to_string(path).await {
            Ok(c) => c,
            Err(e) => return Ok(ToolOutput::bad_json(format!("edit read failed: {e}"))),
        };

        if !content.contains(old) {
            return Ok(ToolOutput::bad_json(
                "edit: old_string not found in file. Re-read the latest file content and use the exact text (including whitespace) as old_string."
            ));
        }

        let count = content.matches(old).count();
        if count > 1 {
            return Ok(ToolOutput::bad_json(format!(
                "edit: old_string appears {count} times — must be unique"
            )));
        }

        let new_content = content.replace(old, new);
        match tokio::fs::write(path, &new_content).await {
            Ok(()) => Ok(ToolOutput::success(serde_json::json!({"status": "edited"}))),
            Err(e) => Ok(ToolOutput::bad_json(format!("edit write failed: {e}"))),
        }
    }
}

// ── GlobTool ──

pub struct GlobTool;

#[async_trait]
impl ToolHandler for GlobTool {
    fn name(&self) -> String {
        "glob".to_string()
    }
    fn description(&self) -> String {
        "List files matching a glob pattern. Use this tool for search — do not write python scripts to read large directories.".to_string()
    }
    fn parameters(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "pattern": { "type": "string", "description": "Glob pattern (e.g. **/*.rs)" },
                "path": { "type": "string", "description": "Base directory (optional)" }
            },
            "required": ["pattern"]
        })
    }

    async fn execute(
        &self,
        args: serde_json::Value,
        ctx: &ToolContext,
    ) -> Result<ToolOutput, ToolError> {
        let pattern = args["pattern"].as_str().unwrap_or("");
        if pattern.is_empty() {
            return Ok(ToolOutput::bad_json(oz_core_types::missing_arg_error(
                "glob",
                &["pattern"],
                &args,
            )));
        }
        let base = args.get("path").and_then(|v| v.as_str()).unwrap_or(".");
        // Path fence: this tool is on the no-approval safe list, so the
        // base directory must respect the same working-dir boundary as
        // read/write (previously `glob "/Users/*/.aws/*"` was allowed).
        if !is_in_working_dir(base, &ctx.working_dir) {
            return Ok(ToolOutput::bad_json(format!(
                "glob: path '{base}' is outside the working directory"
            )));
        }
        let glob_pattern = format!("{}/{}", base.trim_end_matches('/'), pattern);

        let mut results = Vec::new();
        if let Ok(entries) = glob::glob(&glob_pattern) {
            for entry in entries.flatten() {
                // Same per-entry fence as grep: the pattern itself can walk
                // into a fenced file even when the base is inside the
                // working dir (e.g. `glob "**/.env"`).
                if !is_in_working_dir(&entry.to_string_lossy(), &ctx.working_dir) {
                    continue;
                }
                results.push(entry.to_string_lossy().to_string());
            }
        }
        results.sort();
        Ok(ToolOutput::success(serde_json::json!({"files": results})))
    }
}

// ── GrepTool ──

pub struct GrepTool;

#[async_trait]
impl ToolHandler for GrepTool {
    fn name(&self) -> String {
        "grep".to_string()
    }
    fn description(&self) -> String {
        "Search file contents with a regex pattern. Use this tool for search — do not write python scripts to scan large files.".to_string()
    }
    fn parameters(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "pattern": { "type": "string", "description": "Regex pattern" },
                "path": { "type": "string", "description": "Directory to search (optional)" },
                "include": { "type": "string", "description": "File glob filter (optional)" }
            },
            "required": ["pattern"]
        })
    }

    async fn execute(
        &self,
        args: serde_json::Value,
        ctx: &ToolContext,
    ) -> Result<ToolOutput, ToolError> {
        let pattern = args["pattern"].as_str().unwrap_or("");
        if pattern.is_empty() {
            return Ok(ToolOutput::bad_json(oz_core_types::missing_arg_error(
                "grep",
                &["pattern"],
                &args,
            )));
        }
        let base = args.get("path").and_then(|v| v.as_str()).unwrap_or(".");
        if !is_in_working_dir(base, &ctx.working_dir) {
            return Ok(ToolOutput::bad_json(format!(
                "grep: path '{base}' is outside the working directory"
            )));
        }
        let include = args.get("include").and_then(|v| v.as_str());

        let re = regex::Regex::new(pattern)
            .map_err(|e| ToolError::Custom(format!("invalid regex: {e}")))?;

        let mut results: Vec<HashMap<String, serde_json::Value>> = Vec::new();
        let walk_path = Path::new(base);

        if walk_path.is_dir() {
            let mut walk_entries = Vec::new();
            if let Ok(mut rd) = tokio::fs::read_dir(walk_path).await {
                while let Ok(Some(entry)) = rd.next_entry().await {
                    walk_entries.push(entry.path());
                }
            }
            // Only non-recursive for now; keep simple
            for entry_path in walk_entries {
                if entry_path.is_dir() {
                    continue;
                }
                if let Some(inc) = include {
                    let fname = entry_path.file_name().unwrap_or_default().to_string_lossy();
                    if !glob::Pattern::new(inc)
                        .map(|p| p.matches(&fname))
                        .unwrap_or(false)
                    {
                        continue;
                    }
                }
                // The base directory was fenced, but the ENTRIES were not:
                // `grep {pattern:".", path:"<wd>"}` returned every line of
                // `<wd>/.env` — the exact credential-read path that the
                // unapproved `web_fetch` turns into exfiltration. Every file
                // is re-checked (canonicalized, so a symlink out of the
                // working dir is caught too).
                if !is_in_working_dir(&entry_path.to_string_lossy(), &ctx.working_dir) {
                    tracing::debug!(
                        "[safety] grep skipped fenced entry: {}",
                        entry_path.display()
                    );
                    continue;
                }
                if let Ok(content) = tokio::fs::read_to_string(&entry_path).await {
                    for (lineno, line) in content.lines().enumerate() {
                        if re.is_match(line) {
                            let mut m = HashMap::new();
                            m.insert(
                                "file".into(),
                                serde_json::json!(entry_path.to_string_lossy().to_string()),
                            );
                            m.insert("line".into(), serde_json::json!(lineno + 1));
                            m.insert("text".into(), serde_json::json!(line.to_string()));
                            results.push(m);
                        }
                    }
                }
            }
        } else if walk_path.is_file() {
            if let Ok(content) = tokio::fs::read_to_string(walk_path).await {
                for (lineno, line) in content.lines().enumerate() {
                    if re.is_match(line) {
                        let mut m = HashMap::new();
                        m.insert(
                            "file".into(),
                            serde_json::json!(walk_path.to_string_lossy().to_string()),
                        );
                        m.insert("line".into(), serde_json::json!(lineno + 1));
                        m.insert("text".into(), serde_json::json!(line.to_string()));
                        results.push(m);
                    }
                }
            }
        }

        Ok(ToolOutput::success(
            serde_json::json!({"matches": results, "count": results.len()}),
        ))
    }
}

// ── LsTool ──

pub struct LsTool;

#[async_trait]
impl ToolHandler for LsTool {
    fn name(&self) -> String {
        "ls".to_string()
    }
    fn description(&self) -> String {
        "List directory contents. Use this tool for directory exploration — not python scripts."
            .to_string()
    }
    fn parameters(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "path": { "type": "string", "description": "Directory path (default .)" }
            },
            "required": []
        })
    }

    async fn execute(
        &self,
        args: serde_json::Value,
        ctx: &ToolContext,
    ) -> Result<ToolOutput, ToolError> {
        let path = args.get("path").and_then(|v| v.as_str()).unwrap_or(".");
        if !is_in_working_dir(path, &ctx.working_dir) {
            return Ok(ToolOutput::bad_json(format!(
                "ls: path '{path}' is outside the working directory"
            )));
        }
        let mut entries: Vec<serde_json::Value> = Vec::new();
        if let Ok(mut rd) = tokio::fs::read_dir(path).await {
            while let Ok(Some(entry)) = rd.next_entry().await {
                let ftype = if entry.file_type().await.map(|t| t.is_dir()).unwrap_or(false) {
                    "dir"
                } else {
                    "file"
                };
                entries.push(serde_json::json!({
                    "name": entry.file_name().to_string_lossy().to_string(),
                    "type": ftype,
                }));
            }
        }
        entries.sort_by(|a, b| {
            let aname = a["name"].as_str().unwrap_or("");
            let bname = b["name"].as_str().unwrap_or("");
            aname.cmp(bname)
        });
        Ok(ToolOutput::success(serde_json::json!({"entries": entries})))
    }
}

// ── backward compat helpers ──

pub fn read_definition() -> ToolDefinition {
    def_for(&FileReadTool)
}
pub fn write_definition() -> ToolDefinition {
    def_for(&FileWriteTool)
}
pub fn edit_definition() -> ToolDefinition {
    def_for(&FileEditTool)
}
pub fn patch_definition() -> ToolDefinition {
    def_for(&FilePatchTool)
}
pub fn glob_definition() -> ToolDefinition {
    def_for(&GlobTool)
}
pub fn grep_definition() -> ToolDefinition {
    def_for(&GrepTool)
}
pub fn ls_definition() -> ToolDefinition {
    def_for(&LsTool)
}

fn def_for(t: &dyn ToolHandler) -> ToolDefinition {
    ToolDefinition {
        type_: "function".into(),
        function: ToolFunction {
            name: t.name(),
            description: t.description(),
            parameters: t.parameters(),
        },
    }
}

// Old-style handlers (blocking, for backward compat)
use oz_core_types::StepOutcome;
use std::sync::Arc;

macro_rules! compat_handler {
    ($tool:expr) => {{
        let t = Arc::new($tool);
        Arc::new(
            move |_name: &str, args: &serde_json::Value, ctx: &oz_core_types::ToolContext| {
                let args = args.clone();
                let ctx = ctx.clone();
                let t = t.clone();
                let rt = tokio::runtime::Runtime::new().expect("Failed to create tokio runtime");
                let result = rt
                    .block_on(t.execute(args, &ctx))
                    .unwrap_or_else(|e| ToolOutput::bad_json(e.to_string()));
                StepOutcome {
                    data: result.data,
                    next_prompt: result.next_prompt,
                    should_exit: result.should_exit,
                    images: result.images,
                }
            },
        ) as super::ToolHandler
    }};
}

pub fn read_handler() -> super::ToolHandler {
    compat_handler!(FileReadTool)
}
pub fn write_handler() -> super::ToolHandler {
    compat_handler!(FileWriteTool)
}
pub fn edit_handler() -> super::ToolHandler {
    compat_handler!(FileEditTool)
}
pub fn patch_handler() -> super::ToolHandler {
    compat_handler!(FilePatchTool)
}
pub fn glob_handler() -> super::ToolHandler {
    compat_handler!(GlobTool)
}
pub fn grep_handler() -> super::ToolHandler {
    compat_handler!(GrepTool)
}
pub fn ls_handler() -> super::ToolHandler {
    compat_handler!(LsTool)
}

#[cfg(test)]
mod tests {
    use super::*;
    use oz_core_types::ToolContext as TC;

    fn ctx() -> TC {
        TC {
            is_subagent: false,
            working_dir: "/tmp".into(),
            assets_dir: "/tmp".into(),
            script_dir: "/tmp".into(),
            lang: "en".into(),
            skill_mcp_dir: None,
            harness_dir: None,
            session_id: String::new(),
            tool_budget_secs: 0,
        }
    }

    /// Regression (2026-10-23): the workspace boundary is now approvable. An
    /// out-of-workspace write must stay blocked until the user approves that
    /// exact path for the session, and must go through once they have.
    #[tokio::test]
    async fn outside_write_needs_an_approval_and_then_succeeds() {
        // A non-temp location (temp roots are allow-listed by the fence itself).
        let base = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../target/oz-fence-test")
            .join(format!("outside-{}", std::process::id()));
        let ws = base.join("ws");
        let outside = base.join("outside/keep.txt");
        let _ = std::fs::remove_dir_all(&base);
        std::fs::create_dir_all(&ws).unwrap();
        std::fs::create_dir_all(outside.parent().unwrap()).unwrap();
        std::fs::write(&outside, "old\n").unwrap();

        let ctx = TC {
            is_subagent: false,
            working_dir: ws.to_string_lossy().to_string(),
            assets_dir: String::new(),
            script_dir: String::new(),
            lang: "en".into(),
            skill_mcp_dir: None,
            harness_dir: None,
            session_id: "sess-outside".into(),
            tool_budget_secs: 0,
        };
        let args = serde_json::json!({
            "file_path": outside.to_string_lossy(),
            "content": "new\n",
        });

        // 1. Not approved → the boundary still holds.
        let blocked = FileWriteTool.execute(args.clone(), &ctx).await.unwrap();
        assert_eq!(
            blocked.data["status"], "error",
            "unapproved outside write must be rejected: {:?}",
            blocked.data
        );
        let msg = blocked.data["error"].as_str().unwrap_or("");
        assert!(
            msg.contains("outside the workspace"),
            "the message must name the boundary: {msg}"
        );
        assert_eq!(std::fs::read_to_string(&outside).unwrap(), "old\n");

        // 2. The user approves exactly this path → the write lands.
        oz_core_types::path_trust::approve_path("sess-outside", &outside);
        let allowed = FileWriteTool.execute(args, &ctx).await.unwrap();
        assert_eq!(allowed.data["status"], "written", "{:?}", allowed.data);
        assert_eq!(std::fs::read_to_string(&outside).unwrap(), "new\n");

        // 3. A different session does not inherit it.
        oz_core_types::path_trust::forget_session("sess-outside");
        let other = TC {
            session_id: "sess-other".into(),
            ..ctx
        };
        let blocked_again = FileWriteTool
            .execute(
                serde_json::json!({
                    "file_path": outside.to_string_lossy(),
                    "content": "third\n",
                }),
                &other,
            )
            .await
            .unwrap();
        assert_eq!(blocked_again.data["status"], "error");
        assert_eq!(std::fs::read_to_string(&outside).unwrap(), "new\n");

        let _ = std::fs::remove_dir_all(&base);
    }

    #[tokio::test]
    async fn test_read_missing_path() {
        let r = FileReadTool
            .execute(serde_json::json!({}), &ctx())
            .await
            .unwrap();
        assert!(r.next_prompt.unwrap().contains("missing"));
    }

    /// Regression (2026-10-23 isolated run): a write whose parent directory
    /// does not exist yet was rejected as if it were a permission boundary —
    /// "cannot canonicalize path, rejecting: …/work/e2e/hello.py" — and the
    /// agent retried the same write three times before using `mkdir -p`.
    /// The path check must resolve the deepest existing ancestor instead, so
    /// the write proceeds and the tool reports the actionable ENOENT.
    #[tokio::test]
    async fn write_into_a_missing_directory_is_not_a_permission_error() {
        let base = std::env::temp_dir().join(format!("oz-missing-parent-{}", std::process::id()));
        let _ = std::fs::create_dir_all(&base);
        let target = base.join("deeper/not/created/hello.py");
        assert!(!target.parent().unwrap().exists(), "precondition");

        let ctx = TC {
            working_dir: base.to_string_lossy().to_string(),
            assets_dir: String::new(),
            script_dir: String::new(),
            lang: "en".into(),
            skill_mcp_dir: None,
            harness_dir: None,
            session_id: String::new(),
            tool_budget_secs: 0,
            is_subagent: false,
        };
        let out = FileWriteTool
            .execute(
                serde_json::json!({"file_path": target.to_string_lossy(), "content": "print('x')\n"}),
                &ctx,
            )
            .await
            .expect("a write into a non-existent subdirectory must be allowed");
        assert_eq!(out.data["status"], "written");
        assert_eq!(
            std::fs::read_to_string(&target).unwrap(),
            "print('x')\n",
            "write creates the parent directory itself"
        );
        let _ = std::fs::remove_dir_all(&base);
    }

    #[tokio::test]
    async fn test_read_nonexistent_file() {
        let r = FileReadTool
            .execute(
                serde_json::json!({"file_path": "/tmp/nonexistent_ga_test_xxx"}),
                &ctx(),
            )
            .await
            .unwrap();
        assert!(r.next_prompt.unwrap().contains("failed"));
    }

    #[tokio::test]
    async fn test_write_and_read_roundtrip() {
        let tmp = std::env::temp_dir().join("oz_test_write_read.txt");
        let path = tmp.to_string_lossy().to_string();

        let w = FileWriteTool
            .execute(
                serde_json::json!({"file_path": path, "content": "hello world\nline2"}),
                &ctx(),
            )
            .await
            .unwrap();
        assert_eq!(w.data["status"], "written");

        let r = FileReadTool
            .execute(serde_json::json!({"file_path": path}), &ctx())
            .await
            .unwrap();
        assert!(r.data["content"]
            .as_str()
            .unwrap_or("")
            .contains("hello world"));

        let _ = std::fs::remove_file(&tmp);
    }

    #[tokio::test]
    async fn test_patch_basic() {
        let tmp = std::env::temp_dir().join("oz_test_patch.txt");
        let path = tmp.to_string_lossy().to_string();
        tokio::fs::write(&path, "hello world\n").await.unwrap();

        let r = FilePatchTool
            .execute(
                serde_json::json!({"file_path": path, "old_string": "hello", "new_string": "hi"}),
                &ctx(),
            )
            .await
            .unwrap();
        assert_eq!(r.data["status"], "patched");

        let content = tokio::fs::read_to_string(&path).await.unwrap();
        assert_eq!(content, "hi world\n");
        let _ = std::fs::remove_file(&tmp);
    }

    #[tokio::test]
    async fn test_patch_not_found() {
        let tmp = std::env::temp_dir().join("oz_test_patch_nf.txt");
        tokio::fs::write(&tmp, "hello").await.unwrap();
        let path = tmp.to_string_lossy().to_string();

        let r = FilePatchTool
            .execute(serde_json::json!({"file_path": path, "old_string": "nonexistent", "new_string": "x"}), &ctx())
            .await
            .unwrap();
        assert!(r.next_prompt.unwrap().contains("not found"));
        let _ = std::fs::remove_file(&tmp);
    }

    #[tokio::test]
    async fn test_glob_empty_pattern() {
        let r = GlobTool
            .execute(serde_json::json!({}), &ctx())
            .await
            .unwrap();
        assert!(r.next_prompt.unwrap().contains("missing"));
    }

    #[tokio::test]
    async fn test_ls_tmp() {
        let r = LsTool
            .execute(serde_json::json!({"path": "/tmp"}), &ctx())
            .await
            .unwrap();
        let _entries = r.data["entries"].as_array().unwrap();
    }

    #[tokio::test]
    async fn test_grep_invalid_regex() {
        let r = GrepTool
            .execute(serde_json::json!({"pattern": "[invalid"}), &ctx())
            .await;
        assert!(r.is_err());
    }

    /// Review finding D-1: `grep {pattern:".", path:"<wd>"}` used to return
    /// every line of `<wd>/.env`. `read .env` was refused, but grep walked
    /// the directory and read the file anyway — with the unapproved
    /// `web_fetch` that is a complete credential-exfiltration path.
    #[tokio::test]
    async fn grep_does_not_read_fenced_files_inside_the_working_dir() {
        let wd = std::env::temp_dir().join(format!("oz-grep-fence-{}", std::process::id()));
        let _ = std::fs::create_dir_all(&wd);
        std::fs::write(wd.join(".env"), "DB_PASSWORD=supersecret\n").unwrap();
        std::fs::write(wd.join("app.rs"), "fn main() {}\n").unwrap();
        let c = ToolContext {
            working_dir: wd.to_string_lossy().to_string(),
            ..ctx()
        };
        let r = GrepTool
            .execute(
                serde_json::json!({"pattern": ".", "path": wd.to_string_lossy()}),
                &c,
            )
            .await
            .unwrap();
        let body = serde_json::to_string(&r.data).unwrap();
        assert!(
            !body.contains("supersecret"),
            "grep leaked a fenced credential file: {body}"
        );
        // The ordinary file next to it is still searchable.
        assert!(body.contains("app.rs"), "normal files must still be found");
        let _ = std::fs::remove_dir_all(&wd);
    }
}

#[linkme::distributed_slice(crate::registry::TOOL_FACTORIES)]
fn register_file_ops(reg: &mut crate::registry::ToolRegistry) {
    reg.register(crate::file_ops::FileReadTool);
    reg.register(crate::file_ops::FileWriteTool);
    reg.register(crate::file_ops::FilePatchTool);
    reg.register(crate::file_ops::GlobTool);
    reg.register(crate::file_ops::GrepTool);
    reg.register(crate::file_ops::LsTool);
}

#[cfg(test)]
mod fence_tests {
    use super::is_in_working_dir;

    fn wd() -> String {
        wd_named("wd")
    }

    /// Each test gets its own directory: they run in parallel and used to
    /// `remove_dir_all` the tree out from under each other, which made
    /// `canonicalize` fail and the fence reject valid paths (a flaky failure
    /// that looked like a security bug).
    fn wd_named(tag: &str) -> String {
        std::env::temp_dir()
            .join(format!("oz-fence-{tag}-{}", std::process::id()))
            .to_string_lossy()
            .to_string()
    }

    #[test]
    fn tmp_traversal_cannot_escape() {
        let wd = wd();
        let _ = std::fs::create_dir_all(&wd);
        // Component-prefix trick: starts_with("/tmp") used to pass before
        // any canonicalization; now `..` is resolved first.
        assert!(!is_in_working_dir("/tmp/../etc/passwd", &wd));
        assert!(!is_in_working_dir("/tmp/../usr/bin/ssh", &wd));
        let _ = std::fs::remove_dir_all(&wd);
    }

    #[test]
    fn sensitive_paths_rejected_even_under_tmp() {
        let wd = wd();
        let _ = std::fs::create_dir_all(&wd);
        assert!(!is_in_working_dir("/tmp/.ssh/id_rsa", &wd));
        assert!(!is_in_working_dir("/tmp/mykey.toml", &wd));
        let _ = std::fs::remove_dir_all(&wd);
    }

    #[cfg(unix)]
    #[test]
    fn symlink_out_of_tmp_resolves_and_rejects() {
        let wd = wd();
        let _ = std::fs::create_dir_all(&wd);
        let link = std::env::temp_dir().join(format!("oz-fence-link-{}", std::process::id()));
        let _ = std::fs::remove_file(&link);
        // Point a /tmp symlink at /etc — canonicalization resolves it out
        // of the temp roots and the sensitive check catches it.
        if std::os::unix::fs::symlink("/etc", &link).is_ok() {
            assert!(!is_in_working_dir(
                &link.join("passwd").to_string_lossy(),
                &wd
            ));
        }
        let _ = std::fs::remove_file(&link);
        let _ = std::fs::remove_dir_all(&wd);
    }

    #[test]
    fn working_dir_and_tmp_still_allowed() {
        let wd = wd();
        let _ = std::fs::create_dir_all(&wd);
        assert!(is_in_working_dir(&wd, &wd));
        assert!(is_in_working_dir(".", &wd));
        // A plain temp file (the common case) stays allowed.
        let f = std::env::temp_dir().join("oz-fence-plain.txt");
        std::fs::write(&f, "x").unwrap();
        assert!(is_in_working_dir(&f.to_string_lossy(), &wd));
        let _ = std::fs::remove_file(&f);
        let _ = std::fs::remove_dir_all(&wd);
    }

    #[test]
    fn outside_working_dir_rejected() {
        let wd = wd();
        let _ = std::fs::create_dir_all(&wd);
        assert!(!is_in_working_dir("/Users", &wd));
        let _ = std::fs::remove_dir_all(&wd);
    }

    /// round5 P0-4 regression: the trust store, the git hook directory and
    /// the memory text files are injection surfaces — rewriting them through
    /// an unapproved `write` used to let the agent grant itself permission
    /// for arbitrary commands (and to plant text into every future system
    /// prompt). These must be rejected even though they sit inside the
    /// working directory.
    #[test]
    fn runtime_state_paths_are_rejected() {
        use super::is_in_working_dir_for_write;
        let wd = wd_named("runtime");
        let openzen = std::path::Path::new(&wd).join("openzen");
        let hooks = std::path::Path::new(&wd).join(".git").join("hooks");
        let memory = std::path::Path::new(&wd).join("memory");
        for d in [&openzen, &hooks, &memory] {
            let _ = std::fs::create_dir_all(d);
        }
        // Would have been ALLOWED before the fix (all inside wd).
        // These are WRITE-fence assertions: reads into the runtime
        // directories are deliberately allowed now (see the read/write test).
        assert!(!is_in_working_dir_for_write(
            &openzen.join("trust.json").to_string_lossy(),
            &wd
        ));
        assert!(!is_in_working_dir_for_write(
            &hooks.join("pre-commit").to_string_lossy(),
            &wd
        ));
        assert!(!is_in_working_dir_for_write(
            &memory.join("global_mem.txt").to_string_lossy(),
            &wd
        ));
        // servers.toml is refused on BOTH sides (it can carry credentials).
        assert!(!is_in_working_dir(
            &std::path::Path::new(&wd)
                .join("servers.toml")
                .to_string_lossy(),
            &wd
        ));
        assert!(!is_in_working_dir_for_write(
            &std::path::Path::new(&wd)
                .join("servers.toml")
                .to_string_lossy(),
            &wd
        ));
        // A normal project file next to them stays allowed.
        let ok = std::path::Path::new(&wd).join("src").join("main.rs");
        let _ = std::fs::create_dir_all(ok.parent().unwrap());
        std::fs::write(&ok, "fn main() {}").unwrap();
        assert!(is_in_working_dir(&ok.to_string_lossy(), &wd));
        let _ = std::fs::remove_dir_all(&wd);
    }

    /// Review findings B-2 / D-4 / E-3: the write fence used to be applied to
    /// READS as well, which made the system prompt's own instruction
    /// ("read `{working_dir}/.openzen/reflections.jsonl` with the read tool")
    /// impossible to follow — the Reflexion failure log was unreadable and
    /// every turn burned a tool call on it.
    #[test]
    fn runtime_state_is_readable_but_not_writable() {
        use super::is_in_working_dir_for_write;
        let wd = wd_named("readwrite");
        let reflections = std::path::Path::new(&wd).join(".openzen");
        let _ = std::fs::create_dir_all(&reflections);
        let log = reflections.join("reflections.jsonl");
        std::fs::write(&log, "{\"lesson\":\"x\"}").unwrap();
        let log_s = log.to_string_lossy().to_string();
        // Read side: allowed (that is what the system prompt asks for).
        assert!(
            is_in_working_dir(&log_s, &wd),
            "the reflexion log must be readable"
        );
        // Write side: still refused — the agent must not rewrite its own
        // failure log or the trust store.
        assert!(
            !is_in_working_dir_for_write(&log_s, &wd),
            "runtime state must stay write-protected"
        );
        let trust = std::path::Path::new(&wd).join("openzen").join("trust.json");
        let _ = std::fs::create_dir_all(trust.parent().unwrap());
        std::fs::write(&trust, "{}").unwrap();
        assert!(!is_in_working_dir_for_write(&trust.to_string_lossy(), &wd));
        // `servers.toml` can hold MCP credentials → not even readable.
        let servers = std::path::Path::new(&wd).join("servers.toml");
        std::fs::write(&servers, "[mcp]").unwrap();
        assert!(!is_in_working_dir(&servers.to_string_lossy(), &wd));
        // Review finding D-6: the old substring rule rejected any path
        // containing "keychain" — including a project file named after it.
        let kc = std::path::Path::new(&wd).join("src").join("keychain.rs");
        let _ = std::fs::create_dir_all(kc.parent().unwrap());
        std::fs::write(&kc, "//! keychain").unwrap();
        assert!(
            is_in_working_dir(&kc.to_string_lossy(), &wd),
            "a source file named keychain.rs must not be treated as a credential"
        );
        let _ = std::fs::remove_dir_all(&wd);
    }
}
