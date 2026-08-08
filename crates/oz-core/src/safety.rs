//! Safety module — defines security policies for agent tool execution.
//!
//! Provides the safe-tools whitelist and integrates with the ga-safety crate
//! (when present) for progressive trust and approval workflows.

/// Tools that never trigger safety checks or approval dialogs.
///
/// These are read-only or memory-only tools that cannot cause
/// filesystem damage, data exfiltration, or command execution.
pub const SAFE_TOOLS: &[&str] = &[
    "respond",
    "working_mem",
    "ask_user",
    "skill_mcp_search",
    "skill_mcp_list",
];
