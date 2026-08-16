use std::path::PathBuf;

use base64::Engine;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

const API_BASE: &str = "https://ilinkai.weixin.qq.com";
const ILINK_APP_ID: &str = "bot";
const ILINK_CLIENT_VERSION: u32 = (2 << 16) | (1 << 8) | 10;
const UA: &str = "openzen-weixin/2.1.10";
const TOKEN_DIR: &str = ".wxbot";
const TOKEN_FILE: &str = "token.json";
const MSG_USER: i32 = 1;
const MSG_BOT: i32 = 2;
const ITEM_TEXT: i32 = 1;
const STATE_FINISH: i32 = 2;
const CDN_BASE: &str = "https://novac2c.cdn.weixin.qq.com/c2c";

#[derive(Clone)]
pub struct WxBotClient {
    pub token: Option<String>,
    pub bot_id: Option<String>,
    updates_buf: String,
    token_path: PathBuf,
    http: reqwest::Client,
}

#[derive(Debug, Serialize, Deserialize)]
struct TokenData {
    bot_token: String,
    ilink_bot_id: Option<String>,
    updates_buf: String,
    login_time: Option<String>,
}

#[derive(Debug, Deserialize)]
struct QrCodeResponse {
    qrcode: String,
    qrcode_img_content: Option<String>,
}

#[derive(Debug, Deserialize)]
struct QrStatusResponse {
    status: String,
    bot_token: Option<String>,
    ilink_bot_id: Option<String>,
}

#[derive(Debug, Deserialize)]
struct UpdatesResponse {
    errcode: Option<i32>,
    get_updates_buf: Option<String>,
    msgs: Option<Vec<serde_json::Value>>,
}

#[derive(Debug, Deserialize)]
struct GetUploadUrlResponse {
    upload_param: Option<String>,
    upload_full_url: Option<String>,
}

impl WxBotClient {
    pub fn new() -> Self {
        let home = std::env::var("HOME").unwrap_or_else(|_| "/tmp".into());
        let token_path = PathBuf::from(&home).join(TOKEN_DIR).join(TOKEN_FILE);
        let mut client = WxBotClient {
            token: None,
            bot_id: None,
            updates_buf: String::new(),
            token_path,
            // Global timeouts: a hung request used to block the poll loop
            // forever (no supervisor restart, no health signal — the
            // channel went dark until process restart).
            http: reqwest::Client::builder()
                .connect_timeout(std::time::Duration::from_secs(5))
                .timeout(std::time::Duration::from_secs(65))
                .build()
                .unwrap_or_else(|_| reqwest::Client::new()),
        };
        client.load_token();
        client
    }

    fn load_token(&mut self) {
        if let Ok(data) = std::fs::read_to_string(&self.token_path) {
            if let Ok(td) = serde_json::from_str::<TokenData>(&data) {
                self.token = Some(td.bot_token);
                self.bot_id = td.ilink_bot_id;
                self.updates_buf = td.updates_buf;
            }
        }
    }

    fn save_token(&self) {
        if let Some(parent) = self.token_path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        let td = TokenData {
            bot_token: self.token.clone().unwrap_or_default(),
            ilink_bot_id: self.bot_id.clone(),
            updates_buf: self.updates_buf.clone(),
            login_time: Some(chrono::Local::now().format("%Y-%m-%d %H:%M:%S").to_string()),
        };
        if let Ok(json) = serde_json::to_string_pretty(&td) {
            let _ = std::fs::write(&self.token_path, json);
        }
    }

    pub fn is_logged_in(&self) -> bool {
        self.token.is_some()
    }

