//! Lightweight diagnostics collection (P2-8).
//!
//! Runs project-native checkers (`cargo check`, `tsc --noEmit`, `clangd`
//! `--check`) with a short timeout and summarizes errors/warnings as a
//! compact block injected into the `<system-reminder>` — so the model sees
//! compile issues without spending tool calls probing the codebase.
//!
//! Design note: deliberately NOT a full LSP client (tower-lsp). The plan
//! gates that behind an independent dependency review; a CLI-based checker
//! delivers the same "diagnostics as context" value with zero new deps.

use std::path::Path;
use std::process::Stdio;
use std::time::Duration;

const CHECK_TIMEOUT: Duration = Duration::from_secs(15);
const MAX_DIAGNOSTICS: usize = 20;

/// Collect diagnostics for `working_dir` and render them as a reminder block.
/// Returns an empty string when nothing useful can be produced.
pub async fn collect_diagnostics_block(working_dir: &str) -> String {
    let dir = Path::new(working_dir);
    let (checker, args) = if dir.join("Cargo.toml").exists() {
        ("cargo", vec!["check", "--message-format=short", "--quiet"])
    } else if dir.join("package.json").exists() {
        ("npx", vec!["tsc", "--noEmit"])
    } else {
        return String::new();
    };

    let output = match run_checker(checker, &args, dir).await {
        Some(o) => o,
        None => return String::new(),
    };
    let text = String::from_utf8_lossy(&output).to_string();
    if text.trim().is_empty() {
        return String::new();
    }

    let diagnostics = parse_diagnostics(&text);
    if diagnostics.is_empty() {
        return String::new();
    }
    let errors = diagnostics.iter().filter(|d| d.severity == "error").count();
    let warnings = diagnostics.len() - errors;
    let mut block = format!("<diagnostics>\n{errors} error(s), {warnings} warning(s):\n");
    for d in diagnostics.iter().take(MAX_DIAGNOSTICS) {
        block.push_str(&format!(
            "- {}:{}:{} [{}] {}\n",
            d.file, d.line, d.col, d.severity, d.message
        ));
    }
    if diagnostics.len() > MAX_DIAGNOSTICS {
        block.push_str(&format!(
            "… and {} more\n",
            diagnostics.len() - MAX_DIAGNOSTICS
        ));
    }
    block.push_str("</diagnostics>");
    block
}

struct Diagnostic {
    file: String,
    line: String,
    col: String,
    severity: String,
    message: String,
}

fn parse_diagnostics(text: &str) -> Vec<Diagnostic> {
    let mut out = Vec::new();
    for line in text.lines() {
        let rest = line.strip_suffix('\r').unwrap_or(line);
        // "path:line:col: severity: message"
        let mut parts = rest.splitn(5, ':');
        let (Some(file), Some(line_no), Some(col), Some(severity)) =
            (parts.next(), parts.next(), parts.next(), parts.next())
        else {
            continue;
        };
        let message = parts.next().unwrap_or("").trim();
        if message.is_empty() {
            continue;
        }
        let sev_raw = severity.trim();
        let sev = sev_raw
            .split('[')
            .next()
            .unwrap_or(sev_raw)
            .trim()
            .to_string();
        if sev != "error" && sev != "warning" {
            continue;
        }
        out.push(Diagnostic {
            file: file.trim().to_string(),
            line: line_no.trim().to_string(),
            col: col.trim().to_string(),
            severity: sev,
            message: message.to_string(),
        });
    }
    out
}

async fn run_checker(program: &str, args: &[&str], dir: &Path) -> Option<Vec<u8>> {
    match tokio::time::timeout(
        CHECK_TIMEOUT,
        tokio::process::Command::new(program)
            .args(args)
            .current_dir(dir)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .output(),
    )
    .await
    {
        // cargo/tsc emit diagnostics on stderr; merge both streams.
        Ok(Ok(out)) => {
            let mut buf = out.stdout;
            buf.extend_from_slice(&out.stderr);
            Some(buf)
        }
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_diagnostics_cargo_short() {
        let text = "\
crates/oz-config/src/crypto.rs:7:5: warning: unused import: `std::io::Write`
crates/oz-core/src/agent_loop.rs:548:17: error[E0308]: mismatched types
warning: `oz-mcp` (lib) generated 1 warning
";
        let diags = parse_diagnostics(text);
        assert_eq!(diags.len(), 2);
        assert_eq!(diags[0].severity, "warning");
        assert_eq!(diags[0].file, "crates/oz-config/src/crypto.rs");
        assert_eq!(diags[0].message, "unused import: `std::io::Write`");
        assert_eq!(diags[1].severity, "error");
        assert!(diags[1].message.starts_with("mismatched types"));
    }

    #[test]
    fn test_parse_diagnostics_filters_noise() {
        let text = "warning: `oz-mcp` (lib) generated 1 warning\n  = note: something\ncrates/a.rs:1:1: error: boom\n";
        let diags = parse_diagnostics(text);
        assert_eq!(diags.len(), 1);
        assert_eq!(diags[0].message, "boom");
    }

    #[test]
    fn test_parse_diagnostics_empty() {
        assert!(parse_diagnostics("").is_empty());
        assert!(parse_diagnostics("checking foo\n  checked bar\n").is_empty());
    }

    #[tokio::test]
    async fn test_collect_block_non_project_dir_is_empty() {
        let dir = std::env::temp_dir().join("oz-diag-non-project");
        std::fs::create_dir_all(&dir).unwrap();
        assert_eq!(collect_diagnostics_block(dir.to_str().unwrap()).await, "");
    }

    #[tokio::test]
    async fn test_collect_block_renders_in_project() {
        // A tiny cargo project with one deliberate compile error.
        let dir = std::env::temp_dir().join("oz-diag-fixture");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("src")).unwrap();
        std::fs::write(
            dir.join("Cargo.toml"),
            "[package]\nname = \"diag-fixture\"\nversion = \"0.1.0\"\nedition = \"2021\"\n",
        )
        .unwrap();
        std::fs::write(
            dir.join("src/main.rs"),
            "fn main() { let x: i32 = \"oops\"; }\n",
        )
        .unwrap();
        let block = collect_diagnostics_block(dir.to_str().unwrap()).await;
        assert!(block.starts_with("<diagnostics>"), "got: {block}");
        assert!(block.contains("error"), "got: {block}");
        assert!(block.ends_with("</diagnostics>"));
    }
}
