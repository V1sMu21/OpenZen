//! Argument pattern builder — extracts safe matching keys from tool arguments.
//!
//! Each tool type has different rules for what constitutes "the same operation":
//!
//! | Tool | Pattern Rule | Example |
//! |------|-------------|---------|
//! | `code_run` | First command word | `echo`, `rm`, `npm` |
//! | `read`/`write`/`patch`/`edit` | First two path segments | `/tmp/`, `src/components/` |
//! | `web_scan` | Host domain | `example.com` |
//! | `web_js` | Wildcard `*` | All JS is treated the same |
//! | MCP tools | Full tool name | `mcp__playwright__screenshot` |
//! | Everything else | Tool name only | `long_term`, `skill_mcp_store` |

use serde_json::Value;

/// Commands whose approval must never be generalized.
///
/// Approving one invocation of an interpreter is approving *arbitrary code*:
/// after three "trust this session" clicks the old code inserted
/// `("code_run","python3")` into `session_trusts`, and from then on
/// `python3 -c "<anything>"` ran without a prompt for the rest of the
/// process — same for `sh`/`bash`/`node`/`osascript`. Network clients
/// (`curl`/`wget`/`nc`) turn one approval into an exfiltration channel, and
/// task runners that execute project-controlled content (`npx`, `uv`,
/// `docker`, `make`, `pytest`) turn "unapproved `write` + one approval" into
/// arbitrary code execution (review findings D-2/D-3).
///
/// `git`, `cargo`, `npm` and `yarn` are deliberately NOT listed: they are
/// approved many times a day, and asking every time is how users learn to
/// click through prompts. Their dangerous subcommands are covered by the
/// project-level permission policy instead — this is a documented residual
/// risk, not an oversight.
const NEVER_AUTO_TRUST: &[&str] = &[
    "sh",
    "bash",
    "zsh",
    "fish",
    "dash",
    "ksh",
    "csh",
    "tcsh",
    "env",
    "sudo",
    "doas",
    "su",
    "eval",
    "exec",
    "xargs",
    "python",
    "python2",
    "python3",
    "py",
    "node",
    "nodejs",
    "deno",
    "bun",
    "ruby",
    "perl",
    "php",
    "osascript",
    "open",
    "curl",
    "wget",
    "nc",
    "ncat",
    "netcat",
    "socat",
    "telnet",
    "ssh",
    "scp",
    "sftp",
    "rsync",
    "npx",
    "bunx",
    "tsx",
    "ts-node",
    "uv",
    "uvx",
    "pip",
    "pip3",
    "pipx",
    "poetry",
    "pdm",
    "hatch",
    "docker",
    "podman",
    "make",
    "just",
    "rake",
    "gradle",
    "mvn",
    "go",
    "pytest",
    "tox",
    "nodemon",
    "watchman",
    "esbuild",
    "swc",
];

/// Wrappers that execute the command that follows them. `nohup python3 x`
/// must be judged as `python3`, not as `nohup`.
const WRAPPERS: &[&str] = &[
    "nohup",
    "time",
    "xcrun",
    "arch",
    "stdbuf",
    "command",
    "builtin",
    "caffeinate",
    "ionice",
    "nice",
    "setsid",
    "watch",
    "timeout",
    "gtimeout",
    "taskset",
    "unbuffer",
    "script",
    "strace",
    "ltrace",
    "dtruss",
    "hyperfine",
];

/// Tools whose approval pattern carries no argument information at all, so
/// remembering one approval means remembering every future call. `web_js`
/// uses the literal wildcard `*`; MCP tools use their own name.
const NEVER_AUTO_TRUST_TOOLS: &[&str] = &["web_js", "web_execute_js"];

fn is_never_auto_trust_command(word: &str) -> bool {
    if NEVER_AUTO_TRUST.contains(&word) {
        return true;
    }
    // Versioned interpreters: python3.12, node20, ruby3.2 …
    const PREFIXES: &[&str] = &["python", "node", "ruby", "perl", "php", "go"];
    PREFIXES.iter().any(|pre| {
        word.starts_with(pre) && word[pre.len()..].starts_with(|c: char| c.is_ascii_digit())
    })
}

