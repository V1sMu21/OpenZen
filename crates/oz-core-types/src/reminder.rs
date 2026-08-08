//! Reminder system — allows the agent to schedule future self-triggered messages.
//!
//! The `schedule_reminder` tool sends a [`Reminder`] through a global channel.
//! The Tauri backend receives it, stores it, and when the timer fires, injects
//! the message into the session and runs the agent again.

use serde::{Deserialize, Serialize};
use std::sync::{Arc, Mutex, OnceLock};
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

/// Tracks the currently running session ID so the schedule_reminder tool
/// can tag reminders with the correct session. Set by Tauri before each
/// agent run, cleared after.
pub static CURRENT_REMINDER_SESSION: OnceLock<Arc<Mutex<Option<String>>>> = OnceLock::new();
