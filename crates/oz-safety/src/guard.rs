//! Safety guard — the dispatch-time pipeline that checks each tool call.
//!
//! Pipeline order:
//! 1. Safe-tools whitelist → always allow
//! 2. Builtin blocklist → always deny (handled at tool level)
//! 3. Trust store → check (tool, pattern) → Allowed / Blocked / NeedsApproval

use crate::patterns::build_pattern;
use crate::permissions::{match_string, Decision, Permissions};
use crate::trust::{TrustDecision, TrustStore};
use crate::trust_level::ProjectTrustLevel;

pub struct SafetyGuard {
    trust_store: TrustStore,
    safe_tools: Vec<String>,
    permissions: Permissions,
    project_trust: ProjectTrustLevel,
}

impl SafetyGuard {
    pub fn new(trust_store: TrustStore) -> Self {
        SafetyGuard {
            trust_store,
            safe_tools: vec![
                "respond".into(),
                "working_mem".into(),
                "harness_refine".into(),
                "ask_user".into(),
                "skill_mcp_search".into(),
                "skill_mcp_list".into(),
                "read".into(),
                "write".into(),
                "edit".into(),
                "patch".into(),
                "grep".into(),
                "glob".into(),
                "ls".into(),
                "web_search".into(),
                "web_scan".into(),
                "web_fetch".into(),
                // NOTE: `web_js` is deliberately NOT here. It runs arbitrary
                // JavaScript in a live page — the same primitive as
                // `web_execute_js`, which is approval-gated — and its
                // "no exfiltration" guard is a substring blacklist that
                // `window['fe'+'tch'](...)` walks straight through
                // (round5 P1-8b).
                "todowrite".into(),
                "todoupdate".into(),
                // Butler task-graph tools write only {data_dir}/butler.db:
                // no user file access, no shell, no network. Approval-gating
                // them made the whole autonomous flow deadlock into a 30s
                // auto-deny at the first task (caught by the live E2E).
                "task_create".into(),
                "project_list".into(),
                "task_list".into(),
                "task_get".into(),
                "run_list".into(),
                "artifact_find".into(),
                "history_search".into(),
                "task_note".into(),
                "long_term".into(),
                // Background-job controls are pure reads of this session's
                // own registry (kill only cancels work the same session
                // started). Leaving them approval-gated made every poll pop
                // a dialog, which defeats background execution.
                "job_output".into(),
                "job_list".into(),
                "job_kill".into(),
                // Delegation itself is not a file/shell operation; the child
                // inherits this conversation's guard for its own tools.
                "spawn_agent".into(),
                "send_message".into(),
            ],
            permissions: Permissions::default(),
            project_trust: ProjectTrustLevel::Full,
        }
    }

    pub fn with_safe_tools(mut self, tools: &[&str]) -> Self {
        self.safe_tools = tools.iter().map(|s| s.to_string()).collect();
        self
    }

    pub fn with_permissions(mut self, permissions: Permissions) -> Self {
        self.permissions = permissions;
        self
    }

    pub fn with_project_trust(mut self, level: ProjectTrustLevel) -> Self {
        self.project_trust = level;
        self
    }

    pub fn project_trust(&self) -> ProjectTrustLevel {
        self.project_trust
    }

    pub fn trust_store(&self) -> &TrustStore {
        &self.trust_store
    }

    /// Check if a tool call is safe to execute.
    ///
    /// Returns:
    /// - `Allowed` — permission allow, safe tool, or tool+pattern is trusted
    /// - `Blocked(msg)` — permission deny or user-blocked
    /// - `NeedsApproval(info)` — needs user confirmation
    pub fn check(&self, tool: &str, args: &serde_json::Value) -> TrustDecision {
        // Project trust level (B7a) is an environment gate: it must run FIRST
        // so a permissions.toml `allow` can never bypass a restricted/readonly
        // project. Deny wins over everything below.
        if self.project_trust.denied_tools().contains(&tool) {
            return TrustDecision::Blocked(format!(
                "blocked by project trust level ({:?})",
                self.project_trust
            ));
        }

        match self.permissions.check(tool, &match_string(tool, args)) {
            Decision::Deny => return TrustDecision::Blocked("blocked by permission policy".into()),
            Decision::Allow => return TrustDecision::Allowed,
            Decision::Ask => {}
        }

        if self.safe_tools.iter().any(|s| s == tool) {
            return TrustDecision::Allowed;
        }

        let pattern = build_pattern(tool, args);
        self.trust_store.check(&pattern.0, &pattern.1)
    }

    pub fn record_approval(&self, tool: &str, args: &serde_json::Value) {
        let pattern = build_pattern(tool, args);
        let info = match self.trust_store.check(&pattern.0, &pattern.1) {
            TrustDecision::NeedsApproval(i) => i,
            _ => return,
        };
        self.trust_store
            .record_approval(&pattern.0, &pattern.1, &info);
    }

    /// Explicit "trust this session" — allow now, in this run only.
    pub fn trust_session(&self, tool: &str, args: &serde_json::Value) {
        let pattern = crate::patterns::build_pattern(tool, args);
        self.trust_store.trust_session(&pattern.0, &pattern.1);
    }