/// Basename of a token: `/usr/bin/python3` → `python3`.
fn basename(token: &str) -> &str {
    token.rsplit('/').next().unwrap_or(token)
}

/// May an approval for `(tool, pattern)` be remembered and auto-escalated?
///
/// `code_run`'s pattern is only the first command word, so it has to be
/// normalized before judging: `/usr/bin/python3`, `nohup python3` and
/// `env FOO=1 python3` are all `python3` (review finding D-2).
pub fn is_auto_trustable(tool: &str, pattern: &str) -> bool {
    let p = pattern.trim().to_lowercase();
    if p.is_empty() {
        return false;
    }
    // A wildcard pattern means "one approval covers every argument".
    if p == "*" {
        return false;
    }
    if NEVER_AUTO_TRUST_TOOLS.contains(&tool) || tool.starts_with("mcp__") {
        return false;
    }
    if tool != "code_run" {
        // Other tools carry a path / host / name key that is already
        // specific enough (read/write/edit/patch/web_scan/…).
        return true;
    }
    let tokens: Vec<&str> = p.split_whitespace().map(basename).collect();
    let Some(first) = tokens.first() else {
        return false;
    };
    // `sudo`, `env`, `xargs` … are both wrappers and never-trustable on their
    // own: approving them would approve whatever they are pointed at.
    if is_never_auto_trust_command(first) {
        return false;
    }
    // Unwrap `nohup`/`time`/`xcrun`-style prefixes, then any flags or
    // `VAR=value` assignments, and judge the command that actually runs.
    let mut i = 0;
    while i < tokens.len() && WRAPPERS.contains(&tokens[i]) {
        i += 1;
    }
    while i < tokens.len() && (tokens[i].starts_with('-') || tokens[i].contains('=')) {
        i += 1;
    }
    if let Some(cmd) = tokens.get(i) {
        if is_never_auto_trust_command(cmd) {
            return false;
        }
    }
    true
}