    pub async fn qr_login(&mut self) -> Result<(), String> {
        let resp = self
            .http
            .get(format!("{API_BASE}/ilink/bot/get_bot_qrcode"))
            .query(&[("bot_type", "3")])
            .header("User-Agent", UA)
            .send()
            .await
            .map_err(|e| format!("qr request error: {e}"))?;

        let body: QrCodeResponse = resp
            .json()
            .await
            .map_err(|e| format!("qr parse error: {e}"))?;

        let qr_id = body.qrcode;
        let qr_url = body.qrcode_img_content.unwrap_or_default();

        if !qr_url.is_empty() {
            tracing::info!("[WeChat] QR code URL: {qr_url}");
            tracing::info!("[WeChat] Opening QR code in browser...");
            // Open the QR code URL in the default browser so the user can scan it.
            #[cfg(target_os = "macos")]
            {
                let _ = std::process::Command::new("open").arg(&qr_url).spawn();
            }
            #[cfg(target_os = "linux")]
            {
                let _ = std::process::Command::new("xdg-open").arg(&qr_url).spawn();
            }
            #[cfg(target_os = "windows")]
            {
                let _ = std::process::Command::new("cmd")
                    .args(["/c", "start", &qr_url])
                    .spawn();
            }
        }
        tracing::info!("[WeChat] QR ID: {qr_id}");

        let mut last_status = String::new();
        loop {
            tokio::time::sleep(std::time::Duration::from_secs(2)).await;

            let resp = self
                .http
                .get(format!("{API_BASE}/ilink/bot/get_qrcode_status"))
                .query(&[("qrcode", &qr_id)])
                .header("User-Agent", UA)
                .timeout(std::time::Duration::from_secs(60))
                .send()
                .await;

            let resp = match resp {
                Ok(r) => r,
                Err(_) => continue,
            };

            let body: QrStatusResponse = match resp.json().await {
                Ok(b) => b,
                Err(_) => continue,
            };

            if body.status != last_status {
                println!("[WeChat] QR status: {}", body.status);
                last_status = body.status.clone();
            }

            match body.status.as_str() {
                "confirmed" => {
                    self.token = body.bot_token;
                    self.bot_id = body.ilink_bot_id;
                    self.save_token();
                    println!(
                        "[WeChat] Login success! bot_id={}",
                        self.bot_id.as_deref().unwrap_or("?")
                    );
                    return Ok(());
                }
                "expired" => return Err("QR code expired".into()),
                _ => {}
            }
        }
    }

    pub async fn get_updates(
        &mut self,
        timeout_secs: u64,
    ) -> Result<Vec<serde_json::Value>, String> {
        let body = serde_json::json!({
            "get_updates_buf": self.updates_buf,
            "base_info": { "channel_version": "2.1.10" },
        });

        let resp = self
            .post("ilink/bot/getupdates", &body, Some(timeout_secs + 5))
            .await;

        match resp {
            Ok(data) => {
                let result: UpdatesResponse = serde_json::from_value(data)
                    .map_err(|e| format!("updates parse error: {e}"))?;

                if let Some(errcode) = result.errcode {
                    if errcode != 0 {
                        tracing::info!("[WeChat] get_updates errcode={}", errcode);
                        if errcode == -14 {
                            self.updates_buf.clear();
                            self.save_token();
                        }
                        return Ok(Vec::new());
                    }
                }

                if let Some(buf) = result.get_updates_buf {
                    if !buf.is_empty() {
                        self.updates_buf = buf;
                        self.save_token();
                    }
                }

                Ok(result.msgs.unwrap_or_default())
            }
            Err(e) => {
                if e.contains("timeout") || e.contains("Timeout") {
                    Ok(Vec::new())
                } else {
                    Err(e)
                }
            }
        }
    }

    pub async fn send_text(
        &self,
        to_user_id: &str,
        text: &str,
        context_token: &str,
    ) -> Result<(), String> {
        let msg = serde_json::json!({
            "from_user_id": "",
            "to_user_id": to_user_id,
            "client_id": format!("rsclient-{}", Uuid::new_v4().to_string().replace('-', "")[..16].to_string()),
            "message_type": MSG_BOT,
            "message_state": STATE_FINISH,
            "item_list": [{
                "type": ITEM_TEXT,
                "text_item": { "text": text }
            }],
            "context_token": context_token,
        });

        let body = serde_json::json!({
            "msg": msg,
            "base_info": { "channel_version": "2.1.10" },
        });

        self.post("ilink/bot/sendmessage", &body, Some(15)).await?;
        Ok(())
    }

