//! MCP Server Discovery & Lifecycle Management.
//!
//! Manages multiple MCP server instances: startup, health monitoring,
//! tool aggregation, and shutdown.

use crate::client::{McpClient, McpState};
use crate::config::McpDiscovery;
use crate::types::McpTool;
use crate::McpError;

/// Manages the lifecycle of multiple MCP server connections.
pub struct McpManager {
    clients: Vec<McpClient>,
}

impl McpManager {
    /// Create a new manager from a loaded discovery config.
    pub fn from_discovery(discovery: &McpDiscovery) -> Self {
        let clients: Vec<McpClient> = discovery
            .enabled_servers()
            .iter()
            .map(|cfg| McpClient::new(cfg.clone()))
            .collect();

        McpManager { clients }
    }

    /// Start all auto_start servers.
    pub async fn start_all(&mut self) -> Result<usize, McpError> {
        let mut started = 0;
        for client in &mut self.clients {
            if client.state() == &McpState::Disconnected {
                tracing::info!("Starting MCP server: {}", client.server_name());
                match client.start().await {
                    Ok(()) => started += 1,
                    Err(e) => {
                        tracing::warn!("Failed to start MCP server '{}': {}", client.server_name(), e);
                    }
                }
            }
        }
        Ok(started)
    }

    /// Get all tools from all connected servers.
    pub fn all_tools(&self) -> Vec<McpTool> {
        self.clients
            .iter()
            .filter(|c| c.state() == &McpState::Connected)
            .flat_map(|c| c.tools())
            .cloned()
            .collect()
    }

    /// Get tools from a specific server.
    pub fn server_tools(&self, server_name: &str) -> Option<Vec<McpTool>> {
        self.clients
            .iter()
            .find(|c| c.server_name() == server_name && c.state() == &McpState::Connected)
            .map(|c| c.tools().to_vec())
    }

    /// Get the number of connected servers.
    pub fn connected_count(&self) -> usize {
        self.clients
            .iter()
            .filter(|c| c.state() == &McpState::Connected)
            .count()
    }

    /// Total number of tools across all connected servers.
    pub fn tool_count(&self) -> usize {
        self.all_tools().len()
    }

    /// Stop all running servers.
    pub async fn stop_all(&mut self) {
        for client in &mut self.clients {
            client.stop().await;
        }
    }
}

impl Drop for McpManager {
    fn drop(&mut self) {
        // Clients auto-kill on drop via McpClient::Drop
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_manager_from_empty_discovery() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("servers.toml");
        let mut discovery = McpDiscovery::new(&path);
        discovery.load().unwrap();

        let manager = McpManager::from_discovery(&discovery);
        assert_eq!(manager.connected_count(), 0);
        assert_eq!(manager.tool_count(), 0);
    }
}
