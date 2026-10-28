//! Subagent seam — delegation to an isolated child agent.
//!
//! OpenZen runs children **in-process**: the host (Tauri desktop or webui)
//! implements [`SubagentHost`], creates a child session, runs the normal agent
//! loop, and returns the final text. Depth is tracked here so a child cannot
//! spawn grandchildren forever, and `ask_user` is refused inside a child
//! (the question must come back to the parent — see the plan §5.7).

use std::collections::HashMap;
use std::sync::{Arc, Mutex, OnceLock};

use async_trait::async_trait;

/// Hard cap on delegation depth (a root chat is depth 0).
pub const MAX_DEPTH: u8 = 3;

#[derive(Clone, Debug)]
pub struct SubagentRequest {
    /// Caller-chosen id so the tool can cancel a background child before the
    /// host has even started it.
    pub child_id: String,
    /// Session that invoked `spawn_agent`.
    pub parent_session: String,
    /// One-line label shown in the parent's tool card.
    pub label: String,
    /// Full task description (what to do, how to verify, what to return).
    pub prompt: String,
    /// Optional per-child turn budget (None = loop default).
    pub max_turns: Option<u32>,
    /// Optional mykey.toml session name; `None` inherits the parent's model.
    pub model: Option<String>,
}

#[derive(Clone, Debug)]
pub struct SubagentResult {
    pub child_session: String,
    /// "completed" | "failed"
    pub status: String,
    pub text: String,
    /// Set when the child called `submit_result` (P5 structured output).
    pub structured: Option<serde_json::Value>,
}

/// Implemented by the runtime that owns the agent loop.
#[async_trait]
pub trait SubagentHost: Send + Sync {
    async fn run(&self, req: SubagentRequest) -> Result<SubagentResult, String>;
    /// Continue a finished child from its sidecar transcript (P5).
    async fn continue_child(
        &self,
        _child_id: &str,
        _message: &str,
        _parent_session: &str,
    ) -> Result<SubagentResult, String> {
        Err("continuable subagents are not available in this runtime".into())
    }
    /// Best-effort cancellation (parent stopped / job killed). Default: no-op.
    fn cancel(&self, _child_session: &str) {}
}

// ── Child labels for the approval dialog ───────────────────────────────
//
// The approval payload is built deep inside the safety layer, which has no
// AppState. A tiny label registry lets it say "from subagent · <label>".

static LABELS: OnceLock<Mutex<HashMap<String, String>>> = OnceLock::new();

fn labels() -> &'static Mutex<HashMap<String, String>> {
    LABELS.get_or_init(|| Mutex::new(HashMap::new()))
}

pub fn set_label(child: &str, label: &str) {
    if let Ok(mut m) = labels().lock() {
        m.insert(child.to_string(), label.to_string());
    }
}

pub fn label_of(child: &str) -> Option<String> {
    labels().lock().ok().and_then(|m| m.get(child).cloned())
}

pub fn forget_label(child: &str) {
    if let Ok(mut m) = labels().lock() {
        m.remove(child);
    }
}

static HOST: OnceLock<Arc<dyn SubagentHost>> = OnceLock::new();

pub fn set_host(host: Arc<dyn SubagentHost>) {
    let _ = HOST.set(host);
}

pub fn host() -> Option<Arc<dyn SubagentHost>> {
    HOST.get().cloned()
}

// ── Delegation depth ────────────────────────────────────────────────────
//
// Keyed by child session id. A root chat has no entry (depth 0). Entries are
// removed when the child finishes; a bounded map keeps a leak from growing.

static DEPTHS: OnceLock<Mutex<HashMap<String, u8>>> = OnceLock::new();

fn depths() -> &'static Mutex<HashMap<String, u8>> {
    DEPTHS.get_or_init(|| Mutex::new(HashMap::new()))
}

pub fn depth_of(session: &str) -> u8 {
    depths()
        .lock()
        .ok()
        .and_then(|m| m.get(session).copied())
        .unwrap_or(0)
}

/// Register a child's depth before its loop starts.
pub fn register_child(child: &str, depth: u8) {
    if let Ok(mut m) = depths().lock() {
        if m.len() > 128 {
            // Drop the oldest half if a caller forgot to clean up. HashMap
            // order is arbitrary, so this is a bound, not an LRU.
            let keep: Vec<String> = m.keys().take(64).cloned().collect();
            for k in keep {
                m.remove(&k);
            }
        }
        m.insert(child.to_string(), depth);
    }
}

pub fn forget(child: &str) {
    if let Ok(mut m) = depths().lock() {
        m.remove(child);
    }
}

/// Depth the next child of `parent` would have — `None` when over the cap.
pub fn next_depth(parent: &str) -> Option<u8> {
    let next = depth_of(parent).saturating_add(1);
    (next <= MAX_DEPTH).then_some(next)
}

