//! OpenZen platform adapter framework.
//!
//! Provides the `PlatformAdapter` trait that every messaging platform
//! (Telegram, Feishu, WeChat, QQ, etc.) must implement, along with
//! `AgentBridge` — a non-Tauri wrapper around the existing agent loop
//! — and `PlatformRegistry` for managing multiple adapters.
//!
//! ## Architecture
//!
//! ```text
//! Platform (Telegram/Feishu/...) → PlatformAdapter::start()
//!   → AgentBridge::send_message(session_id, text)
//!     → reuses run_agent_for_session() internally
//!       → mpsc::UnboundedReceiver<StreamEvent>
//!         → adapter streams events back to platform user
//! ```

pub mod bridge;
pub mod registry;
pub mod config;

use async_trait::async_trait;
use std::path::PathBuf;
use std::sync::Arc;
use serde::{Deserialize, Serialize};

pub use bridge::AgentBridge;
pub use registry::PlatformRegistry;
pub use config::PlatformConfig;

// ── Core trait ──

/// Every messaging platform adapter must implement this trait.
///
/// ## Lifecycle
///
/// 1. `PlatformRegistry::start_all()` calls `start()` for each registered adapter.
/// 2. `start()` should block until the adapter is stopped or encounters a fatal error.
///    It receives a `PlatformContext` containing the `AgentBridge` for interacting
///    with openzen's agent loop.
/// 3. `stop()` is called on graceful shutdown.
/// 4. `health()` is polled periodically to detect stale connections.
#[async_trait]
pub trait PlatformAdapter: Send + Sync {
    /// Unique identifier: "telegram", "feishu", "wechat", "qq"
    fn id(&self) -> &'static str;

    /// Human-readable name: "Telegram", "飞书"
    fn name(&self) -> &'static str;

    /// Start the adapter. Should block (loop internally) until shutdown or
    /// fatal error. The adapter uses `ctx.agent` to send messages to the
    /// openzen agent and stream results back to platform users.
    async fn start(&self, ctx: PlatformContext) -> Result<(), PlatformError>;

    /// Gracefully stop the adapter.
    async fn stop(&self) -> Result<(), PlatformError>;

    /// Health check. Called periodically by the registry.
    async fn health(&self) -> PlatformHealth;
}

// ── Platform context ──

/// Context passed to each adapter at startup.
#[derive(Clone)]
pub struct PlatformContext {
    /// Bridge to openzen's agent loop.
    pub agent: Arc<AgentBridge>,

    /// Platform-specific configuration (from mykey.toml [platforms.*]).
    pub platform_config: PlatformConfig,

    /// Working directory for temp files, media cache, etc.
    pub working_dir: PathBuf,
}

// ── Health ──

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PlatformHealth {
    pub connected: bool,
    pub last_event_at: Option<chrono::DateTime<chrono::Utc>>,
    pub status: String,
}

impl PlatformHealth {
    pub fn healthy() -> Self {
        PlatformHealth {
            connected: true,
            last_event_at: Some(chrono::Utc::now()),
            status: "ok".into(),
        }
    }

    pub fn disconnected(reason: impl Into<String>) -> Self {
        PlatformHealth {
            connected: false,
            last_event_at: None,
            status: reason.into(),
        }
    }
}

// ── Errors ──

#[derive(Debug, thiserror::Error)]
pub enum PlatformError {
    #[error("configuration error: {0}")]
    Config(String),

    #[error("connection error: {0}")]
    Connection(String),

    #[error("agent error: {0}")]
    Agent(String),

    #[error("send error: {0}")]
    Send(String),

