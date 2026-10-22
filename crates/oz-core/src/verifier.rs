use std::path::Path;
use std::time::Duration;

/// Keyword match for assertion detection. ASCII keywords must match on
/// word boundaries — plain `contains` made "check" hit "checklist",
/// "test" hit "latest", and "build" hit "rebuild", sending unrelated
/// todos into cargo commands (whose failure then triggered spurious
/// rework). Keywords containing CJK stay substring matches (Chinese has
/// no word boundaries).
fn keyword_matches(content_lower: &str, keyword: &str) -> bool {
    let kw = keyword.to_lowercase();
    if !kw.is_ascii() {
        return content_lower.contains(&kw);
    }
    let bytes = content_lower.as_bytes();
    let kb = kw.as_bytes();
    if kb.is_empty() || kb.len() > bytes.len() {
        return false;
    }
    let is_word = |b: u8| b.is_ascii_alphanumeric() || b == b'_';
    let mut start = 0;
    while let Some(pos) = content_lower[start..].find(&kw) {
        let i = start + pos;
        let before_ok = i == 0 || !is_word(bytes[i - 1]);
        let after = i + kb.len();
        let after_ok = after >= bytes.len() || !is_word(bytes[after]);
        if before_ok && after_ok {
            return true;
        }
        start = i + 1;
        if start >= bytes.len() {
            break;
        }
    }
    false
}

/// Which build/test/lint toolchain a working directory carries. Drives
/// command selection so a JS project is not probed with cargo (whose
/// spawn failure used to be reported as a verification FAILURE).
#[derive(Debug, Clone, Copy, PartialEq)]
enum ProjectKind {
    Cargo,
    Node,
    Python,
    Unknown,
}

fn detect_project_kind(working_dir: &str) -> ProjectKind {
    let d = Path::new(working_dir);
    if d.join("Cargo.toml").exists() {
        ProjectKind::Cargo
    } else if d.join("package.json").exists() {
        ProjectKind::Node
    } else if d.join("pyproject.toml").exists()
        || d.join("requirements.txt").exists()
        || d.join("setup.py").exists()
    {
        ProjectKind::Python
    } else {
        ProjectKind::Unknown
    }
}

#[derive(Debug)]
pub enum VerifyResult {
    Passed,
    Failed(String),
    SoftPass,
}

/// Identifies one agent run for the heavy-command throttle below.
#[derive(Debug, Clone, Default)]
pub struct RunScope {
    /// Unique per run (session id + start time); empty disables the throttle.
    pub run_id: String,
    /// When the run started — used to tell "created by this run" from
    /// "already there before".
    pub started_at: Option<std::time::SystemTime>,
}

/// P2-23: one `cargo test`/`clippy` per run, keyed by (run, command).
/// Without it every completed todo re-ran the same 90-120s command.
static HEAVY_CMD_CACHE: std::sync::LazyLock<
    std::sync::Mutex<std::collections::HashMap<String, Result<(), String>>>,
> = std::sync::LazyLock::new(|| std::sync::Mutex::new(std::collections::HashMap::new()));

fn heavy_cmd_cached(run_id: &str, key: &str) -> Option<Result<(), String>> {
    if run_id.is_empty() {
        return None;
    }
    let cache = HEAVY_CMD_CACHE.lock().unwrap_or_else(|e| e.into_inner());
    cache.get(&format!("{run_id}|{key}")).cloned()
}

fn heavy_cmd_store(run_id: &str, key: &str, result: &Result<(), String>) {
    if run_id.is_empty() {
        return;
    }
    let mut cache = HEAVY_CMD_CACHE.lock().unwrap_or_else(|e| e.into_inner());
    // Bound the map: a long-lived process must not accumulate one entry per
    // run forever.
    if cache.len() > 256 {
        cache.clear();
    }
    cache.insert(format!("{run_id}|{key}"), result.clone());
}

pub async fn verify_todo_item(content: &str, working_dir: &str) -> VerifyResult {
    verify_todo_item_scoped(content, working_dir, &RunScope::default()).await
}

