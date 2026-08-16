use std::sync::Mutex;
use std::time::Instant;

use serde::{Deserialize, Serialize};

const FEISHU_API_BASE: &str = "https://open.feishu.cn/open-apis";
const FEISHU_WS_BASE: &str = "https://open.feishu.cn";

pub struct FeishuClient {
    app_id: String,
    app_secret: String,
    http: reqwest::Client,
    access_token: Mutex<Option<(String, Instant)>>,
}

#[derive(Debug, Deserialize)]
struct TenantTokenResponse {
    code: i32,
    msg: Option<String>,
    tenant_access_token: Option<String>,
    expire: Option<i64>,
}

#[derive(Debug, Serialize)]
struct SendMessageBody {
    receive_id: String,
    msg_type: String,
    content: String,
}

#[derive(Debug, Deserialize)]
struct SendMessageResponse {
    code: i32,
    msg: Option<String>,
    data: Option<SendMessageData>,
}

#[derive(Debug, Deserialize)]
struct SendMessageData {
    message_id: Option<String>,
}

#[derive(Debug, Deserialize)]
struct PatchMessageResponse {
    code: i32,
    msg: Option<String>,
}

#[derive(Debug, Serialize)]
struct PatchMessageBody {
    content: String,
}

#[derive(Debug, Deserialize)]
pub struct ImageUploadResponseData {
    pub image_key: Option<String>,
}

#[derive(Debug, Deserialize)]
struct ImageUploadResponse {
    code: i32,
    msg: Option<String>,
    data: Option<ImageUploadResponseData>,
}

#[derive(Debug, Deserialize)]
pub struct FileUploadResponseData {
    pub file_key: Option<String>,
}

#[derive(Debug, Deserialize)]
struct FileUploadResponse {
    code: i32,
    msg: Option<String>,
    data: Option<FileUploadResponseData>,
}

#[derive(Debug, Deserialize)]
pub struct WsEndpointData {
    #[serde(rename = "URL")]
    pub url: Option<String>,
}

