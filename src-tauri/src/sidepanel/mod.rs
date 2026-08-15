//! Side Panel (Artifact Viewer) module for OpenZen Tauri backend.
//!
//! Provides:
//! - `state`      — SidePanelState, ArtifactInfo data structures
//! - `commands`   — Tauri IPC command handlers
//! - `terminal`   — PTY terminal session management (Phase 2)

pub mod commands;
pub mod scheme;
pub mod state;
pub mod terminal;