    /// Explicit "trust this workspace/project" — persisted across sessions.
    pub fn trust_workspace(&self, tool: &str, args: &serde_json::Value) {
        let pattern = crate::patterns::build_pattern(tool, args);
        self.trust_store.trust_workspace(&pattern.0, &pattern.1);
    }

    /// Consult only the progressive-trust store, bypassing the safe-tool
    /// shortcut. Used for out-of-workspace writes: those tools are
    /// safe-listed, so `check()` would return Allowed before any approval.
    pub fn check_trust_only(&self, tool: &str, args: &serde_json::Value) -> TrustDecision {
        let pattern = crate::patterns::build_pattern(tool, args);
        self.trust_store.check(&pattern.0, &pattern.1)
    }

    pub fn record_denial(&self, tool: &str, args: &serde_json::Value) {
        let pattern = build_pattern(tool, args);
        self.trust_store.record_denial(&pattern.0, &pattern.1);
    }

    pub fn block(&self, tool: &str, args: &serde_json::Value) {
        let pattern = build_pattern(tool, args);
        self.trust_store.block(&pattern.0, &pattern.1);
    }
}

impl Clone for SafetyGuard {
    fn clone(&self) -> Self {
        SafetyGuard {
            trust_store: self.trust_store.clone(),
            safe_tools: self.safe_tools.clone(),
            permissions: self.permissions.clone(),
            project_trust: self.project_trust,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_safe_tool_always_allowed() {
        let guard = SafetyGuard::new(TrustStore::in_memory());
        let decision = guard.check("respond", &serde_json::json!({"response": "hello"}));
        assert!(matches!(decision, TrustDecision::Allowed));
    }

    #[test]
    fn test_working_mem_always_allowed() {
        let guard = SafetyGuard::new(TrustStore::in_memory());
        let decision = guard.check("working_mem", &serde_json::json!({"key_info": "test"}));
        assert!(matches!(decision, TrustDecision::Allowed));
    }

    #[test]
    fn test_unknown_tool_needs_approval() {
        let guard = SafetyGuard::new(TrustStore::in_memory());
        let decision = guard.check("code_run", &serde_json::json!({"code": "echo hello"}));
        assert!(matches!(decision, TrustDecision::NeedsApproval(_)));
    }

    #[test]
    fn test_pattern_based_trust() {
        let guard = SafetyGuard::new(TrustStore::in_memory());
        for _ in 0..3 {
            guard.record_approval("code_run", &serde_json::json!({"code": "echo hello"}));
        }

        let decision = guard.check("code_run", &serde_json::json!({"code": "echo hello"}));
        assert!(matches!(decision, TrustDecision::Allowed));
        // A different command must not inherit the grant.
        let other = guard.check("code_run", &serde_json::json!({"code": "echo more"}));
        assert!(matches!(other, TrustDecision::NeedsApproval(_)));
    }

    #[test]
    fn test_permission_deny_blocks() {
        let perms = Permissions::from_toml(
            r#"
[[rules]]
tool = "code_run"
pattern = "rm -rf *"
decision = "deny"
"#,
        );
        let guard = SafetyGuard::new(TrustStore::in_memory()).with_permissions(perms);
        let decision = guard.check("code_run", &serde_json::json!({"code": "rm -rf /tmp/x"}));
        assert!(matches!(decision, TrustDecision::Blocked(_)));
    }

    #[test]
    fn test_permission_allow_skips_trust_store() {
        let perms = Permissions::from_toml(
            r#"
[[rules]]
tool = "code_run"
pattern = "git *"
decision = "allow"
"#,
        );
        let guard = SafetyGuard::new(TrustStore::in_memory()).with_permissions(perms);
        let decision = guard.check("code_run", &serde_json::json!({"code": "git status"}));
        assert!(matches!(decision, TrustDecision::Allowed));
    }

    #[test]
    fn test_permission_ask_falls_through() {
        let perms = Permissions::from_toml(
            r#"
[[rules]]
tool = "code_run"
pattern = "echo *"
decision = "deny"
"#,
        );
        let guard = SafetyGuard::new(TrustStore::in_memory()).with_permissions(perms);
        let decision = guard.check("code_run", &serde_json::json!({"code": "npm install"}));
        assert!(matches!(decision, TrustDecision::NeedsApproval(_)));
    }

    #[test]
    fn test_permission_deny_wins_over_allow() {
        let perms = Permissions::from_toml(
            r#"
[[rules]]
tool = "code_run"
pattern = "rm *"
decision = "allow"

[[rules]]
tool = "code_run"
pattern = "rm -rf *"
decision = "deny"
"#,
        );
        let guard = SafetyGuard::new(TrustStore::in_memory()).with_permissions(perms);
        let denied = guard.check("code_run", &serde_json::json!({"code": "rm -rf /"}));
        assert!(matches!(denied, TrustDecision::Blocked(_)));
        let allowed = guard.check("code_run", &serde_json::json!({"code": "rm -i x"}));
        assert!(matches!(allowed, TrustDecision::Allowed));
    }

    #[test]
    fn test_permission_deny_overrides_safe_tool() {
        let perms = Permissions::from_toml(
            r#"
[[rules]]
tool = "read"
pattern = "/etc/passwd"
decision = "deny"
"#,
        );
        let guard = SafetyGuard::new(TrustStore::in_memory()).with_permissions(perms);
        let decision = guard.check("read", &serde_json::json!({"file_path": "/etc/passwd"}));
        assert!(matches!(decision, TrustDecision::Blocked(_)));
        let other = guard.check("read", &serde_json::json!({"file_path": "/tmp/a.txt"}));
        assert!(matches!(other, TrustDecision::Allowed));
    }

    #[test]
    fn test_restricted_trust_blocks_execution_tools() {
        let guard = SafetyGuard::new(TrustStore::in_memory())
            .with_project_trust(ProjectTrustLevel::Restricted);
        let blocked = guard.check("code_run", &serde_json::json!({"code": "echo hi"}));
        assert!(matches!(blocked, TrustDecision::Blocked(_)));
        let blocked = guard.check("web_execute_js", &serde_json::json!({"code": "1"}));
        assert!(matches!(blocked, TrustDecision::Blocked(_)));
        // Read tools still work.
        let allowed = guard.check("read", &serde_json::json!({"file_path": "/tmp/a"}));
        assert!(matches!(allowed, TrustDecision::Allowed));
    }

    #[test]
    fn test_restricted_trust_deny_wins_over_safe_tools() {
        // code_run is NOT in safe_tools, but write IS — restrict must not
        // block write at Restricted level.
        let guard = SafetyGuard::new(TrustStore::in_memory())
            .with_project_trust(ProjectTrustLevel::Restricted);
        let write_ok = guard.check("write", &serde_json::json!({"file_path": "/tmp/a"}));
        assert!(matches!(write_ok, TrustDecision::Allowed));
    }

    #[test]
    fn test_readonly_trust_blocks_writes() {
        let guard = SafetyGuard::new(TrustStore::in_memory())
            .with_project_trust(ProjectTrustLevel::Readonly);
        let blocked = guard.check("write", &serde_json::json!({"file_path": "/tmp/a"}));
        assert!(matches!(blocked, TrustDecision::Blocked(_)));
        let blocked = guard.check("edit", &serde_json::json!({"file_path": "/tmp/a"}));
        assert!(matches!(blocked, TrustDecision::Blocked(_)));
        let blocked = guard.check("patch", &serde_json::json!({"file_path": "/tmp/a"}));
        assert!(matches!(blocked, TrustDecision::Blocked(_)));
        let allowed = guard.check("grep", &serde_json::json!({"pattern": "x"}));
        assert!(matches!(allowed, TrustDecision::Allowed));
    }

    #[test]
    fn test_full_trust_no_extra_restrictions() {
        let guard = SafetyGuard::new(TrustStore::in_memory());
        assert_eq!(guard.project_trust(), ProjectTrustLevel::Full);
        let allowed = guard.check("code_run", &serde_json::json!({"code": "echo hi"}));
        assert!(matches!(allowed, TrustDecision::NeedsApproval(_)));
    }

    #[test]
    fn test_permissions_allow_cannot_bypass_project_trust() {
        // A permissions.toml `allow code_run` must NOT lift a Restricted
        // project's execution ban (environment gate wins over policy allow).
        let perms = Permissions::from_toml(
            r#"
[[rules]]
tool = "code_run"
pattern = "*"
decision = "allow"
"#,
        );
        let guard = SafetyGuard::new(TrustStore::in_memory())
            .with_permissions(perms)
            .with_project_trust(ProjectTrustLevel::Restricted);
        let decision = guard.check("code_run", &serde_json::json!({"code": "echo hi"}));
        assert!(matches!(decision, TrustDecision::Blocked(_)));
    }

    #[test]
    fn trust_only_does_not_bypass_outside_write_approval() {
        let guard = SafetyGuard::new(TrustStore::in_memory());
        let args = serde_json::json!({"file_path": "/tmp/outside/x.txt"});
        // Normal check says Allowed because write is safe-listed; the
        // outside-write path must still ask.
        assert!(matches!(
            guard.check("write", &args),
            TrustDecision::Allowed
        ));
        assert!(matches!(
            guard.check_trust_only("write", &args),
            TrustDecision::NeedsApproval(_)
        ));
        guard.trust_workspace("write", &args);
        assert!(matches!(
            guard.check_trust_only("write", &args),
            TrustDecision::Allowed
        ));
    }

    #[test]
    fn explicit_trust_refuses_wildcards() {
        let guard = SafetyGuard::new(TrustStore::in_memory());
        guard.trust_workspace("web_js", &serde_json::json!({}));
        assert!(matches!(
            guard.check_trust_only("web_js", &serde_json::json!({})),
            TrustDecision::NeedsApproval(_)
        ));
    }
}