impl FeishuClient {
    pub fn new(app_id: String, app_secret: String) -> Self {
        // Timeouts are mandatory here: every call runs inside the serial
        // message-processing loop, and reqwest's default "no timeout" means
        // one half-open TCP connection freezes the entire Feishu channel.
        let http = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(15))
            .connect_timeout(std::time::Duration::from_secs(5))
            .build()
            .unwrap_or_else(|_| reqwest::Client::new());
        FeishuClient {
            app_id,
            app_secret,
            http,
            access_token: Mutex::new(None),
        }
    }

    pub async fn send_text(
        &self,
        receive_id: &str,
        text: &str,
        receive_id_type: &str,
    ) -> Result<Option<String>, String> {
        let content = serde_json::json!({ "text": text }).to_string();
        self.send_raw(receive_id, &content, "text", receive_id_type)
            .await
    }

    pub async fn send_card(
        &self,
        receive_id: &str,
        card_json: &str,
        receive_id_type: &str,
    ) -> Result<Option<String>, String> {
        self.send_raw(receive_id, card_json, "interactive", receive_id_type)
            .await
    }

    pub async fn patch_card(&self, message_id: &str, card_json: &str) -> Result<bool, String> {
        let token = self.get_token().await?;
        let url = format!("{FEISHU_API_BASE}/im/v1/messages/{message_id}");

        let resp = self
            .http
            .patch(&url)
            .header("Authorization", format!("Bearer {token}"))
            .json(&PatchMessageBody {
                content: card_json.to_string(),
            })
            .send()
            .await
            .map_err(|e| format!("patch_card network error: {e}"))?;

        let body: PatchMessageResponse = resp
            .json()
            .await
            .map_err(|e| format!("patch_card parse error: {e}"))?;

        if body.code == 0 {
            Ok(true)
        } else {
            let msg = format!("{} {}", body.code, body.msg.unwrap_or_default());
            let is_limit =
                msg.contains("230099") || msg.contains("11310") || msg.contains("element exceeds");
            if is_limit {
                Err(format!("card_limit: {msg}"))
            } else {
                tracing::warn!("[feishu] patch_card failed: {msg}");
                Ok(false)
            }
        }
    }

    pub async fn upload_image(&self, file_path: &std::path::Path) -> Result<String, String> {
        let token = self.get_token().await?;
        let url = format!("{FEISHU_API_BASE}/im/v1/images");

        let file_bytes =
            std::fs::read(file_path).map_err(|e| format!("read image file error: {e}"))?;
        let file_name = file_path
            .file_name()
            .unwrap_or_default()
            .to_string_lossy()
            .to_string();

        let form = reqwest::multipart::Form::new()
            .text("image_type", "message")
            .part(
                "image",
                reqwest::multipart::Part::bytes(file_bytes).file_name(file_name),
            );

        let resp = self
            .http
            .post(&url)
            .header("Authorization", format!("Bearer {token}"))
            .multipart(form)
            .send()
            .await
            .map_err(|e| format!("upload_image network error: {e}"))?;

        let body: ImageUploadResponse = resp
            .json()
            .await
            .map_err(|e| format!("upload_image parse error: {e}"))?;

        if body.code == 0 {
            body.data
                .and_then(|d| d.image_key)
                .ok_or_else(|| "upload_image: no image_key in response".into())
        } else {
            Err(format!(
                "upload_image failed: {} {}",
                body.code,
                body.msg.unwrap_or_default()
            ))
        }
    }

    pub async fn upload_file(&self, file_path: &std::path::Path) -> Result<String, String> {
        let token = self.get_token().await?;
        let url = format!("{FEISHU_API_BASE}/im/v1/files");

        let file_bytes = std::fs::read(file_path).map_err(|e| format!("read file error: {e}"))?;
        let file_name = file_path
            .file_name()
            .unwrap_or_default()
            .to_string_lossy()
            .to_string();

        let ext = file_path
            .extension()
            .and_then(|e| e.to_str())
            .unwrap_or("")
            .to_lowercase();
        let file_type = match ext.as_str() {
            "opus" => "opus",
            "mp4" => "mp4",
            "pdf" => "pdf",
            "doc" | "docx" => "doc",
            "xls" | "xlsx" => "xls",
            "ppt" | "pptx" => "ppt",
            _ => "stream",
        };

        let file_name_clone = file_name.clone();
        let form = reqwest::multipart::Form::new()
            .text("file_type", file_type)
            .text("file_name", file_name)
            .part(
                "file",
                reqwest::multipart::Part::bytes(file_bytes).file_name(file_name_clone),
            );

        let resp = self
            .http
            .post(&url)
            .header("Authorization", format!("Bearer {token}"))
            .multipart(form)
            .send()
            .await
            .map_err(|e| format!("upload_file network error: {e}"))?;

        let body: FileUploadResponse = resp
            .json()
            .await
            .map_err(|e| format!("upload_file parse error: {e}"))?;

        if body.code == 0 {
            body.data
                .and_then(|d| d.file_key)
                .ok_or_else(|| "upload_file: no file_key in response".into())
        } else {
            Err(format!(
                "upload_file failed: {} {}",
                body.code,
                body.msg.unwrap_or_default()
            ))
        }
    }

    pub async fn send_raw(
        &self,
        receive_id: &str,
        content: &str,
        msg_type: &str,
        receive_id_type: &str,
    ) -> Result<Option<String>, String> {
        let token = self.get_token().await?;
        let url = format!("{FEISHU_API_BASE}/im/v1/messages?receive_id_type={receive_id_type}");

        let body = SendMessageBody {
            receive_id: receive_id.to_string(),
            msg_type: msg_type.to_string(),
            content: content.to_string(),
        };

        let resp = self
            .http
            .post(&url)
            .header("Authorization", format!("Bearer {token}"))
            .json(&body)
            .send()
            .await
            .map_err(|e| format!("send_raw network error: {e}"))?;

        let result: SendMessageResponse = resp
            .json()
            .await
            .map_err(|e| format!("send_raw parse error: {e}"))?;

        if result.code == 0 {
            Ok(result.data.and_then(|d| d.message_id))
        } else {
            Err(format!(
                "send_raw failed: {} {}",
                result.code,
                result.msg.unwrap_or_default()
            ))
        }
    }

    pub async fn get_token(&self) -> Result<String, String> {
        {
            let cached = self.access_token.lock().unwrap();
            if let Some((ref token, expiry)) = *cached {
                if expiry > Instant::now() {
                    return Ok(token.clone());
                }
            }
        }

        let url = format!("{FEISHU_API_BASE}/auth/v3/tenant_access_token/internal");
        let body = serde_json::json!({
            "app_id": self.app_id,
            "app_secret": self.app_secret,
        });

        let resp = self
            .http
            .post(&url)
            .json(&body)
            .send()
            .await
            .map_err(|e| format!("get_token network error: {e}"))?;

        let result: TenantTokenResponse = resp
            .json()
            .await
            .map_err(|e| format!("get_token parse error: {e}"))?;

        if result.code == 0 {
            let token = result.tenant_access_token.ok_or("no token in response")?;
            let expire_secs = result.expire.unwrap_or(7200) as u64;
            let expiry =
                Instant::now() + std::time::Duration::from_secs(expire_secs.saturating_sub(300));

            let mut cached = self.access_token.lock().unwrap();
            *cached = Some((token.clone(), expiry));
            Ok(token)
        } else {
            Err(format!(
                "get_token failed: {} {}",
                result.code,
                result.msg.unwrap_or_default()
            ))
        }
    }

    pub async fn get_ws_endpoint(&self) -> Result<WsEndpointData, String> {
        let url = format!("{FEISHU_WS_BASE}/callback/ws/endpoint");
        let body = serde_json::json!({
            "AppID": self.app_id,
            "AppSecret": self.app_secret,
        });

        let resp = self
            .http
            .post(&url)
            .json(&body)
            .send()
            .await
            .map_err(|e| format!("get_ws_endpoint network error: {e}"))?;

        let status = resp.status();
        let body_text = resp
            .text()
            .await
            .map_err(|e| format!("get_ws_endpoint read body: {e}"))?;
        tracing::info!("[feishu] ws endpoint HTTP {status}, body: {body_text}");

        let result: serde_json::Value = serde_json::from_str(&body_text)
            .map_err(|e| format!("get_ws_endpoint parse error: {e}. body was: {body_text}"))?;

        let code = result.get("code").and_then(|v| v.as_i64()).unwrap_or(-1);
        if code == 0 {
            let url = result
                .get("data")
                .and_then(|d| d.get("URL").or_else(|| d.get("url")))
                .and_then(|v| v.as_str())
                .map(|s| s.to_string());
            Ok(WsEndpointData { url })
        } else {
            let msg = result
                .get("msg")
                .and_then(|v| v.as_str())
                .unwrap_or("unknown");
            Err(format!("get_ws_endpoint failed: {code} {msg}"))
        }
    }

    pub async fn get_bot_open_id(&self) -> Result<String, String> {
        let token = match self.get_token().await {
            Ok(t) => t,
            Err(e) => {
                tracing::warn!("[feishu] get_bot_open_id: get_token failed: {e}");
                return Err(format!("get_token: {e}"));
            }
        };
        let url = format!("{FEISHU_API_BASE}/bot/v3/info");
        let resp = self
            .http
            .get(&url)
            .header("Authorization", format!("Bearer {token}"))
            .send()
            .await
            .map_err(|e| format!("get_bot_open_id network error: {e}"))?;
        let body_text = resp
            .text()
            .await
            .map_err(|e| format!("get_bot_open_id read body: {e}"))?;
        tracing::info!("[feishu] bot/v3/info response: {body_text}");
        let result: serde_json::Value = serde_json::from_str(&body_text)
            .map_err(|e| format!("get_bot_open_id parse error: {e}"))?;
        let code = result.get("code").and_then(|v| v.as_i64()).unwrap_or(-1);
        if code == 0 {
            result
                .get("data")
                .and_then(|d| d.get("bot"))
                .and_then(|b| b.get("open_id"))
                .and_then(|v| v.as_str())
                .map(|s| s.to_string())
                .ok_or_else(|| "get_bot_open_id: no open_id".into())
        } else {
            Err(format!("get_bot_open_id failed: {code}"))
        }
    }
}