    pub async fn send_image(
        &self,
        to_user_id: &str,
        file_path: &std::path::Path,
        context_token: &str,
    ) -> Result<(), String> {
        self.send_media(to_user_id, file_path, 1, "image_item", context_token)
            .await
    }

    pub async fn send_file(
        &self,
        to_user_id: &str,
        file_path: &std::path::Path,
        context_token: &str,
    ) -> Result<(), String> {
        self.send_media(to_user_id, file_path, 3, "file_item", context_token)
            .await
    }

    async fn send_media(
        &self,
        to_user_id: &str,
        file_path: &std::path::Path,
        media_type: i32,
        item_key: &str,
        context_token: &str,
    ) -> Result<(), String> {
        let raw = std::fs::read(file_path).map_err(|e| format!("read file error: {e}"))?;

        let filekey = Uuid::new_v4().to_string().replace('-', "");
        let aes_key = crate::crypto::random_key();
        let ciphertext_size = ((raw.len() / 16) + 1) * 16;

        let md5 = format!("{:x}", md5::compute(&raw));
        let body = serde_json::json!({
            "filekey": filekey,
            "media_type": media_type,
            "to_user_id": to_user_id,
            "rawsize": raw.len(),
            "rawfilemd5": md5,
            "filesize": ciphertext_size,
            "no_need_thumb": item_key != "image_item",
            "aeskey": crate::crypto::key_to_hex(&aes_key),
            "base_info": { "channel_version": "2.1.10" },
        });

        let data = self.post("ilink/bot/getuploadurl", &body, Some(15)).await?;
        let upload_resp: GetUploadUrlResponse =
            serde_json::from_value(data).map_err(|e| format!("getuploadurl parse error: {e}"))?;

        let upload_param = upload_resp.upload_param.unwrap_or_default();
        let upload_url = upload_resp.upload_full_url.unwrap_or_default();
        if upload_param.is_empty() && upload_url.is_empty() {
            return Err("getuploadurl returned no upload info".into());
        }

        let encrypted = crate::crypto::aes_ecb_encrypt(&raw, &aes_key);
        let cdn_url = if !upload_url.is_empty() {
            upload_url
        } else {
            let quoted = url_escape(&upload_param);
            format!("{CDN_BASE}/upload?encrypted_query_param={quoted}&filekey={filekey}")
        };

        let resp = self
            .http
            .post(&cdn_url)
            .header("Content-Type", "application/octet-stream")
            .header("User-Agent", UA)
            .body(encrypted)
            .timeout(std::time::Duration::from_secs(120))
            .send()
            .await
            .map_err(|e| format!("CDN upload error: {e}"))?;

        let status = resp.status();
        if !status.is_success() {
            let msg = resp
                .headers()
                .get("x-error-message")
                .and_then(|v| v.to_str().ok())
                .map(|s| s.to_string())
                .unwrap_or_else(|| format!("HTTP {status}"));
            return Err(format!("CDN upload failed: {msg}"));
        }

        let encrypt_query_param = resp
            .headers()
            .get("x-encrypted-param")
            .and_then(|v| v.to_str().ok())
            .map(|s| s.to_string())
            .ok_or("CDN upload missing x-encrypted-param")?;

        let aes_key_b64 = base64::engine::general_purpose::STANDARD
            .encode(crate::crypto::key_to_hex(&aes_key).as_bytes());

        let media = serde_json::json!({
            "encrypt_query_param": encrypt_query_param,
            "aes_key": aes_key_b64,
            "encrypt_type": 1,
        });

        let mut item = serde_json::json!({ "media": media });
        if item_key == "file_item" {
            item["file_name"] = serde_json::Value::String(
                file_path
                    .file_name()
                    .unwrap_or_default()
                    .to_string_lossy()
                    .to_string(),
            );
            item["len"] = serde_json::Value::String(raw.len().to_string());
        }

        let msg_body = serde_json::json!({
            "msg": {
                "from_user_id": "",
                "to_user_id": to_user_id,
                "client_id": format!("rsmedia-{}", Uuid::new_v4().to_string().replace('-', "")[..16].to_string()),
                "message_type": MSG_BOT,
                "message_state": STATE_FINISH,
                "item_list": [{
                    "type": if item_key == "image_item" { 2 } else { 4 },
                    item_key: item,
                }],
                "context_token": context_token,
            },
            "base_info": { "channel_version": "2.1.10" },
        });

        self.post("ilink/bot/sendmessage", &msg_body, Some(30))
            .await?;
        Ok(())
    }