pub fn build_pattern(tool: &str, args: &Value) -> (String, String) {
    match tool {
        "code_run" => {
            let code = args.get("code").and_then(|v| v.as_str()).unwrap_or("");
            let first_word = code.split_whitespace().next().unwrap_or("unknown");
            (tool.to_string(), first_word.to_lowercase())
        }
        "read" | "write" | "patch" | "edit" => {
            let path = args.get("file_path").and_then(|v| v.as_str()).unwrap_or("");
            let segments: Vec<&str> = path
                .split('/')
                .filter(|s| !s.is_empty() && *s != ".")
                .collect();
            let key = if segments.len() <= 2 {
                if segments.is_empty() {
                    ".".to_string()
                } else {
                    segments.join("/")
                }
            } else {
                segments[..2].join("/")
            };
            (tool.to_string(), key)
        }
        "web_scan" => {
            let url = args.get("url").and_then(|v| v.as_str()).unwrap_or("");
            let host = url
                .split("://")
                .nth(1)
                .unwrap_or(url)
                .split('/')
                .next()
                .unwrap_or("unknown")
                .to_lowercase();
            (tool.to_string(), host)
        }
        "web_js" => (tool.to_string(), "*".to_string()),
        name if name.starts_with("mcp__") => (tool.to_string(), tool.to_string()),
        _ => (tool.to_string(), tool.to_string()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_code_run_pattern() {
        let (t, p) = build_pattern(
            "code_run",
            &serde_json::json!({"code": "npm install --save-dev typescript"}),
        );
        assert_eq!(t, "code_run");
        assert_eq!(p, "npm");
    }

    #[test]
    fn test_code_run_single_word() {
        let (_, p) = build_pattern("code_run", &serde_json::json!({"code": "ls"}));
        assert_eq!(p, "ls");
    }

    #[test]
    fn test_write_pattern_deep() {
        let (t, p) = build_pattern(
            "write",
            &serde_json::json!({"file_path": "/tmp/project/src/main.rs"}),
        );
        assert_eq!(t, "write");
        assert_eq!(p, "tmp/project");
    }

    #[test]
    fn test_write_pattern_shallow() {
        let (_, p) = build_pattern("write", &serde_json::json!({"file_path": "/tmp/test.txt"}));
        assert_eq!(p, "tmp/test.txt");
    }

    #[test]
    fn test_web_scan_pattern() {
        let (_, p) = build_pattern(
            "web_scan",
            &serde_json::json!({"url": "https://docs.rs/tokio/latest/tokio/"}),
        );
        assert_eq!(p, "docs.rs");
    }

    #[test]
    fn test_web_js_pattern() {
        let (_, p) = build_pattern("web_js", &serde_json::json!({"code": "document.title"}));
        assert_eq!(p, "*");
    }

    #[test]
    fn test_mcp_tool_pattern() {
        let (_, p) = build_pattern("mcp__playwright__screenshot", &serde_json::json!({}));
        assert_eq!(p, "mcp__playwright__screenshot");
    }

    #[test]
    fn test_other_tool_pattern() {
        let (_, p) = build_pattern("skill_mcp_store", &serde_json::json!({"name": "test"}));
        assert_eq!(p, "skill_mcp_store");
    }

    /// round5 P1-8a regression: three approvals of `python3 script.py` used
    /// to insert ("code_run","python3") into session_trusts, after which
    /// `python3 -c "<anything>"` ran unprompted for the rest of the process.
    ///
    /// Review findings D-2/D-3 widened this: path-qualified names, wrapper
    /// prefixes and project-content executors were all bypasses.
    #[test]
    fn interpreters_are_never_auto_trusted() {
        for cmd in [
            "python3",
            "python",
            "python3.12",
            "sh",
            "bash",
            "zsh",
            "node",
            "node20",
            "osascript",
            "curl",
            "wget",
            "nc",
            "sudo",
            "env",
            "ruby3.2",
            // path-qualified (review D-2)
            "/usr/bin/python3",
            "/bin/bash",
            "/opt/homebrew/bin/node",
            // wrapper-prefixed (review D-2)
            "nohup python3",
            "time python3 x.py",
            "xcrun python3 x.py",
            "env FOO=1 python3 x.py",
            "arch -arm64 python3 x.py",
            "stdbuf -o0 python3 x.py",
            // project-content executors (review D-3)
            "npx",
            "npx tsx evil.ts",
            "uv run python x.py",
            "docker",
            "make",
            "just",
            "pytest",
            "poetry",
            "go",
            "deno",
        ] {
            assert!(
                !is_auto_trustable("code_run", cmd),
                "`{cmd}` must never be auto-trusted"
            );
        }
        // Ordinary, specific commands still escalate normally.
        for cmd in [
            "cargo",
            "npm",
            "yarn",
            "pytest-html-report",
            "ls",
            "git",
            "rg",
        ] {
            assert!(
                is_auto_trustable("code_run", cmd),
                "`{cmd}` should escalate"
            );
        }
        // Non-code_run tools keep their (path/host-scoped) behaviour …
        assert!(is_auto_trustable("write", "src/components"));
        assert!(is_auto_trustable("web_scan", "docs.rs"));
        // … except the ones whose pattern carries no argument information.
        assert!(!is_auto_trustable("web_js", "*"));
        assert!(!is_auto_trustable("web_execute_js", "web_execute_js"));
        assert!(!is_auto_trustable(
            "mcp__playwright__browser_evaluate",
            "mcp__playwright__browser_evaluate"
        ));
        // Empty and wildcard patterns are never trustable.
        assert!(!is_auto_trustable("code_run", ""));
        assert!(!is_auto_trustable("read", "*"));
    }
}