/// Run-aware variant (P2-23): knows when the run started and which run this
/// is, so document/asset checks can require "created by THIS run" and heavy
/// commands can be throttled per run.
pub async fn verify_todo_item_scoped(
    content: &str,
    working_dir: &str,
    scope: &RunScope,
) -> VerifyResult {
    // 1. File existence check: parse file path from todo content
    if let Some(path) = extract_file_path(content) {
        let full = Path::new(working_dir).join(&path);
        let direct = Path::new(&path);
        if full.exists() || direct.exists() {
            return VerifyResult::Passed;
        }
        // The todo content often lists a bare filename (e.g. "models.py")
        // while the agent actually created it in a nested project
        // subdirectory (e.g. <working_dir>/backend/models.py). Fall back
        // to a bounded recursive search by basename before declaring the
        // file missing — otherwise every completed todo is reverted to
        // in_progress and the checklist gate blocks agent exit forever
        // (agent loops on todoupdate → revert → retry indefinitely).
        let basename = Path::new(&path)
            .file_name()
            .and_then(|s| s.to_str())
            .unwrap_or(&path);
        // The recursive scan reads directories synchronously — run it on
        // the blocking pool so the agent loop thread is not stalled.
        let scan_dir = std::path::PathBuf::from(working_dir);
        let scan_name = basename.to_string();
        let found =
            tokio::task::spawn_blocking(move || file_exists_recursive(&scan_dir, &scan_name, 5))
                .await
                .unwrap_or(false);
        if found {
            return VerifyResult::Passed;
        }
        return VerifyResult::Failed(format!(
            "File does not exist: {} (checked {}, {} and recursively under {})",
            path,
            full.display(),
            direct.display(),
            working_dir
        ));
    }

    let content_lower = content.to_lowercase();
    let kind = detect_project_kind(working_dir);

    // Verdict for a probed command. A SpawnError means the toolchain for
    // this project is absent (e.g. cargo on a JS project) — that is NOT a
    // verification failure; fail-open with a log so the gate does not
    // send the agent into a rework loop over a missing binary.
    // P2-23: throttle the expensive probes — same command, same run, once.
    let verdict_cached = |name: &str, key: &str, r: CommandResult| -> VerifyResult {
        // Record the FRESH result, then answer from it. The old order asked
        // the cache first and let a stale entry overwrite the new outcome, so
        // (a) the command still ran every time — the throttle saved nothing —
        // and (b) a first failure stuck for the whole run: fixing the code and
        // re-verifying still returned Failed (review B6).
        let result = match r {
            CommandResult::Success => Ok(()),
            CommandResult::Failed(e) => Err(e),
            CommandResult::Timeout(secs) => Err(format!("{name} timed out after {secs}s")),
            CommandResult::SpawnError(e) => {
                tracing::info!("[verifier] {name} toolchain unavailable ({e}); not a failure");
                return VerifyResult::Passed;
            }
        };
        heavy_cmd_store(&scope.run_id, key, &result);
        match result {
            Ok(()) => VerifyResult::Passed,
            Err(e) => VerifyResult::Failed(e),
        }
    };

    // 2. Build/compile detection
    let build_keywords = ["编译", "build", "cargo build", "npm run build", "make"];
    if build_keywords
        .iter()
        .any(|k| keyword_matches(&content_lower, k))
    {
        if let Some(prev) = heavy_cmd_cached(&scope.run_id, "cargo-build") {
            tracing::info!("[verifier] build: reusing this run's earlier result");
            return match prev {
                Ok(()) => VerifyResult::Passed,
                Err(e) => VerifyResult::Failed(e),
            };
        }
        let wd = working_dir.to_string();
        let r = match kind {
            ProjectKind::Cargo => {
                run_command_with_timeout("cargo", &["build", "--quiet"], &wd, 60).await
            }
            ProjectKind::Node => {
                run_command_with_timeout("npm", &["run", "build", "--if-present"], &wd, 60).await
            }
            ProjectKind::Python | ProjectKind::Unknown => {
                tracing::info!("[verifier] build assertion with no recognized manifest; skipping");
                return VerifyResult::Passed;
            }
        };
        return verdict_cached("build", "cargo-build", r);
    }

    // 3. Test detection
    let test_keywords = ["测试", "test", "cargo test", "npm test", "pytest"];
    if test_keywords
        .iter()
        .any(|k| keyword_matches(&content_lower, k))
    {
        if let Some(prev) = heavy_cmd_cached(&scope.run_id, "tests") {
            tracing::info!("[verifier] tests: reusing this run's earlier result");
            return match prev {
                Ok(()) => VerifyResult::Passed,
                Err(e) => VerifyResult::Failed(e),
            };
        }
        let wd = working_dir.to_string();
        let r = match kind {
            ProjectKind::Cargo => {
                run_command_with_timeout("cargo", &["test", "--quiet"], &wd, 120).await
            }
            ProjectKind::Node => {
                run_command_with_timeout("npm", &["test", "--silent", "--if-present"], &wd, 120)
                    .await
            }
            ProjectKind::Python => run_command_with_timeout("pytest", &["-q"], &wd, 120).await,
            ProjectKind::Unknown => {
                tracing::info!("[verifier] test assertion with no recognized manifest; skipping");
                return VerifyResult::Passed;
            }
        };
        return verdict_cached("tests", "tests", r);
    }

    // 4. Lint/check detection
    let lint_keywords = ["lint", "clippy", "cargo clippy", "cargo check", "check"];
    if lint_keywords
        .iter()
        .any(|k| keyword_matches(&content_lower, k))
    {
        if let Some(prev) = heavy_cmd_cached(&scope.run_id, "lint") {
            tracing::info!("[verifier] lint: reusing this run's earlier result");
            return match prev {
                Ok(()) => VerifyResult::Passed,
                Err(e) => VerifyResult::Failed(e),
            };
        }
        let wd = working_dir.to_string();
        let r = match kind {
            ProjectKind::Cargo => {
                run_command_with_timeout(
                    "cargo",
                    &["clippy", "--quiet", "--", "-D", "warnings"],
                    &wd,
                    90,
                )
                .await
            }
            ProjectKind::Node => {
                run_command_with_timeout("npm", &["run", "lint", "--if-present"], &wd, 90).await
            }
            ProjectKind::Python => {
                run_command_with_timeout("ruff", &["check", "-q"], &wd, 90).await
            }
            ProjectKind::Unknown => {
                tracing::info!("[verifier] lint assertion with no recognized manifest; skipping");
                return VerifyResult::Passed;
            }
        };
        return verdict_cached("lint", "lint", r);
    }

    // 5. Document/spec creation: check for .md/.txt file content
    let doc_keywords = ["document", "文档", "spec", "说明", "readme"];
    if doc_keywords
        .iter()
        .any(|k| keyword_matches(&content_lower, k))
    {
        // P2-23: "any .md/.txt anywhere in the tree" passed even when the run
        // wrote nothing (an unrelated README counted). Require a file that
        // THIS run created AND that looks like a document: non-empty, a
        // heading, and a minimum body length.
        match find_doc_created_this_run(working_dir, scope.started_at) {
            DocCheck::Ok(path) => {
                tracing::info!("[verifier] document deliverable verified: {path}");
                return VerifyResult::Passed;
            }
            DocCheck::TooThin(path, why) => {
                return VerifyResult::Failed(format!(
                    "Document deliverable `{path}` is not usable: {why} (expected a non-empty \
                     document with a heading and at least 200 characters of body)"
                ));
            }
            DocCheck::NotFound => {
                return VerifyResult::Failed(
                    "No .md/.txt file created by this run was found in the working directory \
                     (pre-existing documents do not count)"
                        .to_string(),
                );
            }
        }
    }

    // 5b. Asset deliverables (P2-23): an image or HTML page must at least be
    // a real file of its type — a 0-byte or truncated PNG used to pass.
    let asset_keywords = [
        "素材", "图片", "sprite", "asset", "html", "页面", "png", "jpg",
    ];
    if asset_keywords
        .iter()
        .any(|k| keyword_matches(&content_lower, k))
    {
        if let Some((path, kind)) = find_asset_created_this_run(working_dir, scope.started_at) {
            if let Err(why) = check_asset_structure(&path, kind) {
                return VerifyResult::Failed(format!(
                    "Asset `{path}` failed its structural check: {why}"
                ));
            }
            tracing::info!("[verifier] asset deliverable verified: {path}");
            return VerifyResult::Passed;
        }
        // No asset found at all: fall through to the soft passes below so a
        // task that merely mentions "html" is not failed for it.
    }

    // 6. Code review / analysis: soft pass (can't verify subjective quality)
    let review_keywords = ["review", "审查", "analyze", "分析", "检查", "check"];
    if review_keywords
        .iter()
        .any(|k| content.to_lowercase().contains(k))
    {
        return VerifyResult::SoftPass;
    }

    VerifyResult::SoftPass
}