    pub fn extract_text(msg: &serde_json::Value) -> String {
        msg.get("item_list")
            .and_then(|v| v.as_array())
            .map(|items| {
                items
                    .iter()
                    .filter_map(|item| {
                        if item.get("type").and_then(|t| t.as_i64()) == Some(ITEM_TEXT as i64) {
                            item.get("text_item")
                                .and_then(|t| t.get("text"))
                                .and_then(|t| t.as_str())
                        } else {
                            None
                        }
                    })
                    .collect::<Vec<_>>()
                    .join("\n")
            })
            .unwrap_or_default()
    }

    pub fn is_user_msg(msg: &serde_json::Value) -> bool {
        msg.get("message_type").and_then(|v| v.as_i64()) == Some(MSG_USER as i64)
    }

    fn make_uin() -> String {
        let n: u32 = fast_rand();
        base64::engine::general_purpose::STANDARD.encode(n.to_string().as_bytes())
    }

    fn build_headers(&self) -> reqwest::header::HeaderMap {
        use reqwest::header::{HeaderMap, HeaderValue, AUTHORIZATION, CONTENT_TYPE, USER_AGENT};
        let mut headers = HeaderMap::new();
        headers.insert(CONTENT_TYPE, HeaderValue::from_static("application/json"));
        headers.insert(
            "AuthorizationType",
            HeaderValue::from_static("ilink_bot_token"),
        );
        headers.insert(
            "X-WECHAT-UIN",
            HeaderValue::from_str(&Self::make_uin()).unwrap_or(HeaderValue::from_static("0")),
        );
        headers.insert("iLink-App-Id", HeaderValue::from_static(ILINK_APP_ID));
        headers.insert(
            "iLink-App-ClientVersion",
            HeaderValue::from_str(&ILINK_CLIENT_VERSION.to_string())
                .unwrap_or(HeaderValue::from_static("0")),
        );
        headers.insert(USER_AGENT, HeaderValue::from_static(UA));
        if let Some(ref token) = self.token {
            if !token.is_empty() {
                if let Ok(val) = HeaderValue::from_str(&format!("Bearer {token}")) {
                    headers.insert(AUTHORIZATION, val);
                }
            }
        }
        headers
    }

    async fn post(
        &self,
        endpoint: &str,
        body: &serde_json::Value,
        timeout_secs: Option<u64>,
    ) -> Result<serde_json::Value, String> {
        let data = serde_json::to_string(body)
            .map_err(|e| format!("json serialize error: {e}"))?
            .into_bytes();

        let headers = self.build_headers();
        let url = format!("{API_BASE}/{endpoint}");

        let mut req = self.http.post(&url).headers(headers).body(data);
        if let Some(t) = timeout_secs {
            req = req.timeout(std::time::Duration::from_secs(t));
        }

        let resp = req.send().await.map_err(|e| {
            if e.is_timeout() {
                "timeout".to_string()
            } else {
                format!("http error: {e}")
            }
        })?;

        resp.json()
            .await
            .map_err(|e| format!("json parse error: {e}"))
    }
}

fn url_escape(s: &str) -> String {
    s.chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' || c == '_' || c == '.' || c == '~' {
                c.to_string()
            } else {
                format!("%{:02X}", c as u8)
            }
        })
        .collect()
}

fn fast_rand() -> u32 {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.subsec_nanos())
        .unwrap_or(0);
    nanos.wrapping_mul(1103515245).wrapping_add(12345)
}
