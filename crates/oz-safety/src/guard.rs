//! Safety guard — the dispatch-time pipeline that checks each tool call.
//!
//! Pipeline order:
//! 1. Safe-tools whitelist → always allow
//! 2. Builtin blocklist → always deny (handled at tool level)
//! 3. Trust store → check (tool, pattern) → Allowed / Blocked / NeedsApproval

use crate::patterns::build_pattern;
use crate::trust::{TrustDecision, TrustStore};

pub struct SafetyGuard {
    trust_store: TrustStore,
    safe_tools: Vec<String>,
}

impl SafetyGuard {
    pub fn new(trust_store: TrustStore) -> Self {
        SafetyGuard {
            trust_store,
            safe_tools: vec![
                "respond".into(),
                "working_mem".into(),
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
                "web_js".into(),
                "todowrite".into(),
                "todoupdate".into(),
                "long_term".into(),
            ],
        }
    }

    pub fn with_safe_tools(mut self, tools: &[&str]) -> Self {
        self.safe_tools = tools.iter().map(|s| s.to_string()).collect();
        self
    }

    pub fn trust_store(&self) -> &TrustStore {
        &self.trust_store
    }

    /// Check if a tool call is safe to execute.
    ///
    /// Returns:
    /// - `Allowed` — safe tool, or tool+pattern is trusted
    /// - `Blocked(msg)` — builtin blocklist or user-blocked
    /// - `NeedsApproval(info)` — needs user confirmation
    pub fn check(&self, tool: &str, args: &serde_json::Value) -> TrustDecision {
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
        self.trust_store.record_approval(&pattern.0, &pattern.1, &info);
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
        guard.record_approval("code_run", &serde_json::json!({"code": "echo hello"}));
        guard.record_approval("code_run", &serde_json::json!({"code": "echo world"}));
        guard.record_approval("code_run", &serde_json::json!({"code": "echo test"}));

        let decision = guard.check("code_run", &serde_json::json!({"code": "echo more"}));
        assert!(matches!(decision, TrustDecision::Allowed));
    }
}
