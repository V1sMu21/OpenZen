//! MCP Client — JSON-RPC over stdio transport.
//!
//! Manages a single MCP server process. Communicates via stdin/stdout
//! using the MCP JSON-RPC protocol.

use std::process::Stdio;

use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::{Child, Command};

use crate::config::ServerConfig;
use crate::types::*;
use crate::McpError;

/// State of an MCP client connection.
#[derive(Debug, Clone, PartialEq)]
pub enum McpState {
    Disconnected,
    Connecting,
    Connected,
    Error(String),
}

/// An MCP client connected to a single server process.
pub struct McpClient {
    config: ServerConfig,
    state: McpState,
    child: Option<Child>,
    next_id: u64,
    server_info: Option<ServerInfo>,
    tools: Vec<McpTool>,
}

impl McpClient {
    /// Create a new client for a server config. Does not start the process.
    pub fn new(config: ServerConfig) -> Self {
        McpClient {
            config,
            state: McpState::Disconnected,
            child: None,
            next_id: 1,
            server_info: None,
            tools: Vec::new(),
        }
    }

    /// Get the server name.
    pub fn server_name(&self) -> &str {
        &self.config.name
    }

    /// Current connection state.
    pub fn state(&self) -> &McpState {
        &self.state
    }

    /// Get cached tools.
    pub fn tools(&self) -> &[McpTool] {
        &self.tools
    }

    /// Start the server process and perform MCP handshake.
    pub async fn start(&mut self) -> Result<(), McpError> {
        if self.state == McpState::Connected {
            return Ok(());
        }

        self.state = McpState::Connecting;

        let mut cmd = Command::new(&self.config.command);
        cmd.args(&self.config.args);
        cmd.stdin(Stdio::piped());
        cmd.stdout(Stdio::piped());
        cmd.stderr(Stdio::inherit());
        cmd.kill_on_drop(true);

        for (key, value) in &self.config.env {
            cmd.env(key, value);
        }

        let child = cmd.spawn().map_err(|e| {
            McpError::Config(format!("Failed to start '{}': {}", self.config.command, e))
        })?;

        let _ = child.stdin.as_ref().ok_or_else(|| {
            McpError::Config("Failed to capture stdin".into())
        })?;

        let _ = child.stdout.as_ref().ok_or_else(|| {
            McpError::Config("Failed to capture stdout".into())
        })?;

        let mut stdin_writer = child.stdin.unwrap();
        let stdout_reader = child.stdout.unwrap();

        // Send initialize request
        let init_params = serde_json::to_value(InitializeParams {
            protocol_version: "2024-11-05".into(),
            capabilities: ClientCapabilities {
                roots: None,
                sampling: None,
            },
            client_info: ImplementationInfo {
                name: "openzen".into(),
                version: "0.1.0".into(),
            },
        }).map_err(|e| McpError::Rpc(e.to_string()))?;

        let init_req = JsonRpcRequest::new("initialize", Some(init_params));
        let req_json = serde_json::to_string(&init_req).map_err(|e| McpError::Rpc(e.to_string()))?;

        stdin_writer.write_all(req_json.as_bytes()).await.map_err(McpError::Io)?;
        stdin_writer.write_all(b"\n").await.map_err(McpError::Io)?;
        stdin_writer.flush().await.map_err(McpError::Io)?;

        // Read initialize response
        let mut reader = BufReader::new(stdout_reader);
        let mut line = String::new();
        reader.read_line(&mut line).await.map_err(McpError::Io)?;

        let resp: JsonRpcResponse = serde_json::from_str(&line)
            .map_err(|e| McpError::Rpc(format!("Initialize response parse: {}", e)))?;

        if let Some(err) = &resp.error {
            return Err(McpError::Rpc(format!("Initialize failed: {} (code {})", err.message, err.code)));
        }

        if let Some(result) = &resp.result {
            // The initialize result carries several top-level fields
            // (protocolVersion, capabilities, serverInfo, instructions).
            // Only the serverInfo subtree matches our ServerInfo struct.
            if let Some(si) = result.get("serverInfo") {
                self.server_info = serde_json::from_value(si.clone()).ok();
            }
        }

        // Send initialized notification
        let init_done = JsonRpcRequest::notification("notifications/initialized", None);
        let done_json = serde_json::to_string(&init_done).map_err(|e| McpError::Rpc(e.to_string()))?;

        stdin_writer.write_all(done_json.as_bytes()).await.map_err(McpError::Io)?;
        stdin_writer.write_all(b"\n").await.map_err(McpError::Io)?;
        stdin_writer.flush().await.map_err(McpError::Io)?;

        // List tools
        let tools_req = JsonRpcRequest::new("tools/list", None);
        let tools_json = serde_json::to_string(&tools_req).map_err(|e| McpError::Rpc(e.to_string()))?;

        stdin_writer.write_all(tools_json.as_bytes()).await.map_err(McpError::Io)?;
        stdin_writer.write_all(b"\n").await.map_err(McpError::Io)?;
        stdin_writer.flush().await.map_err(McpError::Io)?;

        line.clear();
        reader.read_line(&mut line).await.map_err(McpError::Io)?;

        let tools_resp: JsonRpcResponse = serde_json::from_str(&line)
            .map_err(|e| McpError::Rpc(format!("tools/list response parse: {}", e)))?;

        if let Some(err) = &tools_resp.error {
            return Err(McpError::Rpc(format!("tools/list failed: {}", err.message)));
        }

        if let Some(result) = &tools_resp.result {
            let tools_result: ListToolsResult = serde_json::from_value(result.clone())
                .map_err(|e| McpError::Rpc(format!("tools/list result parse: {}", e)))?;
            self.tools = tools_result.tools;
        }

        self.state = McpState::Connected;
        tracing::info!(
            "MCP server '{}' connected: {} tools available",
            self.config.name, self.tools.len()
        );

        Ok(())
    }