enum CommandResult {
    Success,
    Failed(String),
    Timeout(u64),
    SpawnError(String),
}

const MAX_STDERR_LEN: usize = 1024;

async fn run_command_with_timeout(
    cmd: &str,
    args: &[&str],
    working_dir: &str,
    timeout_secs: u64,
) -> CommandResult {
    // tokio::process with its own process group, exactly like
    // `code_run::run_child_with_timeout`: on timeout the whole TREE must die.
    // `spawn_blocking(Command::output())` cannot be cancelled — the timeout
    // only stopped *waiting*, so a timed-out `cargo test` kept a rustc tree
    // alive and, worse, kept holding the target-dir lock, which made the next
    // code_run build hang and time out too (round5 P1-9).
    let mut command = tokio::process::Command::new(cmd);
    command
        .args(args)
        .current_dir(working_dir)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .kill_on_drop(true);
    #[cfg(unix)]
    command.process_group(0);

    let child = match command.spawn() {
        Ok(c) => c,
        Err(e) => return CommandResult::SpawnError(format!("{cmd} spawn error: {e}")),
    };
    let pid = child.id();

    match tokio::time::timeout(Duration::from_secs(timeout_secs), child.wait_with_output()).await {
        Ok(Ok(out)) if out.status.success() => CommandResult::Success,
        Ok(Ok(out)) => {
            let stderr_full = String::from_utf8_lossy(&out.stderr);
            let tail = if stderr_full.len() > MAX_STDERR_LEN {
                format!(
                    "...{}",
                    &stderr_full[stderr_full.len().saturating_sub(MAX_STDERR_LEN)..]
                )
            } else {
                stderr_full.to_string()
            };
            CommandResult::Failed(format!("{cmd} failed:\n{tail}"))
        }
        Ok(Err(e)) => CommandResult::SpawnError(format!("{cmd} wait error: {e}")),
        Err(_elapsed) => {
            kill_verifier_tree(pid);
            CommandResult::Timeout(timeout_secs)
        }
    }
}