    #[error("internal error: {0}")]
    Internal(#[from] anyhow::Error),
}

// ── Model info (for /llm command responses) ──

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModelInfo {
    pub index: usize,
    pub name: String,
    pub is_current: bool,
}

// ── Shared text utilities (used by multiple adapters) ──

/// Remove XML-style tags like <thinking>, <summary>, <tool_use>, <file_content>
/// from agent output before sending to platforms.
/// Uses simple string scanning to avoid adding a regex dependency
/// to the framework crate. Tag removal is O(n) per tag which is fine
/// for the small fixed set of tags.
pub fn clean_agent_output(text: &str) -> String {
    let tags = ["thinking", "summary", "tool_use", "file_content"];
    let mut result = text.to_string();
    // First pass: remove well-formed <tag>…</tag> pairs.
    for tag in &tags {
        let open = format!("<{}>", tag);
        let close = format!("</{}>", tag);
        while let (Some(start), Some(end)) = (result.find(&open), result.rfind(&close)) {
            if end > start {
                result.replace_range(start..end + close.len(), "");
            } else {
                break;
            }
        }
    }
    // Second pass: strip any leftover standalone opening or closing tags
    // (e.g. a stray </summary> without a matching <summary>).
    for tag in &tags {
        let open = format!("<{}>", tag);
        let close = format!("</{}>", tag);
        result = result.replace(&close, "").replace(&open, "");
    }
    let max_consecutive_newlines = 2;
    let mut cleaned = String::with_capacity(result.len());
    let mut newline_count = 0;
    for ch in result.chars() {
        if ch == '\n' {
            newline_count += 1;
            if newline_count <= max_consecutive_newlines {
                cleaned.push(ch);
            }
        } else {
            newline_count = 0;
            cleaned.push(ch);
        }
    }
    cleaned.trim().to_string()
}

/// Extract [FILE:...] markers from agent output.
pub fn extract_files(text: &str) -> Vec<String> {
    let mut files = Vec::new();
    let mut rest = text;
    while let Some(start) = rest.find("[FILE:") {
        let after_tag = &rest[start + "[FILE:".len()..];
        if let Some(end) = after_tag.find(']') {
            files.push(after_tag[..end].to_string());
            rest = &after_tag[end + 1..];
        } else {
            break;
        }
    }
    files
}

/// Split text into chunks respecting a maximum byte length,
/// breaking at newline boundaries when possible.
pub fn split_text(text: &str, max_len: usize) -> Vec<String> {
    let text = text.trim();
    if text.is_empty() {
        return vec!["...".into()];
    }
    let mut parts = Vec::new();
    let mut remaining = text;
    while remaining.len() > max_len {
        let cut = remaining[..max_len].rfind('\n')
            .filter(|&pos| pos > max_len * 60 / 100)
            .unwrap_or(max_len);
        parts.push(remaining[..cut].trim_end().to_string());
        remaining = remaining[cut..].trim_start();
    }
    if !remaining.is_empty() {
        parts.push(remaining.to_string());
    }
    if parts.is_empty() {
        parts.push("...".into());
    }
    parts
}

/// FILE_HINT injected into user messages so the agent knows it can
/// reference generated files with [FILE:path] markers.
pub const FILE_HINT: &str = "If you need to show files to user, use [FILE:filepath] in your response.";

// ── Platform session counter persistence ──

use std::collections::HashMap;
use std::path::Path;

/// Load per-chat /new session counters from disk.
/// Returns an empty map if the file doesn't exist or can't be parsed.
pub fn load_platform_counters(path: &Path) -> HashMap<String, u32> {
    std::fs::read_to_string(path)
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default()
}

/// Save per-chat /new session counters to disk.
pub fn save_platform_counters(path: &Path, counters: &HashMap<String, u32>) {
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    if let Ok(json) = serde_json::to_string(counters) {
        let _ = std::fs::write(path, &json);
    }
}

// ── Persistent message deduplication ──

use std::collections::VecDeque;
const MAX_PERSISTED_MSG_IDS: usize = 1000;

/// Load recently-seen message IDs from disk. Survives restarts so old
/// messages replayed by the Feishu server on reconnect are skipped.
pub fn load_seen_msg_ids(path: &Path) -> VecDeque<String> {
    std::fs::read_to_string(path)
        .ok()
        .and_then(|s| serde_json::from_str::<Vec<String>>(&s).ok())
        .map(|v| v.into_iter().collect())
        .unwrap_or_default()
}

/// Save recently-seen message IDs to disk. Trims to MAX_PERSISTED_MSG_IDS.
pub fn save_seen_msg_ids(path: &Path, ids: &VecDeque<String>) {
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let tail: Vec<&String> = ids.iter().rev().take(MAX_PERSISTED_MSG_IDS).collect();
    let trimmed: Vec<&String> = tail.into_iter().rev().collect();
    if let Ok(json) = serde_json::to_string(&trimmed) {
        let _ = std::fs::write(path, &json);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn clean_removes_thinking_tag() {
        let input = "Hello <thinking>internal stuff</thinking> world";
        assert_eq!(clean_agent_output(input), "Hello  world");
    }

    #[test]
    fn clean_collapses_multiple_newlines() {
        let input = "a\n\n\n\nb";
        assert_eq!(clean_agent_output(input), "a\n\nb");
    }

    #[test]
    fn extract_files_finds_markers() {
        let input = "Here is [FILE:/tmp/a.txt] and [FILE:/tmp/b.png]";
        let files = extract_files(input);
        assert_eq!(files, vec!["/tmp/a.txt", "/tmp/b.png"]);
    }

    #[test]
    fn split_text_at_newlines() {
        let input = "line1\nline2\nline3\nline4";
        let parts = split_text(input, 15);
        assert!(parts.len() >= 2);
        // split_text keeps newlines inside each part (cuts at newline
        // boundaries to preserve paragraph structure); only the cut-point
        // newline is trimmed. Content (minus newlines) must be preserved.
        assert_eq!(
            parts.join("").replace('\n', ""),
            input.replace('\n', "")
        );
    }
}