    /// Call a tool on the server and return the result.
    pub async fn call_tool(&mut self, name: &str, arguments: serde_json::Value) -> Result<CallToolResult, McpError> {
        if self.state != McpState::Connected {
            return Err(McpError::Rpc("Not connected".into()));
        }

        let params = serde_json::to_value(CallToolParams {
            name: name.to_string(),
            arguments,
        }).map_err(|e| McpError::Rpc(e.to_string()))?;

        let _req = JsonRpcRequest {
            jsonrpc: "2.0".into(),
            id: Some(self.next_id),
            method: "tools/call".into(),
            params: Some(params),
        };
        self.next_id += 1;

        // Re-open the communication (simplified: we assume single-request model)
        // For production, we'd keep the reader/writer alive.
        // This is a simplest-possible implementation.
        Err(McpError::Rpc(
            "call_tool requires persistent bidirectional transport — use list_tools + register approach".into()
        ))
    }

    /// Stop the server process.
    pub async fn stop(&mut self) {
        if let Some(ref mut child) = self.child.take() {
            let _ = child.kill().await;
        }
        self.state = McpState::Disconnected;
    }
}

impl Drop for McpClient {
    fn drop(&mut self) {
        if let Some(ref mut child) = self.child.take() {
            let _ = child.start_kill();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_client_new() {
        let config = ServerConfig {
            name: "test".into(),
            command: "echo".into(),
            args: vec![],
            enabled: true,
            auto_start: false,
            env: Default::default(),
        };
        let client = McpClient::new(config);
        assert_eq!(client.server_name(), "test");
        assert_eq!(client.state(), &McpState::Disconnected);
    }

    #[test]
    fn test_client_initial_state() {
        let config = ServerConfig {
            name: "test2".into(),
            command: "cat".into(),
            args: vec![],
            enabled: true,
            auto_start: true,
            env: Default::default(),
        };
        let client = McpClient::new(config);
        assert!(client.tools().is_empty());
        assert!(client.state() == &McpState::Disconnected);
    }
}