/// SIGKILL the verifier child's process group (best effort).
#[cfg(unix)]
fn kill_verifier_tree(pid: Option<u32>) {
    if let Some(pid) = pid {
        unsafe {
            // Negative pid = process group (the child ran with process_group(0)).
            libc::kill(-(pid as i32), libc::SIGKILL);
        }
    }
}

#[cfg(not(unix))]
fn kill_verifier_tree(_pid: Option<u32>) {}

/// File-path probe used on every todoupdate completion — compiled once.
static FILE_PATH_RE: std::sync::LazyLock<Option<regex::Regex>> = std::sync::LazyLock::new(|| {
    regex::Regex::new(
            r"[\w\-_./]+\.(rs|toml|json|yaml|yml|ts|tsx|js|jsx|py|go|java|cpp|c|h|hpp|css|html|md|txt|sh|sql|svg|png|jpg)",
        )
        .ok()
});

pub fn extract_file_path(text: &str) -> Option<String> {
    FILE_PATH_RE
        .as_ref()?
        .find(text)
        .map(|m| m.as_str().to_string())
}

/// Result of the "did this run deliver a usable document?" probe.
pub enum DocCheck {
    Ok(String),
    TooThin(String, String),
    NotFound,
}

/// Newest .md/.txt created at/after `started_at` that looks like a document.
fn find_doc_created_this_run(
    working_dir: &str,
    started_at: Option<std::time::SystemTime>,
) -> DocCheck {
    let dir = Path::new(working_dir);
    if !dir.is_dir() {
        return DocCheck::NotFound;
    }
    let mut candidates: Vec<(std::time::SystemTime, std::path::PathBuf)> = Vec::new();
    collect_docs(dir, 3, started_at, &mut candidates);
    candidates.sort_by_key(|(t, _)| std::cmp::Reverse(*t));
    for (_, path) in candidates {
        match std::fs::read_to_string(&path) {
            Ok(body) => {
                let trimmed = body.trim();
                if trimmed.is_empty() {
                    return DocCheck::TooThin(
                        path.to_string_lossy().to_string(),
                        "the file is empty".to_string(),
                    );
                }
                let has_heading = trimmed
                    .lines()
                    .any(|l| l.trim_start().starts_with('#') || l.trim_start().starts_with("<h"));
                if !has_heading {
                    return DocCheck::TooThin(
                        path.to_string_lossy().to_string(),
                        "no heading (# / <h1>) was found".to_string(),
                    );
                }
                if trimmed.chars().count() < 200 {
                    return DocCheck::TooThin(
                        path.to_string_lossy().to_string(),
                        format!("only {} characters of content", trimmed.chars().count()),
                    );
                }
                return DocCheck::Ok(path.to_string_lossy().to_string());
            }
            Err(_) => continue,
        }
    }
    DocCheck::NotFound
}

