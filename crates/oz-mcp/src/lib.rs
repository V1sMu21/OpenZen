//! # ga-mcp — MCP (Model Context Protocol) Client
//!
//! Implements the MCP JSON-RPC protocol for connecting to tool servers.
//!
//! ## Architecture
//!
//! ```text
//! servers.toml ──▶ McpDiscovery ──▶ Vec<ServerConfig>
//!                                        │
//!                              ┌─────────┴─────────┐
//!                              ▼                   ▼
//!                       McpClient           McpClient
//!                       (stdio)             (stdio)
//!                              │                   │
//!                              ▼                   ▼
//!                       list_tools()        list_tools()
//!                              │                   │
//!                              └─────────┬─────────┘
//!                                        ▼
//!                              Vec<McpToolDefinition>
//! ```
//!
//! The bridge to `ga-tools::ToolRegistry` is in `ga-tools/src/mcp_bridge.rs`.

pub mod types;
pub mod config;
pub mod client;
pub mod discovery;

pub use types::*;
pub use config::*;
pub use client::*;
pub use discovery::*;

#[derive(Debug, thiserror::Error)]
pub enum McpError {
    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),

    #[error("JSON-RPC error: {0}")]
    Rpc(String),

    #[error("Config error: {0}")]
    Config(String),

    #[error("Server not found: {0}")]
    ServerNotFound(String),

    #[error("Timeout: {0}")]
    Timeout(String),
}
