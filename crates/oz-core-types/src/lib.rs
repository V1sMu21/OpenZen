//! GenericAgent core types — shared across all crates.
//!
//! Defines the fundamental data structures that mirror the Python codebase:
//! - `ContentBlock` / `Message` / `Role` — message format (Claude content-block style)
//! - `ToolDefinition` / `ToolFunction` — tool schema format
//! - `StepOutcome` — tool dispatch result
//! - `MockResponse` / `MockToolCall` — LLM response abstraction

pub mod error;
pub mod event;
pub mod message;
pub mod notification_text;
pub mod path_trust;
pub mod reminder;
pub mod session_role;
pub mod skill_mcp;
pub mod supervision;
pub mod tool;
pub mod tool_args;

pub use error::*;
pub use event::*;
pub use message::*;
pub use reminder::*;
pub use skill_mcp::*;
pub use supervision::*;
pub use tool::*;
pub use tool_args::*;

/// Per-session write confinement for background workers. Kept here (rather
/// than in oz-core) so tool crates can consult it without a cycle.
pub mod confinement {
    use std::collections::HashMap;
    use std::sync::{Mutex, OnceLock};

    static ROOTS: OnceLock<Mutex<HashMap<String, String>>> = OnceLock::new();

    pub fn set_workspace(session_id: &str, root: &str) {
        if let Ok(mut m) = ROOTS.get_or_init(|| Mutex::new(HashMap::new())).lock() {
            m.insert(session_id.to_string(), root.to_string());
        }
    }

    pub fn workspace_of(session_id: &str) -> Option<String> {
        ROOTS
            .get()
            .and_then(|m| m.lock().ok())
            .and_then(|m| m.get(session_id).cloned())
    }
}
