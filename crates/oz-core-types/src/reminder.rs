//! Reminder system — allows the agent to schedule future self-triggered messages.
//!
//! The `schedule_reminder` tool sends a [`Reminder`] through a global channel.
//! The Tauri backend receives it, stores it, and when the timer fires, injects
//! the message into the session and runs the agent again.

use serde::{Deserialize, Serialize};
use std::sync::OnceLock;
use tokio::sync::mpsc::UnboundedSender;

/// A scheduled reminder created by the agent during a turn.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Reminder {
    pub session_id: String,
    pub message: String,
    pub fire_at_ms: u64,
    pub repeat_count: u32,
    pub repeat_interval_secs: u64,
}

/// Global channel for sending reminders from the `schedule_reminder` tool
/// to the Tauri reminder manager. Initialized once by the Tauri setup.
pub static REMINDER_TX: OnceLock<UnboundedSender<Reminder>> = OnceLock::new();

// NOTE: the old CURRENT_REMINDER_SESSION process-global was removed — it held
// a single session id that concurrent agent runs overwrote, so reminders
// could be tagged with the wrong session. The session id now travels on
// `ToolContext::session_id`.
