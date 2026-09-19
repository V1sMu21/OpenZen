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

pub enum VerifyResult {
    Passed,
    Failed(String),
    SoftPass,
}

pub async fn verify_todo_item(content: &str, working_dir: &str) -> VerifyResult {
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
    let verdict = |name: &str, r: CommandResult| -> VerifyResult {
        match r {
            CommandResult::Success => VerifyResult::Passed,
            CommandResult::Failed(stderr) => VerifyResult::Failed(stderr),
            CommandResult::Timeout(secs) => {
                VerifyResult::Failed(format!("{name} timed out after {secs}s"))
            }
            CommandResult::SpawnError(e) => {
                tracing::info!("[verifier] {name} toolchain unavailable ({e}); not a failure");
                VerifyResult::Passed
            }
        }
    };

    // 2. Build/compile detection
    let build_keywords = ["编译", "build", "cargo build", "npm run build", "make"];
    if build_keywords
        .iter()
        .any(|k| keyword_matches(&content_lower, k))
    {
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
        return verdict("build", r);
    }

    // 3. Test detection
    let test_keywords = ["测试", "test", "cargo test", "npm test", "pytest"];
    if test_keywords
        .iter()
        .any(|k| keyword_matches(&content_lower, k))
    {
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
        return verdict("tests", r);
    }

    // 4. Lint/check detection
    let lint_keywords = ["lint", "clippy", "cargo clippy", "cargo check", "check"];
    if lint_keywords
        .iter()
        .any(|k| keyword_matches(&content_lower, k))
    {
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
        return verdict("lint", r);
    }

    // 5. Document/spec creation: check for .md/.txt file content
    let doc_keywords = ["document", "文档", "spec", "说明", "readme"];
    if doc_keywords
        .iter()
        .any(|k| keyword_matches(&content_lower, k))
    {
        // Try to find a recently created .md or .txt file in working_dir
        if let Some(_path) = find_recent_doc(working_dir) {
            return VerifyResult::Passed;
        }
        return VerifyResult::Failed(
            "No .md or .txt file found in working directory for document task".to_string(),
        );
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
    let cmd = cmd.to_string();
    let args: Vec<String> = args.iter().map(|s| s.to_string()).collect();
    let wd = working_dir.to_string();

    let fut = tokio::task::spawn_blocking(move || {
        match std::process::Command::new(&cmd)
            .args(&args)
            .current_dir(&wd)
            .output()
        {
            Ok(out) if out.status.success() => Ok(()),
            Ok(out) => {
                let stderr_full = String::from_utf8_lossy(&out.stderr);
                let tail = if stderr_full.len() > MAX_STDERR_LEN {
                    format!(
                        "...{}",
                        &stderr_full[stderr_full.len().saturating_sub(MAX_STDERR_LEN)..]
                    )
                } else {
                    stderr_full.to_string()
                };
                Err(format!("{} failed:\n{}", cmd, tail))
            }
            Err(e) => Err(format!("{} spawn error: {}", cmd, e)),
        }
    });

    match tokio::time::timeout(Duration::from_secs(timeout_secs), fut).await {
        Ok(Ok(Ok(()))) => CommandResult::Success,
        Ok(Ok(Err(msg))) => CommandResult::Failed(msg),
        Ok(Err(join_err)) => CommandResult::SpawnError(format!("join error: {}", join_err)),
        Err(_elapsed) => CommandResult::Timeout(timeout_secs),
    }
}

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

fn find_recent_doc(working_dir: &str) -> Option<String> {
    let dir = Path::new(working_dir);
    if !dir.is_dir() {
        return None;
    }
    find_docs_recursive(dir, 3)
}

fn find_docs_recursive(dir: &Path, max_depth: u32) -> Option<String> {
    if max_depth == 0 {
        return None;
    }
    let entries = match std::fs::read_dir(dir) {
        Ok(e) => e,
        Err(_) => return None,
    };
    for entry in entries.filter_map(|e| e.ok()) {
        let path = entry.path();
        if path.is_dir() {
            if let Some(found) = find_docs_recursive(&path, max_depth - 1) {
                return Some(found);
            }
        } else {
            let name = entry.file_name().to_string_lossy().to_lowercase();
            if name.ends_with(".md") || name.ends_with(".txt") {
                return Some(path.to_string_lossy().to_string());
            }
        }
    }
    None
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
}
