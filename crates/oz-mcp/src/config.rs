//! MCP Server Configuration — parses `servers.toml`.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::McpError;

/// A single MCP server entry from servers.toml.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ServerConfig {
    pub name: String,
    pub command: String,
    #[serde(default)]
    pub args: Vec<String>,
    #[serde(default = "default_true")]
    pub enabled: bool,
    #[serde(default = "default_true")]
    pub auto_start: bool,
    #[serde(default)]
    pub env: std::collections::HashMap<String, String>,
}

fn default_true() -> bool { true }

/// Top-level servers.toml structure.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct ServersToml {
    pub servers: Vec<ServerConfig>,
}

/// Discovers and loads MCP server configurations.
pub struct McpDiscovery {
    config_path: PathBuf,
    servers: Vec<ServerConfig>,
}

impl McpDiscovery {
    /// Create a new discovery instance.
    /// Looks for `servers.toml` at the given path.
    pub fn new(config_path: &Path) -> Self {
        McpDiscovery {
            config_path: config_path.to_path_buf(),
            servers: Vec::new(),
        }
    }

    /// Load server configurations from the config file.
    pub fn load(&mut self) -> Result<usize, McpError> {
        if !self.config_path.exists() {
            return Ok(0);
        }

        let content = std::fs::read_to_string(&self.config_path)
            .map_err(|e| McpError::Config(format!("Failed to read {}: {}", self.config_path.display(), e)))?;

        let config: ServersToml = toml::from_str(&content)
            .map_err(|e| McpError::Config(format!("Failed to parse {}: {}", self.config_path.display(), e)))?;

        let count = config.servers.len();
        self.servers = config.servers
            .into_iter()
            .filter(|s| s.enabled)
            .collect();

        Ok(count)
    }

    /// Get enabled server configs.
    pub fn enabled_servers(&self) -> &[ServerConfig] {
        &self.servers
    }

    /// Find a server by name.
    pub fn find(&self, name: &str) -> Option<&ServerConfig> {
        self.servers.iter().find(|s| s.name == name)
    }

    /// Write default servers.toml template if file does not exist.
    pub fn ensure_default(&self) -> Result<(), McpError> {
        if self.config_path.exists() {
            return Ok(());
        }

        if let Some(parent) = self.config_path.parent() {
            std::fs::create_dir_all(parent).map_err(McpError::Io)?;
        }

        let default = r#"# MCP Server Configuration
# Add MCP-compatible servers here.
# Each server runs as a subprocess and exposes tools to the agent.

[[servers]]
name = "playwright"
command = "npx"
args = ["-y", "@modelcontextprotocol/server-playwright"]
enabled = false
auto_start = true

[[servers]]
name = "filesystem"
command = "npx"
args = ["-y", "@modelcontextprotocol/server-filesystem", "/tmp"]
enabled = false
auto_start = true
"#;

        std::fs::write(&self.config_path, default).map_err(McpError::Io)?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_servers_toml() {
        let toml_str = r#"
[[servers]]
name = "test-server"
command = "echo"
args = ["hello"]
enabled = true

[[servers]]
name = "disabled-server"
command = "echo"
args = ["world"]
enabled = false
"#;
        let config: ServersToml = toml::from_str(toml_str).unwrap();
        assert_eq!(config.servers.len(), 2);
        assert!(config.servers[0].enabled);
        assert!(!config.servers[1].enabled);
    }

    #[test]
    fn test_discovery_load() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("servers.toml");

        std::fs::write(&path, r#"
[[servers]]
name = "playwright"
command = "npx"
args = ["-y", "@modelcontextprotocol/server-playwright"]
"#).unwrap();

        let mut discovery = McpDiscovery::new(&path);
        let count = discovery.load().unwrap();
        assert_eq!(count, 1);
        assert_eq!(discovery.enabled_servers().len(), 1);
    }

    #[test]
    fn test_discovery_empty_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("servers.toml");

        let mut discovery = McpDiscovery::new(&path);
        let count = discovery.load().unwrap();
        assert_eq!(count, 0);
    }

    #[test]
    fn test_ensure_default_creates_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("servers.toml");
        let discovery = McpDiscovery::new(&path);
        discovery.ensure_default().unwrap();
        assert!(path.exists());
    }

    #[test]
    fn test_find_server() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("servers.toml");
        std::fs::write(&path, r#"
[[servers]]
name = "my-server"
command = "echo"
"#).unwrap();

        let mut discovery = McpDiscovery::new(&path);
        discovery.load().unwrap();
        assert!(discovery.find("my-server").is_some());
        assert!(discovery.find("nope").is_none());
    }
}
