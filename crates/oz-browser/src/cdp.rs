use std::collections::HashMap;
use std::process::{Child, Command, Stdio};
use std::sync::Arc;
use std::time::Duration;

use futures_util::{SinkExt, StreamExt};
use oz_core_types::ToolError;
use serde::{Deserialize, Serialize};
use tokio::net::TcpStream;
use tokio::sync::Mutex;
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::{connect_async, MaybeTlsStream, WebSocketStream};

type WsStream = WebSocketStream<MaybeTlsStream<TcpStream>>;
type WriteHalf = futures_util::stream::SplitSink<WsStream, Message>;

#[derive(Debug, Deserialize)]
struct CdpResponse {
    id: u64,
    #[serde(default)]
    result: Option<serde_json::Value>,
    #[serde(default)]
    error: Option<CdpErrorDetail>,
}

#[derive(Debug, Deserialize)]
struct CdpErrorDetail {
    code: i64,
    message: String,
}

#[derive(Debug, Deserialize)]
struct CdpEvent {
    method: Option<String>,
    #[allow(dead_code)]
    params: Option<serde_json::Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TargetInfo {
    pub id: String,
    #[serde(rename = "type")]
    pub type_: String,
    pub title: String,
    pub url: String,
    #[serde(default)]
    pub attached: bool,
}

struct CdpInner {
    pending: HashMap<u64, tokio::sync::oneshot::Sender<Result<serde_json::Value, String>>>,
    next_id: u64,
    closed: bool,
}

pub struct CdpClient {
    write: Arc<Mutex<WriteHalf>>,
    inner: Arc<Mutex<CdpInner>>,
    chrome_process: Option<Child>,
    #[allow(dead_code)]
    target_id: String,
    session_id: Option<String>,
}

impl CdpClient {
    pub async fn launch(chrome_path: &str, port: u16) -> Result<Self, ToolError> {
        let actual_port = if port == 0 { 0 } else { port };
        let mut cmd = Command::new(chrome_path);
        cmd.arg(format!("--remote-debugging-port={}", actual_port))
            .arg("--headless")
            .arg("--no-first-run")
            .arg("--no-default-browser-check")
            .arg("--disable-gpu")
            .arg("--disable-extensions")
            .arg("--mute-audio")
            .arg("--disable-sync")
            .arg("--disable-translate")
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        let child = cmd
            .spawn()
            .map_err(|e| ToolError::Custom(format!("Failed to launch Chrome: {e}")))?;
        // Kill the Chrome we just spawned if any step below fails —
        // otherwise every failed launch orphans a browser process.
        struct ChildGuard(Option<std::process::Child>);
        impl Drop for ChildGuard {
            fn drop(&mut self) {
                if let Some(mut c) = self.0.take() {
                    let _ = c.kill();
                    let _ = c.wait();
                }
            }
        }
        let mut guard = ChildGuard(Some(child));
        let debug_port = if port == 0 {
            Self::find_debug_port().await?
        } else {
            port
        };
        Self::wait_for_chrome(debug_port).await?;
        let ws_url = Self::get_websocket_url(debug_port).await?;
        let mut client = Self::connect_inner(&ws_url).await?;
        client.chrome_process = Some(guard.0.take().expect("child present"));
        Ok(client)
    }

    pub async fn connect(ws_url: &str) -> Result<Self, ToolError> {
        Self::connect_inner(ws_url).await
    }