fn collect_docs(
    dir: &Path,
    max_depth: u32,
    started_at: Option<std::time::SystemTime>,
    out: &mut Vec<(std::time::SystemTime, std::path::PathBuf)>,
) {
    if max_depth == 0 {
        return;
    }
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.filter_map(|e| e.ok()) {
        let path = entry.path();
        let name = entry.file_name().to_string_lossy().to_lowercase();
        if path.is_dir() {
            if matches!(
                name.as_str(),
                "node_modules" | "target" | ".git" | "dist" | "build" | ".venv" | "__pycache__"
            ) {
                continue;
            }
            collect_docs(&path, max_depth - 1, started_at, out);
        } else if name.ends_with(".md") || name.ends_with(".txt") {
            let Ok(meta) = entry.metadata() else { continue };
            let Ok(mtime) = meta.modified() else { continue };
            // 2s slack: the file may be written just before the run is
            // recorded as started.
            let fresh = match started_at {
                Some(start) => mtime
                    .duration_since(start)
                    .map(|_| true)
                    .unwrap_or_else(|e| e.duration() < Duration::from_secs(2)),
                None => true,
            };
            if fresh {
                out.push((mtime, path));
            }
        }
    }
}

/// Asset kinds we can check mechanically.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum AssetKind {
    Png,
    Jpeg,
    Gif,
    Webp,
    Html,
}

fn find_asset_created_this_run(
    working_dir: &str,
    started_at: Option<std::time::SystemTime>,
) -> Option<(String, AssetKind)> {
    let dir = Path::new(working_dir);
    let mut best: Option<(std::time::SystemTime, std::path::PathBuf, AssetKind)> = None;
    collect_assets(dir, 3, started_at, &mut best);
    best.map(|(_, p, k)| (p.to_string_lossy().to_string(), k))
}

fn collect_assets(
    dir: &Path,
    max_depth: u32,
    started_at: Option<std::time::SystemTime>,
    best: &mut Option<(std::time::SystemTime, std::path::PathBuf, AssetKind)>,
) {
    if max_depth == 0 {
        return;
    }
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.filter_map(|e| e.ok()) {
        let path = entry.path();
        let name = entry.file_name().to_string_lossy().to_lowercase();
        if path.is_dir() {
            if matches!(
                name.as_str(),
                "node_modules" | "target" | ".git" | "dist" | "build" | ".venv" | "__pycache__"
            ) {
                continue;
            }
            collect_assets(&path, max_depth - 1, started_at, best);
            continue;
        }
        let kind = if name.ends_with(".png") {
            AssetKind::Png
        } else if name.ends_with(".jpg") || name.ends_with(".jpeg") {
            AssetKind::Jpeg
        } else if name.ends_with(".gif") {
            AssetKind::Gif
        } else if name.ends_with(".webp") {
            AssetKind::Webp
        } else if name.ends_with(".html") || name.ends_with(".htm") {
            AssetKind::Html
        } else {
            continue;
        };
        let Ok(meta) = entry.metadata() else { continue };
        let Ok(mtime) = meta.modified() else { continue };
        let fresh = match started_at {
            Some(start) => mtime
                .duration_since(start)
                .map(|_| true)
                .unwrap_or_else(|e| e.duration() < Duration::from_secs(2)),
            None => true,
        };
        if !fresh {
            continue;
        }
        if best.as_ref().map(|(t, _, _)| mtime > *t).unwrap_or(true) {
            *best = Some((mtime, path, kind));
        }
    }
}

