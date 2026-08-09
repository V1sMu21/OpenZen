//! Declarative permission policy — loaded from `~/.openzen/permissions.toml`.
//!
//! Rules are matched by tool name + argument pattern, with `Deny` taking
//! priority over `Allow`. Anything unmatched falls back to `Ask`, which the
//! caller resolves through the progressive trust store / ask_user flow.

use std::path::Path;

use serde::{Deserialize, Serialize};
use serde_json::Value;

/// Map tool call args to the string permission patterns are matched against.
/// Field selection mirrors `patterns::build_pattern` so `[[rules]]` patterns
/// read naturally (`pattern = "rm -rf *"` matches the `code` field of
/// `code_run`). Unknown tools fall back to the compact JSON of args.
pub fn match_string(tool: &str, args: &Value) -> String {
    match tool {
        "code_run" => args
            .get("code")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string(),
        "read" | "write" | "patch" | "edit" => args
            .get("file_path")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string(),
        "web_scan" | "web_fetch" => args
            .get("url")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string(),
        _ => args.to_string(),
    }
}

/// Policy decision for a matched rule.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Decision {
    /// Execute without asking.
    Allow,
    /// Block without asking.
    Deny,
    /// Fall through to the trust store / ask_user flow (default).
    Ask,
}

/// One `[[rules]]` entry in `permissions.toml`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PermissionRule {
    /// Tool name to match; empty matches any tool.
    #[serde(default)]
    pub tool: String,
    /// Glob pattern matched against the call args; empty matches anything.
    #[serde(default)]
    pub pattern: String,
    pub decision: Decision,
}

impl PermissionRule {
    fn matches(&self, tool: &str, args: &str) -> bool {
        if !self.tool.is_empty() && self.tool != tool {
            return false;
        }
        if self.pattern.is_empty() {
            return true;
        }
        glob_match(&self.pattern, args)
    }
}

/// Loaded permission set. Missing file / parse error ⇒ empty set (all `Ask`).
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Permissions {
    #[serde(default)]
    pub rules: Vec<PermissionRule>,
}

impl Permissions {
    /// Load from `<dir>/permissions.toml`; missing/unreadable → empty set.
    pub fn load_from_dir(dir: &Path) -> Self {
        let path = dir.join("permissions.toml");
        match std::fs::read_to_string(&path) {
            Ok(data) => Self::from_toml(&data),
            Err(_) => Self::default(),
        }
    }

    /// Parse TOML content; on parse error → empty set (never panics).
    pub fn from_toml(data: &str) -> Self {
        toml::from_str(data).unwrap_or_default()
    }

    /// Evaluate the rule set for `(tool, args)`.
    ///
    /// Deny is sticky: any matching `Deny` rule wins over matching `Allow`
    /// rules regardless of declaration order. Allow only applies when no
    /// deny rule matches. Anything unmatched falls back to `Ask`.
    pub fn check(&self, tool: &str, args: &str) -> Decision {
        let mut saw_allow = false;
        for rule in &self.rules {
            if rule.matches(tool, args) {
                match rule.decision {
                    Decision::Deny => return Decision::Deny,
                    Decision::Allow => saw_allow = true,
                    Decision::Ask => {}
                }
            }
        }
        if saw_allow {
            Decision::Allow
        } else {
            Decision::Ask
        }
    }

    /// One-line summary of active `Deny` rules, for sys_prompt injection.
    pub fn deny_summary(&self) -> Vec<String> {
        self.rules
            .iter()
            .filter(|r| r.decision == Decision::Deny)
            .map(|r| {
                if r.pattern.is_empty() {
                    format!("deny {}", r.tool)
                } else {
                    format!("deny {}({})", r.tool, r.pattern)
                }
            })
            .collect()
    }
}

/// Glob match with `*` wildcard (any sequence, incl. empty).
fn glob_match(pattern: &str, text: &str) -> bool {
    let pat: Vec<char> = pattern.chars().collect();
    let txt: Vec<char> = text.chars().collect();
    let (mut p, mut t) = (0, 0);
    let (mut star, mut mark) = (None, 0usize);
    while t < txt.len() {
        if p < pat.len() && pat[p] == txt[t] {
            p += 1;
            t += 1;
        } else if p < pat.len() && pat[p] == '*' {
            star = Some(p);
            mark = t;
            p += 1;
        } else if let Some(sp) = star {
            p = sp + 1;
            mark += 1;
            t = mark;
        } else {
            return false;
        }
    }
    while p < pat.len() && pat[p] == '*' {
        p += 1;
    }
    p == pat.len()
}

#[cfg(test)]
mod tests {
    use super::*;

    const DENY_RM: &str = r#"
[[rules]]
tool = "code_run"
pattern = "rm -rf *"
decision = "deny"
"#;

    #[test]
    fn test_default_is_ask() {
        let perms = Permissions::default();
        assert_eq!(perms.check("code_run", "rm -rf /tmp/x"), Decision::Ask);
    }

    #[test]
    fn test_deny_matches_rm_rf() {
        let perms = Permissions::from_toml(DENY_RM);
        assert_eq!(perms.check("code_run", "rm -rf /tmp/x"), Decision::Deny);
    }

    #[test]
    fn test_deny_does_not_match_git_status() {
        let perms = Permissions::from_toml(DENY_RM);
        assert_eq!(perms.check("code_run", "git status"), Decision::Ask);
    }

    #[test]
    fn test_allow_git() {
        let toml = r#"
[[rules]]
tool = "code_run"
pattern = "git *"
decision = "allow"
"#;
        let perms = Permissions::from_toml(toml);
        assert_eq!(perms.check("code_run", "git status"), Decision::Allow);
    }

    #[test]
    fn test_deny_wins_over_allow() {
        let toml = r#"
[[rules]]
tool = "code_run"
pattern = "rm *"
decision = "allow"

[[rules]]
tool = "code_run"
pattern = "rm -rf *"
decision = "deny"
"#;
        let perms = Permissions::from_toml(toml);
        assert_eq!(perms.check("code_run", "rm -rf /"), Decision::Deny);
        assert_eq!(perms.check("code_run", "rm -i x"), Decision::Allow);
    }

    #[test]
    fn test_tool_scoped_rule() {
        let toml = r#"
[[rules]]
tool = "write"
pattern = "/etc/**"
decision = "deny"
"#;
        let perms = Permissions::from_toml(toml);
        assert_eq!(perms.check("write", "/etc/passwd"), Decision::Deny);
        assert_eq!(perms.check("code_run", "/etc/passwd"), Decision::Ask);
    }

    #[test]
    fn test_glob_middle_wildcard() {
        assert!(glob_match("/etc/**", "/etc/passwd"));
        assert!(glob_match("git * --force", "git push --force"));
        assert!(!glob_match("git * --force", "git push"));
    }

    #[test]
    fn test_load_from_dir_missing_file() {
        let dir = std::env::temp_dir().join("oz-safety-missing-perms");
        let perms = Permissions::load_from_dir(&dir);
        assert!(perms.rules.is_empty());
        assert_eq!(perms.check("code_run", "anything"), Decision::Ask);
    }

    #[test]
    fn test_deny_summary() {
        let toml = r#"
[[rules]]
tool = "code_run"
pattern = "rm -rf *"
decision = "deny"

[[rules]]
tool = "code_run"
pattern = "git *"
decision = "allow"
"#;
        let perms = Permissions::from_toml(toml);
        assert_eq!(perms.deny_summary(), vec!["deny code_run(rm -rf *)"]);
    }
}
