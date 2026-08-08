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

pub fn build_pattern(tool: &str, args: &Value) -> (String, String) {
    match tool {
        "code_run" => {
            let code = args.get("code").and_then(|v| v.as_str()).unwrap_or("");
            let first_word = code.split_whitespace().next().unwrap_or("unknown");
            (tool.to_string(), first_word.to_lowercase())
        }
        "read" | "write" | "patch" | "edit" => {
            let path = args.get("file_path").and_then(|v| v.as_str()).unwrap_or("");
            let segments: Vec<&str> = path.split('/')
                .filter(|s| !s.is_empty() && *s != ".")
                .collect();
            let key = if segments.len() <= 2 {
                if segments.is_empty() { ".".to_string() } else { segments.join("/") }
            } else {
                segments[..2].join("/")
            };
            (tool.to_string(), key)
        }
        "web_scan" => {
            let url = args.get("url").and_then(|v| v.as_str()).unwrap_or("");
            let host = url.split("://")
                .nth(1)
                .unwrap_or(url)
                .split('/')
                .next()
                .unwrap_or("unknown")
                .to_lowercase();
            (tool.to_string(), host)
        }
        "web_js" => {
            (tool.to_string(), "*".to_string())
        }
        name if name.starts_with("mcp__") => {
            (tool.to_string(), tool.to_string())
        }
        _ => {
            (tool.to_string(), tool.to_string())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_code_run_pattern() {
        let (t, p) = build_pattern("code_run", &serde_json::json!({"code": "npm install --save-dev typescript"}));
        assert_eq!(t, "code_run");
        assert_eq!(p, "npm");
    }

    #[test]
    fn test_code_run_single_word() {
        let (t, p) = build_pattern("code_run", &serde_json::json!({"code": "ls"}));
        assert_eq!(p, "ls");
    }

    #[test]
    fn test_write_pattern_deep() {
        let (t, p) = build_pattern("write", &serde_json::json!({"file_path": "/tmp/project/src/main.rs"}));
        assert_eq!(t, "write");
        assert_eq!(p, "tmp/project");
    }

    #[test]
    fn test_write_pattern_shallow() {
        let (t, p) = build_pattern("write", &serde_json::json!({"file_path": "/tmp/test.txt"}));
        assert_eq!(p, "tmp/test.txt");
    }

    #[test]
    fn test_web_scan_pattern() {
        let (t, p) = build_pattern("web_scan", &serde_json::json!({"url": "https://docs.rs/tokio/latest/tokio/"}));
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
}
