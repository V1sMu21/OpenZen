use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PlatformConfig {
    pub enabled: bool,
    pub allowed_users: Option<Vec<String>>,
    pub default_model: Option<String>,
    pub proxy: Option<String>,

    #[serde(flatten)]
    pub extra: serde_json::Value,
}

impl PlatformConfig {
    pub fn is_public(&self) -> bool {
        self.allowed_users.is_none()
            || self
                .allowed_users
                .as_ref()
                .map(|v| v.is_empty() || v.contains(&"*".to_string()))
                .unwrap_or(true)
    }

    pub fn is_allowed(&self, user_id: &str) -> bool {
        if self.is_public() {
            return true;
        }
        self.allowed_users
            .as_ref()
            .map(|v| v.contains(&user_id.to_string()))
            .unwrap_or(false)
    }

    pub fn telegram_token(&self) -> Option<&str> {
        self.extra.get("bot_token")?.as_str()
    }

    pub fn feishu_app_id(&self) -> Option<&str> {
        self.extra.get("app_id")?.as_str()
    }

    pub fn feishu_app_secret(&self) -> Option<&str> {
        self.extra.get("app_secret")?.as_str()
    }

    pub fn qq_app_id(&self) -> Option<&str> {
        self.extra.get("app_id")?.as_str()
    }

    pub fn qq_app_secret(&self) -> Option<&str> {
        self.extra.get("app_secret")?.as_str()
    }

    pub fn qq_sandbox(&self) -> bool {
        self.extra.get("sandbox").and_then(|v| v.as_bool()).unwrap_or(true)
    }
}

impl Default for PlatformConfig {
    fn default() -> Self {
        PlatformConfig {
            enabled: false,
            allowed_users: None,
            default_model: None,
            proxy: None,
            extra: serde_json::Value::Object(Default::default()),
        }
    }
}
