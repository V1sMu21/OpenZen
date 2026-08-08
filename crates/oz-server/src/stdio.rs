use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

use crate::types::JsonRpcMessage;
use crate::SharedMcpState;

/// Run MCP server over stdin/stdout.
pub async fn serve(state: SharedMcpState) -> anyhow::Result<()> {
    let stdin = tokio::io::stdin();
    let reader = BufReader::new(stdin);
    let mut lines = reader.lines();
    let mut stdout = tokio::io::stdout();

    while let Some(line) = lines.next_line().await? {
        if line.trim().is_empty() {
            continue;
        }

        let msg: JsonRpcMessage = match serde_json::from_str(&line) {
            Ok(m) => m,
            Err(e) => {
                let err = JsonRpcMessage::error(0, -32700, format!("parse error: {e}"));
                let out = serde_json::to_string(&err)?;
                stdout.write_all(out.as_bytes()).await?;
                stdout.write_all(b"\n").await?;
                stdout.flush().await?;
                continue;
            }
        };

        let response = handle_message(&state, msg).await;
        if let Some(resp) = response {
            let out = serde_json::to_string(&resp)?;
            stdout.write_all(out.as_bytes()).await?;
            stdout.write_all(b"\n").await?;
            stdout.flush().await?;
        }
    }

    Ok(())
}

async fn handle_message(
    state: &SharedMcpState,
    msg: JsonRpcMessage,
) -> Option<JsonRpcMessage> {
    let method = msg.method.as_deref().unwrap_or("");
    let id = msg.id.unwrap_or(0);

    match method {
        "tools/list" => {
            let s = state.lock().await;
            let tools = s.tool_definitions();
            Some(JsonRpcMessage::success(id, serde_json::json!({ "tools": tools })))
        }
        "tools/call" => {
            let params = msg.params.unwrap_or_default();
            let name = params.get("name").and_then(|v| v.as_str()).unwrap_or("");
            let args = params.get("arguments").cloned().unwrap_or_default();
            let s = state.lock().await;
            match s.call_tool(name, args).await {
                Ok(result) => Some(JsonRpcMessage::success(id, serde_json::json!({
                    "content": [{"type": "text", "text": serde_json::to_string(&result).unwrap_or_default()}]
                }))),
                Err(e) => Some(JsonRpcMessage::error(id, -1, e)),
            }
        }
        "initialize" => {
            Some(JsonRpcMessage::success(id, serde_json::json!({
                "protocolVersion": "2024-11-05",
                "capabilities": {
                    "tools": {}
                },
                "serverInfo": {
                    "name": "openzen",
                    "version": "0.1.0"
                }
            })))
        }
        "ping" => {
            Some(JsonRpcMessage::success(id, serde_json::json!({})))
        }
        "notifications/initialized" => {
            None
        }
        _ => {
            Some(JsonRpcMessage::error(id, -32601, format!("method not found: {method}")))
        }
    }
}
