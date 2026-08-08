use std::convert::Infallible;

use axum::{
    Router,
    extract::State,
    http::StatusCode,
    response::{
        sse::{Event, Sse},
        Html, Json,
    },
    routing::{get, post},
};
use futures::stream::Stream;
use futures::StreamExt;
use serde::Deserialize;
use tokio_stream::wrappers::ReceiverStream;

use crate::SharedMcpState;
use crate::types::JsonRpcMessage;

/// Start the SSE-based MCP server on the given port.
pub async fn serve(state: SharedMcpState, port: u16) -> anyhow::Result<()> {
    let app = Router::new()
        .route("/", get(index))
        .route("/health", get(health))
        .route("/tools", get(list_tools))
        .route("/tools/call", post(call_tool))
        .route("/sse", get(sse_handler))
        .route("/messages", post(messages_handler))
        .layer(
            tower_http::cors::CorsLayer::permissive()
        )
        .with_state(state);

    let addr = format!("0.0.0.0:{port}");
    tracing::info!("MCP SSE server listening on {addr}");

    let listener = tokio::net::TcpListener::bind(&addr).await?;
    axum::serve(listener, app).await?;

    Ok(())
}

async fn index() -> Html<&'static str> {
    Html(r#"<!DOCTYPE html>
<html><head><title>OpenZen MCP Server</title></head>
<body><h1>OpenZen MCP Server</h1>
<p>SSE endpoint: <code>/sse</code></p>
<p>Messages endpoint: <code>/messages</code></p>
<p>Tools: <code>/tools</code></p>
</body></html>"#)
}

async fn health() -> Json<serde_json::Value> {
    Json(serde_json::json!({"status": "ok"}))
}

async fn list_tools(
    State(state): State<SharedMcpState>,
) -> Json<serde_json::Value> {
    let state = state.lock().await;
    let tools = state.tool_definitions();
    Json(serde_json::json!({
        "tools": tools
    }))
}

#[derive(Deserialize)]
struct CallToolRequest {
    name: String,
    arguments: serde_json::Value,
}

async fn call_tool(
    State(state): State<SharedMcpState>,
    Json(req): Json<CallToolRequest>,
) -> Json<serde_json::Value> {
    let state = state.lock().await;
    match state.call_tool(&req.name, req.arguments).await {
        Ok(result) => Json(serde_json::json!({
            "result": result,
            "isError": false,
        })),
        Err(e) => Json(serde_json::json!({
            "result": null,
            "isError": true,
            "error": e,
        })),
    }
}

async fn sse_handler(
    State(_state): State<SharedMcpState>,
) -> Sse<impl Stream<Item = Result<Event, Infallible>>> {
    let (tx, rx) = tokio::sync::mpsc::channel::<String>(32);

    let msg = serde_json::json!({
        "type": "endpoint",
        "endpoint": "/messages"
    });
    let _ = tx.send(format!("data: {}\n\n", serde_json::to_string(&msg).unwrap_or_default())).await;

    tokio::spawn(async move {
        loop {
            tokio::time::sleep(tokio::time::Duration::from_secs(15)).await;
            if tx.send(": keepalive\n\n".to_string()).await.is_err() {
                break;
            }
        }
    });

    let stream = ReceiverStream::new(rx).map(|msg| Ok(Event::default().data(msg)));
    Sse::new(stream)
}

async fn messages_handler(
    State(state): State<SharedMcpState>,
    Json(msg): Json<JsonRpcMessage>,
) -> Result<Json<JsonRpcMessage>, StatusCode> {
    let method = msg.method.as_deref().unwrap_or("");
    let id = msg.id.unwrap_or(0);

    match method {
        "tools/list" => {
            let state = state.lock().await;
            let tools = state.tool_definitions();
            Ok(Json(JsonRpcMessage::success(id, serde_json::json!({ "tools": tools }))))
        }
        "tools/call" => {
            let params = msg.params.unwrap_or_default();
            let name = params.get("name").and_then(|v| v.as_str()).unwrap_or("");
            let args = params.get("arguments").cloned().unwrap_or_default();
            let state = state.lock().await;
            match state.call_tool(name, args).await {
                Ok(result) => Ok(Json(JsonRpcMessage::success(id, serde_json::json!({
                    "content": [{"type": "text", "text": serde_json::to_string(&result).unwrap_or_default()}]
                })))),
                Err(e) => Ok(Json(JsonRpcMessage::error(id, -1, e))),
            }
        }
        "initialize" => {
            Ok(Json(JsonRpcMessage::success(id, serde_json::json!({
                "protocolVersion": "2024-11-05",
                "capabilities": {
                    "tools": {}
                },
                "serverInfo": {
                    "name": "openzen",
                    "version": "0.1.0"
                }
            }))))
        }
        "ping" => {
            Ok(Json(JsonRpcMessage::success(id, serde_json::json!({}))))
        }
        _ => {
            Ok(Json(JsonRpcMessage::error(id, -32601, format!("method not found: {method}"))))
        }
    }
}