/// Minimal mechanical validation: real file header / closing tag.
pub fn check_asset_structure(path: &str, kind: AssetKind) -> Result<(), String> {
    let bytes = std::fs::read(path).map_err(|e| format!("unreadable: {e}"))?;
    if bytes.is_empty() {
        return Err("the file is empty".to_string());
    }
    match kind {
        AssetKind::Png => {
            if !bytes.starts_with(&[0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A]) {
                return Err("missing the PNG magic bytes".to_string());
            }
        }
        AssetKind::Jpeg => {
            if !bytes.starts_with(&[0xFF, 0xD8, 0xFF]) {
                return Err("missing the JPEG magic bytes".to_string());
            }
        }
        AssetKind::Gif => {
            if !(bytes.starts_with(b"GIF87a") || bytes.starts_with(b"GIF89a")) {
                return Err("missing the GIF magic bytes".to_string());
            }
        }
        AssetKind::Webp => {
            if bytes.len() < 12 || &bytes[0..4] != b"RIFF" || &bytes[8..12] != b"WEBP" {
                return Err("missing the WEBP container header".to_string());
            }
        }
        AssetKind::Html => {
            let text = String::from_utf8_lossy(&bytes).to_lowercase();
            if !text.contains("</html>") {
                return Err("no closing </html> tag".to_string());
            }
            if !text.contains("<body") && !text.contains("<script") && !text.contains("<canvas") {
                return Err("no <body>/<script>/<canvas> content".to_string());
            }
        }
    }
    Ok(())
}