    async fn connect_inner(ws_url: &str) -> Result<Self, ToolError> {
        let (ws_stream, _) = connect_async(ws_url)
            .await
            .map_err(|e| ToolError::Custom(format!("WebSocket connection failed: {e}")))?;

        let (write, mut read) = ws_stream.split();
        let write = Arc::new(Mutex::new(write));
        let inner = Arc::new(Mutex::new(CdpInner {
            pending: HashMap::new(),
            next_id: 1,
            closed: false,
        }));

        let inner_clone = inner.clone();
        tokio::spawn(async move {
            while let Some(msg) = read.next().await {
                match msg {
                    Ok(Message::Text(text)) => {
                        Self::handle_message(&inner_clone, &text).await;
                    }
                    Ok(Message::Binary(data)) => {
                        if let Ok(text) = String::from_utf8(data.to_vec()) {
                            Self::handle_message(&inner_clone, &text).await;
                        }
                    }
                    Ok(Message::Close(_)) | Err(_) => break,
                    _ => {}
                }
            }
            let mut i = inner_clone.lock().await;
            i.closed = true;
            for (_, sender) in i.pending.drain() {
                let _ = sender.send(Err("Connection closed".into()));
            }
        });

        let targets = Self::send_raw(
            &write,
            &inner,
            None,
            "Target.getTargets",
            serde_json::json!({}),
        )
        .await?;
        let target_list: Vec<TargetInfo> = serde_json::from_value(targets["targetInfos"].clone())
            .or_else(|_| serde_json::from_value(targets.clone()))
            .unwrap_or_default();

        let page_target = target_list.iter().find(|t| t.type_ == "page").cloned();

        let (target_id, session_id) = match page_target {
            Some(ref t) => {
                let sess = Self::send_raw(
                    &write,
                    &inner,
                    None,
                    "Target.attachToTarget",
                    serde_json::json!({
                        "targetId": &t.id, "flatten": true
                    }),
                )
                .await?;
                (
                    t.id.clone(),
                    sess.get("sessionId")
                        .and_then(|v| v.as_str())
                        .map(String::from),
                )
            }
            None => {
                let created = Self::send_raw(
                    &write,
                    &inner,
                    None,
                    "Target.createTarget",
                    serde_json::json!({"url": "about:blank"}),
                )
                .await?;
                let tid = created["targetId"].as_str().unwrap_or("").to_string();
                let sess = Self::send_raw(
                    &write,
                    &inner,
                    None,
                    "Target.attachToTarget",
                    serde_json::json!({
                        "targetId": &tid, "flatten": true
                    }),
                )
                .await?;
                (
                    tid,
                    sess.get("sessionId")
                        .and_then(|v| v.as_str())
                        .map(String::from),
                )
            }
        };

        Ok(CdpClient {
            write,
            inner,
            chrome_process: None,
            target_id,
            session_id,
        })
    }

    pub async fn navigate(&self, url: &str) -> Result<String, ToolError> {
        self.cmd("Page.enable", serde_json::json!({})).await?;
        let result = self
            .cmd("Page.navigate", serde_json::json!({"url": url}))
            .await?;
        if let Some(err) = result.get("errorText").and_then(|v| v.as_str()) {
            return Err(ToolError::Custom(format!("Navigation error: {err}")));
        }
        tokio::time::sleep(Duration::from_secs(2)).await;
        self.get_title().await
    }

    pub async fn evaluate_js(&self, expression: &str) -> Result<serde_json::Value, ToolError> {
        let result = self
            .cmd(
                "Runtime.evaluate",
                serde_json::json!({
                    "expression": expression, "returnByValue": true, "awaitPromise": true,
                }),
            )
            .await?;
        if let Some(exception) = result.get("exceptionDetails") {
            return Err(ToolError::Custom(format!(
                "JS error: {}",
                exception["text"].as_str().unwrap_or("unknown")
            )));
        }
        Ok(result
            .get("result")
            .and_then(|r| r.get("value"))
            .cloned()
            .unwrap_or(serde_json::Value::Null))
    }

    pub async fn get_outer_html(&self) -> Result<String, ToolError> {
        let doc = self
            .cmd("DOM.getDocument", serde_json::json!({"depth": -1}))
            .await?;
        let root_id = doc["root"]["nodeId"]
            .as_i64()
            .ok_or_else(|| ToolError::Custom("No root nodeId".into()))?;
        let result = self
            .cmd("DOM.getOuterHTML", serde_json::json!({"nodeId": root_id}))
            .await?;
        Ok(result["outerHTML"].as_str().unwrap_or("").to_string())
    }

    pub async fn get_simplified_html(&self, max_chars: usize) -> Result<String, ToolError> {
        let html = self.get_outer_html().await?;
        Ok(crate::simplify::simplify_html(&html, max_chars))
    }

    pub async fn capture_screenshot(&self) -> Result<String, ToolError> {
        let _ = self.cmd("Page.enable", serde_json::json!({})).await;
        let result = self
            .cmd(
                "Page.captureScreenshot",
                serde_json::json!({"format": "png"}),
            )
            .await?;
        Ok(result["data"].as_str().unwrap_or("").to_string())
    }

    pub async fn get_title(&self) -> Result<String, ToolError> {
        Ok(self
            .evaluate_js("document.title")
            .await?
            .as_str()
            .unwrap_or("")
            .to_string())
    }

    pub async fn get_url(&self) -> Result<String, ToolError> {
        Ok(self
            .evaluate_js("window.location.href")
            .await?
            .as_str()
            .unwrap_or("")
            .to_string())
    }

    pub async fn close(&mut self) -> Result<(), ToolError> {
        {
            let mut w = self.write.lock().await;
            let _ = w.close().await;
        }
        if let Some(mut child) = self.chrome_process.take() {
            let _ = child.kill();
            let _ = child.wait();
        }
        Ok(())
    }

    async fn cmd(
        &self,
        method: &str,
        params: serde_json::Value,
    ) -> Result<serde_json::Value, ToolError> {
        Self::send_raw(
            &self.write,
            &self.inner,
            self.session_id.as_deref(),
            method,
            params,
        )
        .await
    }