// ── Per-root concurrency ──────────────────────────────────────────────
//
// Depth alone does not bound fan-out: one turn can call spawn_agent many
// times in parallel. Cap direct children per parent (default 4).

const MAX_CHILDREN_PER_ROOT: u8 = 4;
static ACTIVE: OnceLock<Mutex<HashMap<String, u8>>> = OnceLock::new();

fn active() -> &'static Mutex<HashMap<String, u8>> {
    ACTIVE.get_or_init(|| Mutex::new(HashMap::new()))
}

/// Reserve a child slot for `parent`. `None` means the parent is at capacity.
pub fn try_acquire_slot(parent: &str) -> Option<()> {
    let mut m = active().lock().ok()?;
    let n = m.entry(parent.to_string()).or_insert(0);
    if *n >= MAX_CHILDREN_PER_ROOT {
        None
    } else {
        *n += 1;
        Some(())
    }
}

pub fn release_slot(parent: &str) {
    if let Ok(mut m) = active().lock() {
        if let Some(n) = m.get_mut(parent) {
            *n = n.saturating_sub(1);
            if *n == 0 {
                m.remove(parent);
            }
        }
    }
}

// ── Per-child turn budget & structured result ─────────────────────────
//
// Both are keyed by child session and consumed exactly once by the runner /
// host. They are cleared by the child guard so a reused id cannot inherit
// stale state.

static TURN_LIMITS: OnceLock<Mutex<HashMap<String, u32>>> = OnceLock::new();
static STRUCTURED: OnceLock<Mutex<HashMap<String, serde_json::Value>>> = OnceLock::new();

pub fn set_max_turns(child: &str, turns: u32) {
    if let Ok(mut m) = TURN_LIMITS
        .get_or_init(|| Mutex::new(HashMap::new()))
        .lock()
    {
        m.insert(child.to_string(), turns.clamp(1, 40));
    }
}

pub fn take_max_turns(child: &str) -> Option<u32> {
    TURN_LIMITS
        .get()
        .and_then(|m| m.lock().ok())
        .and_then(|mut m| m.remove(child))
}

pub fn set_structured(child: &str, value: serde_json::Value) {
    if let Ok(mut m) = STRUCTURED.get_or_init(|| Mutex::new(HashMap::new())).lock() {
        m.insert(child.to_string(), value);
    }
}

pub fn take_structured(child: &str) -> Option<serde_json::Value> {
    STRUCTURED
        .get()
        .and_then(|m| m.lock().ok())
        .and_then(|mut m| m.remove(child))
}

// ── Single-flight continuation ────────────────────────────────────────
//
// Two parallel `send_message` calls for one child would both rehydrate and
// then overwrite the same sidecar. Only one continuation per child at a time.

static CONTINUING: OnceLock<Mutex<HashMap<String, ()>>> = OnceLock::new();

fn continuing() -> &'static Mutex<HashMap<String, ()>> {
    CONTINUING.get_or_init(|| Mutex::new(HashMap::new()))
}

pub fn try_begin_continue(child: &str) -> bool {
    match continuing().lock() {
        Ok(mut m) => m.insert(child.to_string(), ()).is_none(),
        Err(_) => false,
    }
}

pub fn end_continue(child: &str) {
    if let Ok(mut m) = continuing().lock() {
        m.remove(child);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn turn_budget_is_consumed_once() {
        set_max_turns("lim-1", 7);
        assert_eq!(take_max_turns("lim-1"), Some(7));
        assert_eq!(take_max_turns("lim-1"), None);
    }

    #[test]
    fn structured_result_is_consumed_once() {
        set_structured("s-1", serde_json::json!({"a": 1}));
        assert_eq!(take_structured("s-1"), Some(serde_json::json!({"a": 1})));
        assert_eq!(take_structured("s-1"), None);
    }

    #[test]
    fn continuation_is_single_flight() {
        assert!(try_begin_continue("c1"));
        assert!(!try_begin_continue("c1"));
        end_continue("c1");
        assert!(try_begin_continue("c1"));
        end_continue("c1");
    }

    #[test]
    fn slots_are_capped_per_parent() {
        for _ in 0..MAX_CHILDREN_PER_ROOT {
            assert!(try_acquire_slot("root-slots").is_some());
        }
        assert!(try_acquire_slot("root-slots").is_none());
        release_slot("root-slots");
        assert!(try_acquire_slot("root-slots").is_some());
        for _ in 0..MAX_CHILDREN_PER_ROOT {
            release_slot("root-slots");
        }
    }

    #[test]
    fn depth_chains_and_caps() {
        register_child("a", 1);
        assert_eq!(depth_of("a"), 1);
        assert_eq!(next_depth("a"), Some(2));
        register_child("b", 2);
        assert_eq!(next_depth("b"), Some(3));
        register_child("c", 3);
        assert_eq!(next_depth("c"), None);
        forget("a");
        assert_eq!(depth_of("a"), 0);
    }
}
