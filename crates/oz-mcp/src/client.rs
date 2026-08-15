//! MCP Client — JSON-RPC over stdio transport.
//!
//! Manages a single MCP server process. Communicates via stdin/stdout
//! using the MCP JSON-RPC protocol.

use std::process::Stdio;

use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::{Child, ChildStdin, ChildStdout, Command};

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
    stdin: Option<ChildStdin>,
    stdout: Option<BufReader<ChildStdout>>,
    next_id: u64,
    tools: Vec<McpTool>,
}

impl McpClient {
    /// Create a new client for a server config. Does not start the process.
    pub fn new(config: ServerConfig) -> Self {
        McpClient {
            config,
            state: McpState::Disconnected,
            child: None,
            stdin: None,
            stdout: None,
            next_id: 1,
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

        let mut child = cmd.spawn().map_err(|e| {
            McpError::Config(format!("Failed to start '{}': {}", self.config.command, e))
        })?;

        let stdin = child.stdin.take().ok_or_else(|| {
            McpError::Config("Failed to capture stdin".into())
        })?;
        let stdout = BufReader::new(child.stdout.take().ok_or_else(|| {
            McpError::Config("Failed to capture stdout".into())
        })?);

        self.child = Some(child);
        self.stdin = Some(stdin);
        self.stdout = Some(stdout);

        self.send_request("initialize", Some(self.initialize_params()?)).await?;
        self.send_notification("notifications/initialized", None).await?;
        self.list_tools().await?;

        self.state = McpState::Connected;
        tracing::info!(
            "MCP server '{}' connected: {} tools available",
            self.config.name, self.tools.len()
        );

        Ok(())
    }

    fn initialize_params(&self) -> Result<serde_json::Value, McpError> {
        serde_json::to_value(InitializeParams {
            protocol_version: "2024-11-05".into(),
            capabilities: ClientCapabilities {
                roots: None,
                sampling: None,
            },
            client_info: ImplementationInfo {
                name: "openzen".into(),
                version: "0.1.0".into(),
            },
        }).map_err(|e| McpError::Rpc(e.to_string()))
    }

    async fn send_request(&mut self, method: &str, params: Option<serde_json::Value>) -> Result<serde_json::Value, McpError> {
        let id = self.next_id;
        self.next_id += 1;
        let req = JsonRpcRequest {
            jsonrpc: "2.0".into(),
            id: Some(id),
            method: method.into(),
            params,
        };
        self.write_line(&serde_json::to_string(&req).map_err(|e| McpError::Rpc(e.to_string()))?).await?;
        let resp = self.read_response(id).await?;
        if let Some(err) = &resp.error {
            return Err(McpError::Rpc(format!("{method} failed: {} (code {})", err.message, err.code)));
        }
        resp.result.ok_or_else(|| McpError::Rpc(format!("{method}: empty result")))
    }

    async fn send_notification(&mut self, method: &str, params: Option<serde_json::Value>) -> Result<(), McpError> {
        let req = JsonRpcRequest {
            jsonrpc: "2.0".into(),
            id: None,
            method: method.into(),
            params,
        };
        self.write_line(&serde_json::to_string(&req).map_err(|e| McpError::Rpc(e.to_string()))?).await
    }

    async fn write_line(&mut self, line: &str) -> Result<(), McpError> {
        let stdin = self.stdin.as_mut().ok_or_else(|| McpError::Rpc("Not connected".into()))?;
        stdin.write_all(line.as_bytes()).await.map_err(McpError::Io)?;
        stdin.write_all(b"\n").await.map_err(McpError::Io)?;
        stdin.flush().await.map_err(McpError::Io)
    }

    async fn read_response(&mut self, expected_id: u64) -> Result<JsonRpcResponse, McpError> {
        let stdout = self.stdout.as_mut().ok_or_else(|| McpError::Rpc("Not connected".into()))?;
        loop {
            let mut line = String::new();
            stdout.read_line(&mut line).await.map_err(McpError::Io)?;
            if line.trim().is_empty() {
                continue;
            }
            let resp: JsonRpcResponse = serde_json::from_str(&line)
                .map_err(|e| McpError::Rpc(format!("Response parse: {e}")))?;
            // Skip server-initiated notifications (no id); wait for our id.
            if resp.id == Some(expected_id) {
                return Ok(resp);
            }
        }
    }

    async fn list_tools(&mut self) -> Result<(), McpError> {
        let result = self.send_request("tools/list", None).await?;
        let tools_result: ListToolsResult = serde_json::from_value(result)
            .map_err(|e| McpError::Rpc(format!("tools/list result parse: {e}")))?;
        self.tools = tools_result.tools;
        Ok(())
    }

    /// Call a tool on the server and return the result.
    pub async fn call_tool(&mut self, name: &str, arguments: serde_json::Value) -> Result<CallToolResult, McpError> {
        let params = serde_json::to_value(CallToolParams {
            name: name.to_string(),
            arguments,
        }).map_err(|e| McpError::Rpc(e.to_string()))?;
        let result = self.send_request("tools/call", Some(params)).await?;
        serde_json::from_value(result)
            .map_err(|e| McpError::Rpc(format!("tools/call result parse: {e}")))
    }

    /// Stop the server process.
    pub async fn stop(&mut self) {
        if let Some(ref mut child) = self.child.take() {
            let _ = child.kill().await;
        }
        self.stdin = None;
        self.stdout = None;
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
