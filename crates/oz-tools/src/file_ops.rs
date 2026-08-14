use std::collections::HashMap;
use std::path::Path;

use async_trait::async_trait;
use oz_core_types::{ImageRef, ToolContext, ToolDefinition, ToolError, ToolFunction, ToolOutput};

use crate::registry::ToolHandler;

/// Maximum file size for read operations (10 MB).
const MAX_READ_SIZE: u64 = 10 * 1024 * 1024;

/// Paths that are always blocked regardless of working_dir.
const SENSITIVE_PATHS: &[&str] = &[
    "/etc/", "/usr/", "/bin/", "/sbin/", "/var/", "/boot/",
    "/dev/", "/proc/", "/sys/", "/root/",
    ".ssh/", ".aws/", ".gnupg/", ".docker/",
    "mykey.toml", ".env", "credentials", "id_rsa",
];

pub(crate) fn is_in_working_dir(path: &str, working_dir: &str) -> bool {
    let wd = Path::new(working_dir);
    let p = Path::new(path);

    // Always allow /tmp
    if p.starts_with("/tmp") || p.starts_with("/var/tmp") || p.starts_with("/var/folders") {
        return true;
    }

    // Check for sensitive paths
    let lower = path.to_lowercase();
    for sensitive in SENSITIVE_PATHS {
        if lower.contains(&sensitive.to_lowercase()) {
            return false;
        }
    }

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

    let real_p = std::fs::canonicalize(&resolved).ok()
        .or_else(|| resolved.parent().and_then(|parent| std::fs::canonicalize(parent).ok()));
    let real_wd = std::fs::canonicalize(wd).ok();

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
    fn name(&self) -> String { "read".to_string() }
    fn description(&self) -> String { "Read a file (txt, pdf, docx, pptx, xlsx) with optional line range. Do NOT re-read files you just wrote/edited — failures report themselves and the harness tracks file state. Use start/count for long files.".to_string() }
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

    async fn execute(&self, args: serde_json::Value, ctx: &ToolContext) -> Result<ToolOutput, ToolError> {
        let path = args["file_path"].as_str().unwrap_or("");
        if path.is_empty() {
            return Ok(ToolOutput::bad_json("read: missing file_path"));
        }
        if let Ok(meta) = tokio::fs::metadata(path).await {
            if meta.len() > MAX_READ_SIZE {
                return Ok(ToolOutput::bad_json(
                    format!("read: file too large ({} bytes, max {})", meta.len(), MAX_READ_SIZE)
                ));
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
    fn name(&self) -> String { "write".to_string() }
    fn description(&self) -> String { "Write content to a file (creates parent dirs). No confirmation re-read after writing — failures report themselves.".to_string() }
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

    async fn execute(&self, args: serde_json::Value, ctx: &ToolContext) -> Result<ToolOutput, ToolError> {
        let path = args["file_path"].as_str().unwrap_or("");
        let content = args["content"].as_str().unwrap_or("");
        if path.is_empty() {
            return Ok(ToolOutput::bad_json("write: missing file_path"));
        }
        if !is_in_working_dir(path, &ctx.working_dir) {
            let hint = if !Path::new(path).is_absolute() {
                let suggested = Path::new(&ctx.working_dir).join(path);
                format!(" Try the absolute path `{}` instead.", suggested.display())
            } else {
                String::new()
            };
            return Ok(ToolOutput::bad_json(
                format!("write: path `{path}` is outside working directory `{}` or in a protected location.{}", ctx.working_dir, hint)
            ));
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
    fn name(&self) -> String { "patch".to_string() }
    fn description(&self) -> String { "Apply a search-and-replace edit to a file. No confirmation re-read after editing — failures report themselves.".to_string() }
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

    async fn execute(&self, args: serde_json::Value, ctx: &ToolContext) -> Result<ToolOutput, ToolError> {
        let path = args["file_path"].as_str().unwrap_or("");
        let old = args["old_string"].as_str().unwrap_or("");
        let new = args["new_string"].as_str().unwrap_or("");
        if path.is_empty() || old.is_empty() {
            return Ok(ToolOutput::bad_json("patch: missing file_path or old_string"));
        }
        if !is_in_working_dir(path, &ctx.working_dir) {
            let hint = if !Path::new(path).is_absolute() {
                let suggested = Path::new(&ctx.working_dir).join(path);
                format!(" Try the absolute path `{}` instead.", suggested.display())
            } else {
                String::new()
            };
            return Ok(ToolOutput::bad_json(
                format!("patch: path `{path}` is outside working directory or in a protected location.{}", hint)
            ));
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
            return Ok(ToolOutput::bad_json(
                format!("patch: old_string appears {count} times — must be unique"),
            ));
        }

        let new_content = content.replace(old, new);
        match tokio::fs::write(path, &new_content).await {
            Ok(()) => Ok(ToolOutput::success(serde_json::json!({"status": "patched"}))),
            Err(e) => Ok(ToolOutput::bad_json(format!("patch write failed: {e}"))),
        }
    }
}

pub struct FileEditTool;

#[async_trait]
impl ToolHandler for FileEditTool {
    fn name(&self) -> String { "edit".to_string() }
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

    async fn execute(&self, args: serde_json::Value, ctx: &ToolContext) -> Result<ToolOutput, ToolError> {
        let path = args["file_path"].as_str().unwrap_or("");
        let old = args["old_string"].as_str().unwrap_or("");
        let new = args["new_string"].as_str().unwrap_or("");
        if path.is_empty() || old.is_empty() {
            return Ok(ToolOutput::bad_json("edit: missing file_path or old_string"));
        }
        if !is_in_working_dir(path, &ctx.working_dir) {
            let hint = if !Path::new(path).is_absolute() {
                let suggested = Path::new(&ctx.working_dir).join(path);
                format!(" Try the absolute path `{}` instead.", suggested.display())
            } else {
                String::new()
            };
            return Ok(ToolOutput::bad_json(
                format!("edit: path `{path}` is outside working directory or in a protected location.{}", hint)
            ));
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
            return Ok(ToolOutput::bad_json(
                format!("edit: old_string appears {count} times — must be unique"),
            ));
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
    fn name(&self) -> String { "glob".to_string() }
    fn description(&self) -> String { "List files matching a glob pattern. Use this tool for search — do not write python scripts to read large directories.".to_string() }
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

    async fn execute(&self, args: serde_json::Value, _ctx: &ToolContext) -> Result<ToolOutput, ToolError> {
        let pattern = args["pattern"].as_str().unwrap_or("");
        if pattern.is_empty() {
            return Ok(ToolOutput::bad_json("glob: missing pattern"));
        }
        let base = args.get("path").and_then(|v| v.as_str()).unwrap_or(".");
        let glob_pattern = format!("{}/{}", base.trim_end_matches('/'), pattern);

        let mut results = Vec::new();
        if let Ok(entries) = glob::glob(&glob_pattern) {
            for entry in entries.flatten() {
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
    fn name(&self) -> String { "grep".to_string() }
    fn description(&self) -> String { "Search file contents with a regex pattern. Use this tool for search — do not write python scripts to scan large files.".to_string() }
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

    async fn execute(&self, args: serde_json::Value, _ctx: &ToolContext) -> Result<ToolOutput, ToolError> {
        let pattern = args["pattern"].as_str().unwrap_or("");
        if pattern.is_empty() {
            return Ok(ToolOutput::bad_json("grep: missing pattern"));
        }
        let base = args.get("path").and_then(|v| v.as_str()).unwrap_or(".");
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
                if entry_path.is_dir() { continue; }
                if let Some(inc) = include {
                    let fname = entry_path.file_name().unwrap_or_default().to_string_lossy();
                    if !glob::Pattern::new(inc).map(|p| p.matches(&fname)).unwrap_or(false) {
                        continue;
                    }
                }
                if let Ok(content) = tokio::fs::read_to_string(&entry_path).await {
                    for (lineno, line) in content.lines().enumerate() {
                        if re.is_match(line) {
                            let mut m = HashMap::new();
                            m.insert("file".into(), serde_json::json!(entry_path.to_string_lossy().to_string()));
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
                        m.insert("file".into(), serde_json::json!(walk_path.to_string_lossy().to_string()));
                        m.insert("line".into(), serde_json::json!(lineno + 1));
                        m.insert("text".into(), serde_json::json!(line.to_string()));
                        results.push(m);
                    }
                }
            }
        }

        Ok(ToolOutput::success(serde_json::json!({"matches": results, "count": results.len()})))
    }
}

// ── LsTool ──

pub struct LsTool;

#[async_trait]
impl ToolHandler for LsTool {
    fn name(&self) -> String { "ls".to_string() }
    fn description(&self) -> String { "List directory contents. Use this tool for directory exploration — not python scripts.".to_string() }
    fn parameters(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "path": { "type": "string", "description": "Directory path (default .)" }
            },
            "required": []
        })
    }

    async fn execute(&self, args: serde_json::Value, _ctx: &ToolContext) -> Result<ToolOutput, ToolError> {
        let path = args.get("path").and_then(|v| v.as_str()).unwrap_or(".");
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

pub fn read_definition() -> ToolDefinition { def_for(&FileReadTool) }
pub fn write_definition() -> ToolDefinition { def_for(&FileWriteTool) }
pub fn edit_definition() -> ToolDefinition { def_for(&FileEditTool) }
pub fn patch_definition() -> ToolDefinition { def_for(&FilePatchTool) }
pub fn glob_definition() -> ToolDefinition { def_for(&GlobTool) }
pub fn grep_definition() -> ToolDefinition { def_for(&GrepTool) }
pub fn ls_definition() -> ToolDefinition { def_for(&LsTool) }

fn def_for(t: &dyn ToolHandler) -> ToolDefinition {
    ToolDefinition {
        type_: "function".into(),
        function: ToolFunction {
            name: t.name().into(),
            description: t.description().into(),
            parameters: t.parameters(),
        },
    }
}

// Old-style handlers (blocking, for backward compat)
use std::sync::Arc;
use oz_core_types::StepOutcome;

macro_rules! compat_handler {
    ($tool:expr) => {{
        let t = Arc::new($tool);
        Arc::new(move |_name: &str, args: &serde_json::Value, ctx: &oz_core_types::ToolContext| {
            let args = args.clone();
            let ctx = ctx.clone();
            let t = t.clone();
            let rt = tokio::runtime::Runtime::new().expect("Failed to create tokio runtime");
            let result = rt.block_on(t.execute(args, &ctx))
                .unwrap_or_else(|e| ToolOutput::bad_json(e.to_string()));
            StepOutcome { data: result.data, next_prompt: result.next_prompt, should_exit: result.should_exit, images: result.images }
        }) as super::ToolHandler
    }};
}

pub fn read_handler() -> super::ToolHandler { compat_handler!(FileReadTool) }
pub fn write_handler() -> super::ToolHandler { compat_handler!(FileWriteTool) }
pub fn edit_handler() -> super::ToolHandler { compat_handler!(FileEditTool) }
pub fn patch_handler() -> super::ToolHandler { compat_handler!(FilePatchTool) }
pub fn glob_handler() -> super::ToolHandler { compat_handler!(GlobTool) }
pub fn grep_handler() -> super::ToolHandler { compat_handler!(GrepTool) }
pub fn ls_handler() -> super::ToolHandler { compat_handler!(LsTool) }

#[cfg(test)]
mod tests {
    use super::*;
    use oz_core_types::ToolContext as TC;

    fn ctx() -> TC {
        TC { working_dir: "/tmp".into(), assets_dir: "/tmp".into(), script_dir: "/tmp".into(), lang: "en".into(), skill_mcp_dir: None, session_id: String::new() }
    }

    #[tokio::test]
    async fn test_read_missing_path() {
        let r = FileReadTool.execute(serde_json::json!({}), &ctx()).await.unwrap();
        assert!(r.next_prompt.unwrap().contains("missing"));
    }

    #[tokio::test]
    async fn test_read_nonexistent_file() {
        let r = FileReadTool
            .execute(serde_json::json!({"file_path": "/tmp/nonexistent_ga_test_xxx"}), &ctx())
            .await
            .unwrap();
        assert!(r.next_prompt.unwrap().contains("failed"));
    }

    #[tokio::test]
    async fn test_write_and_read_roundtrip() {
        let tmp = std::env::temp_dir().join("oz_test_write_read.txt");
        let path = tmp.to_string_lossy().to_string();

        let w = FileWriteTool
            .execute(serde_json::json!({"file_path": path, "content": "hello world\nline2"}), &ctx())
            .await
            .unwrap();
        assert_eq!(w.data["status"], "written");

        let r = FileReadTool
            .execute(serde_json::json!({"file_path": path}), &ctx())
            .await
            .unwrap();
        assert!(r.data["content"].as_str().unwrap_or("").contains("hello world"));

        let _ = std::fs::remove_file(&tmp);
    }

    #[tokio::test]
    async fn test_patch_basic() {
        let tmp = std::env::temp_dir().join("oz_test_patch.txt");
        let path = tmp.to_string_lossy().to_string();
        tokio::fs::write(&path, "hello world\n").await.unwrap();

        let r = FilePatchTool
            .execute(serde_json::json!({"file_path": path, "old_string": "hello", "new_string": "hi"}), &ctx())
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
        let r = GlobTool.execute(serde_json::json!({}), &ctx()).await.unwrap();
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