/// Bounded recursive search for a file whose basename equals `target`,
/// starting at `dir`. Skips common dependency/build directories so the
/// scan stays fast and does not false-positive on vendored copies.
fn file_exists_recursive(dir: &Path, target: &str, max_depth: u32) -> bool {
    if max_depth == 0 {
        return false;
    }
    let entries = match std::fs::read_dir(dir) {
        Ok(e) => e,
        Err(_) => return false,
    };
    for entry in entries.filter_map(|e| e.ok()) {
        let path = entry.path();
        if path.is_dir() {
            let name = path
                .file_name()
                .map(|n| n.to_string_lossy().to_lowercase())
                .unwrap_or_default();
            if matches!(
                name.as_str(),
                "node_modules"
                    | "target"
                    | ".git"
                    | "dist"
                    | "build"
                    | ".venv"
                    | "venv"
                    | "__pycache__"
            ) {
                continue;
            }
            if file_exists_recursive(&path, target, max_depth - 1) {
                return true;
            }
        } else if path.is_file()
            && path
                .file_name()
                .map(|n| n.to_string_lossy() == target)
                .unwrap_or(false)
        {
            return true;
        }
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_extract_rust_file() {
        assert_eq!(
            extract_file_path("创建文件 src/auth.rs"),
            Some("src/auth.rs".into())
        );
    }

    #[test]
    fn test_no_file_path() {
        assert_eq!(extract_file_path("理解现有代码结构"), None);
    }

    #[tokio::test]
    async fn test_verify_nested_file_exists() {
        let dir = std::env::temp_dir().join(format!("oz_verify_nested_{}", std::process::id()));
        let nested = dir.join("backend");
        std::fs::create_dir_all(&nested).unwrap();
        std::fs::write(nested.join("models.py"), "x = 1").unwrap();

        let result = verify_todo_item(
            "Phase 1: create backend (models.py, schemas.py)",
            dir.to_str().unwrap(),
        )
        .await;
        assert!(matches!(result, VerifyResult::Passed));

        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[tokio::test]
    async fn test_verify_missing_file_fails() {
        let dir = std::env::temp_dir().join(format!("oz_verify_missing_{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();

        let result = verify_todo_item("create missing.py", dir.to_str().unwrap()).await;
        assert!(matches!(result, VerifyResult::Failed(_)));

        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[tokio::test]
    async fn test_verify_skips_dependency_dirs() {
        let dir = std::env::temp_dir().join(format!("oz_verify_skips_{}", std::process::id()));
        let nested = dir.join("node_modules");
        std::fs::create_dir_all(&nested).unwrap();
        std::fs::write(nested.join("models.py"), "x = 1").unwrap();

        let result = verify_todo_item("create models.py", dir.to_str().unwrap()).await;
        assert!(matches!(result, VerifyResult::Failed(_)));

        std::fs::remove_dir_all(&dir).unwrap();
    }
}

#[cfg(test)]
mod keyword_boundary_tests {
    use super::*;

    #[test]
    fn ascii_keywords_need_word_boundaries() {
        assert!(!keyword_matches("update the checklist", "check"));
        assert!(keyword_matches("run cargo check please", "check"));
        assert!(!keyword_matches("the latest release", "test"));
        assert!(keyword_matches("run the test suite", "test"));
        assert!(!keyword_matches("rebuild the index", "build"));
        assert!(keyword_matches("build the app", "build"));
        assert!(!keyword_matches("inspect the file", "spec"));
        assert!(keyword_matches("write a spec", "spec"));
        assert!(keyword_matches("cargo check", "cargo check"));
    }

    #[test]
    fn cjk_keywords_stay_substring() {
        assert!(keyword_matches("完成编译与测试", "编译"));
        assert!(keyword_matches("补充文档说明", "文档"));
    }

    // ── P2-23 regression tests ──

    /// A pre-existing document must not satisfy a document task, and a
    /// 3-line stub must not either. Before the fix, ANY `.md`/`.txt` found
    /// in the tree passed — including an unrelated README that was already
    /// there.
    #[tokio::test]
    async fn document_deliverable_must_be_created_by_this_run_and_structured() {
        let dir = std::env::temp_dir().join(format!("oz-ver-doc-{}", std::process::id()));
        let _ = std::fs::create_dir_all(&dir);
        // Pre-existing README (mtime in the past) → not this run's work.
        let readme = dir.join("README.md");
        std::fs::write(&readme, "# old readme\n").unwrap();
        let future = std::time::SystemTime::now() + Duration::from_secs(3600);
        let scope = RunScope {
            run_id: "doc-test".into(),
            started_at: Some(future),
        };
        let r = verify_todo_item_scoped("写一份说明文档 document", &dir.to_string_lossy(), &scope)
            .await;
        assert!(
            matches!(r, VerifyResult::Failed(_)),
            "a pre-existing document must not pass"
        );

        // A stub created by this run → still failed (no heading / too short).
        std::fs::write(dir.join("notes.md"), "hi").unwrap();
        let scope2 = RunScope {
            run_id: "doc-test-2".into(),
            started_at: Some(std::time::SystemTime::now() - Duration::from_secs(5)),
        };
        let r2 =
            verify_todo_item_scoped("写一份说明文档 document", &dir.to_string_lossy(), &scope2)
                .await;
        assert!(
            matches!(r2, VerifyResult::Failed(_)),
            "a 2-character stub must not pass: {r2:?}"
        );

        // A real document → passed.
        let body = format!("# 设计说明\n\n{}\n", "内容段落。".repeat(60));
        std::fs::write(dir.join("design.md"), body).unwrap();
        let r3 =
            verify_todo_item_scoped("写一份说明文档 document", &dir.to_string_lossy(), &scope2)
                .await;
        assert!(
            matches!(r3, VerifyResult::Passed),
            "a structured document must pass: {r3:?}"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Asset deliverables get a mechanical structural check (magic bytes /
    /// closing tag). A 0-byte PNG used to pass silently.
    #[tokio::test]
    async fn asset_deliverables_get_a_structural_check() {
        let dir = std::env::temp_dir().join(format!("oz-ver-asset-{}", std::process::id()));
        let _ = std::fs::create_dir_all(&dir);
        let scope = RunScope {
            run_id: "asset-test".into(),
            started_at: Some(std::time::SystemTime::now() - Duration::from_secs(5)),
        };
        // Broken PNG.
        std::fs::write(dir.join("sprite.png"), b"not really a png").unwrap();
        let r = verify_todo_item_scoped("生成游戏素材 sprite 图片", &dir.to_string_lossy(), &scope)
            .await;
        assert!(
            matches!(r, VerifyResult::Failed(_)),
            "a fake PNG must fail its structural check: {r:?}"
        );
        // Real PNG header.
        let mut png = vec![0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A];
        png.extend_from_slice(&[0u8; 64]);
        std::fs::write(dir.join("sprite2.png"), &png).unwrap();
        let r2 =
            verify_todo_item_scoped("生成游戏素材 sprite 图片", &dir.to_string_lossy(), &scope)
                .await;
        assert!(matches!(r2, VerifyResult::Passed), "valid PNG: {r2:?}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn asset_structure_checks_magic_bytes() {
        let dir = std::env::temp_dir().join(format!("oz-ver-magic-{}", std::process::id()));
        let _ = std::fs::create_dir_all(&dir);
        let html = dir.join("index.html");
        std::fs::write(&html, "<html><body>hi</body></html>").unwrap();
        assert!(check_asset_structure(&html.to_string_lossy(), AssetKind::Html).is_ok());
        std::fs::write(&html, "<html><body>truncated").unwrap();
        assert!(check_asset_structure(&html.to_string_lossy(), AssetKind::Html).is_err());
        let empty = dir.join("empty.png");
        std::fs::write(&empty, b"").unwrap();
        assert!(check_asset_structure(&empty.to_string_lossy(), AssetKind::Png).is_err());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