    async fn send_raw(
        write: &Arc<Mutex<WriteHalf>>,
        inner: &Arc<Mutex<CdpInner>>,
        session_id: Option<&str>,
        method: &str,
        params: serde_json::Value,
    ) -> Result<serde_json::Value, ToolError> {
        let id = {
            let mut i = inner.lock().await;
            let id = i.next_id;
            i.next_id += 1;
            id
        };
        let (tx, rx) = tokio::sync::oneshot::channel();
        {
            let mut i = inner.lock().await;
            if i.closed {
                return Err(ToolError::Custom("CDP connection closed".into()));
            }
            i.pending.insert(id, tx);
        }
        let mut msg = serde_json::json!({"id": id, "method": method, "params": params});
        if let Some(sess) = session_id {
            msg["sessionId"] = serde_json::json!(sess);
        }
        {
            let mut w = write.lock().await;
            w.send(Message::Text(msg.to_string()))
                .await
                .map_err(|e| ToolError::Custom(format!("CDP send error: {e}")))?;
        }
        let result = rx
            .await
            .map_err(|_| ToolError::Custom("CDP response channel closed".into()))?
            .map_err(ToolError::Custom)?;
        Ok(result)
    }

    async fn handle_message(inner: &Arc<Mutex<CdpInner>>, text: &str) {
        if let Ok(resp) = serde_json::from_str::<CdpResponse>(text) {
            let mut i = inner.lock().await;
            if let Some(sender) = i.pending.remove(&resp.id) {
                if let Some(err) = resp.error {
                    let _ = sender.send(Err(format!("CDP error {}: {}", err.code, err.message)));
                } else {
                    let _ = sender.send(Ok(resp.result.unwrap_or(serde_json::Value::Null)));
                }
            }
            return;
        }
        if let Ok(evt) = serde_json::from_str::<CdpEvent>(text) {
            if let Some(method) = evt.method {
                tracing::trace!("CDP event: {method}");
            }
        }
    }

    async fn find_debug_port() -> Result<u16, ToolError> {
        tokio::time::sleep(Duration::from_secs(1)).await;
        for port in [9222, 9223, 9224, 9225, 9229, 9230] {
            if TcpStream::connect(format!("127.0.0.1:{port}"))
                .await
                .is_ok()
            {
                return Ok(port);
            }
        }
        Err(ToolError::Custom("Could not find Chrome debug port".into()))
    }

    async fn wait_for_chrome(port: u16) -> Result<(), ToolError> {
        for _ in 0..30 {
            if TcpStream::connect(format!("127.0.0.1:{port}"))
                .await
                .is_ok()
            {
                return Ok(());
            }
            tokio::time::sleep(Duration::from_millis(200)).await;
        }
        Err(ToolError::Custom(format!(
            "Chrome did not start on port {port}"
        )))
    }

    async fn get_websocket_url(port: u16) -> Result<String, ToolError> {
        let url_str = format!("http://127.0.0.1:{port}/json/version");
        let resp = reqwest::get(&url_str)
            .await
            .map_err(|e| ToolError::Custom(format!("Chrome version req failed: {e}")))?;
        let data: serde_json::Value = resp
            .json()
            .await
            .map_err(|e| ToolError::Custom(format!("Chrome version parse failed: {e}")))?;
        data["webSocketDebuggerUrl"]
            .as_str()
            .map(String::from)
            .ok_or_else(|| ToolError::Custom("No webSocketDebuggerUrl".into()))
    }
}

impl Drop for CdpClient {
    fn drop(&mut self) {
        if let Some(mut child) = self.chrome_process.take() {
            let _ = child.kill();
            let _ = child.wait();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_cdp_response_deserialize() {
        let json = r#"{"id": 1, "result": {"outerHTML": "<html></html>"}}"#;
        let resp: CdpResponse = serde_json::from_str(json).unwrap();
        assert_eq!(resp.id, 1);
        assert!(resp.result.is_some());
        assert!(resp.error.is_none());
    }

    #[test]
    fn test_cdp_response_error() {
        let json = r#"{"id": 2, "error": {"code": -32000, "message": "Not found"}}"#;
        let resp: CdpResponse = serde_json::from_str(json).unwrap();
        assert_eq!(resp.error.unwrap().message, "Not found");
    }

    #[test]
    fn test_cdp_event() {
        let json = r#"{"method": "Page.loadEventFired", "params": {"ts": 1.0}}"#;
        let evt: CdpEvent = serde_json::from_str(json).unwrap();
        assert_eq!(evt.method.as_deref(), Some("Page.loadEventFired"));
    }

    #[test]
    fn test_target_info() {
        let json = r#"{"id": "x", "type": "page", "title": "T", "url": "https://e.com"}"#;
        let t: TargetInfo = serde_json::from_str(json).unwrap();
        assert_eq!(t.id, "x");
        assert_eq!(t.type_, "page");
    }
}
