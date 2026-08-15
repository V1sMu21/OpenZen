use std::path::Path;
use std::time::Duration;

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

    // 2. Build/compile detection
    let build_keywords = ["编译", "build", "cargo build", "npm run build", "make"];
    if build_keywords
        .iter()
        .any(|k| content.to_lowercase().contains(k))
    {
        let wd = working_dir.to_string();
        return match run_command_with_timeout("cargo", &["build", "--quiet"], &wd, 60).await {
            CommandResult::Success => VerifyResult::Passed,
            CommandResult::Failed(stderr) => VerifyResult::Failed(stderr),
            CommandResult::Timeout(secs) => {
                VerifyResult::Failed(format!("Build timed out after {}s", secs))
            }
            CommandResult::SpawnError(e) => {
                VerifyResult::Failed(format!("Cannot run cargo build: {}", e))
            }
        };
    }

    // 3. Test detection
    let test_keywords = ["测试", "test", "cargo test", "npm test", "pytest"];
    if test_keywords
        .iter()
        .any(|k| content.to_lowercase().contains(k))
    {
        let wd = working_dir.to_string();
        return match run_command_with_timeout("cargo", &["test", "--quiet"], &wd, 120).await {
            CommandResult::Success => VerifyResult::Passed,
            CommandResult::Failed(stderr) => VerifyResult::Failed(stderr),
            CommandResult::Timeout(secs) => {
                VerifyResult::Failed(format!("Tests timed out after {}s", secs))
            }
            CommandResult::SpawnError(e) => {
                VerifyResult::Failed(format!("Cannot run cargo test: {}", e))
            }
        };
    }

    // 4. Lint/check detection
    let lint_keywords = ["lint", "clippy", "cargo clippy", "cargo check", "check"];
    if lint_keywords
        .iter()
        .any(|k| content.to_lowercase().contains(k))
    {
        let wd = working_dir.to_string();
        return match run_command_with_timeout(
            "cargo",
            &["clippy", "--quiet", "--", "-D", "warnings"],
            &wd,
            90,
        )
        .await
        {
            CommandResult::Success => VerifyResult::Passed,
            CommandResult::Failed(stderr) => VerifyResult::Failed(stderr),
            CommandResult::Timeout(secs) => {
                VerifyResult::Failed(format!("Clippy timed out after {}s", secs))
            }
            CommandResult::SpawnError(e) => {
                VerifyResult::Failed(format!("Cannot run cargo clippy: {}", e))
            }
        };
    }

    // 5. Document/spec creation: check for .md/.txt file content
    let doc_keywords = ["document", "文档", "spec", "说明", "readme"];
    if doc_keywords
        .iter()
        .any(|k| content.to_lowercase().contains(k))
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

pub fn extract_file_path(text: &str) -> Option<String> {
    let re = regex::Regex::new(
        r"[\w\-_./]+\.(rs|toml|json|yaml|yml|ts|tsx|js|jsx|py|go|java|cpp|c|h|hpp|css|html|md|txt|sh|sql|svg|png|jpg)"
    ).ok()?;
    re.find(text).map(|m| m.as_str().to_string())
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
