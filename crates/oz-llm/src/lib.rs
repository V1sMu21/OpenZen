pub mod claude;
pub mod client;
pub mod message_format;
pub mod mixin;
pub mod native_claude;
pub mod native_oai;
pub mod openai;
pub mod retry;
pub mod session;
pub mod smart_router;
pub mod stream;

/// True when the API base points at a local deployment (omlx, ollama,
/// llama.cpp on 127.0.0.1 / localhost). Local quantized models prefill
/// and generate much slower than cloud APIs, so callers use this to pick
/// longer HTTP/stream timeouts and avoid mid-response timeouts.
pub fn is_local_apibase(apibase: &str) -> bool {
    let base = apibase.to_lowercase();
    base.contains("localhost")
        || base.contains("127.0.0.1")
        || base.contains("0.0.0.0")
        || base.starts_with("http://")
            && (base.contains(".local") || base.contains(".lan") || base.contains(".internal"))
}

/// Build the shared HTTP client for an LLM API base. Local deployments must
/// bypass the system proxy: macOS system proxies (Clash etc.) cannot forward
/// loopback requests and return 502 Bad Gateway for 127.0.0.1 endpoints.
pub fn build_http_client(apibase: &str, timeout_secs: u64) -> reqwest::Client {
    http_client_builder(apibase, timeout_secs)
        .build()
        .unwrap_or_default()
}

fn http_client_builder(apibase: &str, timeout_secs: u64) -> reqwest::ClientBuilder {
    let mut builder = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(timeout_secs))
        .connect_timeout(std::time::Duration::from_secs(15))
        .no_gzip()
        .no_brotli()
        .no_deflate();
    if is_local_apibase(apibase) {
        builder = builder.no_proxy();
    }
    builder
}

/// opencode.ai's "Go" gateway routes requests per conversation and rejects
/// clients that send no session affinity with `400 MissingSessionID`
/// ("Request is missing x-opencode-session"). The header carries a stable
/// per-conversation id so the gateway can pin routing and reuse its prompt
/// cache across a conversation's turns.
const OPENCODE_API_HOST: &str = "opencode.ai";
const OPENCODE_SESSION_HEADER: &str = "x-opencode-session";

/// Default headers for a model-backed session client: `config.extra_headers`
/// verbatim, plus the auto `x-opencode-session` for opencode.ai endpoints
/// (`session_tag` when wired, else a random UUID).
fn session_default_headers(cfg: &oz_config::SessionConfig) -> reqwest::header::HeaderMap {
    let mut headers = reqwest::header::HeaderMap::new();
    if let Some(extra) = &cfg.extra_headers {
        for (name, value) in extra {
            if let (Ok(name), Ok(value)) = (
                reqwest::header::HeaderName::from_bytes(name.as_bytes()),
                reqwest::header::HeaderValue::from_str(value),
            ) {
                headers.insert(name, value);
            }
        }
    }
    if cfg.apibase.contains(OPENCODE_API_HOST) && !headers.contains_key(OPENCODE_SESSION_HEADER) {
        let tag = cfg
            .session_tag
            .clone()
            .unwrap_or_else(|| uuid::Uuid::new_v4().to_string());
        if let Ok(value) = reqwest::header::HeaderValue::from_str(&tag) {
            headers.insert(OPENCODE_SESSION_HEADER, value);
        }
    }
    headers
}

/// Build the HTTP client for a model-backed LLM session: same base builder
/// as [`build_http_client`], plus the session's default headers
/// (see [`session_default_headers`]).
pub fn build_session_http_client(
    cfg: &oz_config::SessionConfig,
    timeout_secs: u64,
) -> reqwest::Client {
    let mut builder = http_client_builder(&cfg.apibase, timeout_secs);
    let headers = session_default_headers(cfg);
    if !headers.is_empty() {
        builder = builder.default_headers(headers);
    }
    builder.build().unwrap_or_default()
}

pub use claude::ClaudeSession;
pub use client::*;
pub use mixin::MixinSession;
pub use native_claude::NativeClaudeSession;
pub use native_oai::NativeOAISession;
pub use openai::OaiSession;
pub use session::*;

#[cfg(test)]
mod tests {
    use super::*;
    use reqwest::header::HeaderMap;
    use std::collections::HashMap;

    fn session_cfg(apibase: &str) -> oz_config::SessionConfig {
        oz_config::SessionConfig {
            apikey: "sk-test".into(),
            apibase: apibase.into(),
            model: "glm-5.3-flash".into(),
            context_win: 128000,
            max_tokens: None,
            temperature: None,
            api_mode: Default::default(),
            reasoning_effort: None,
            max_retries: None,
            proxy: None,
            verify: None,
            timeout: None,
            llm_nos: None,
            base_delay: None,
            spring_back: None,
            extra_headers: None,
            session_tag: None,
        }
    }

    fn header_str(map: &HeaderMap, name: &str) -> Option<String> {
        map.get(name)
            .and_then(|v| v.to_str().ok().map(String::from))
    }

    #[test]
    fn opencode_session_uses_wired_session_tag() {
        let mut cfg = session_cfg("https://opencode.ai/zen/go/v1");
        cfg.session_tag = Some("6bbea6ed-conv".into());
        let headers = session_default_headers(&cfg);
        assert_eq!(
            header_str(&headers, "x-opencode-session").as_deref(),
            Some("6bbea6ed-conv")
        );
    }

    #[test]
    fn opencode_session_falls_back_to_uuid() {
        let cfg = session_cfg("https://opencode.ai/zen/go/v1");
        let headers = session_default_headers(&cfg);
        let v = header_str(&headers, "x-opencode-session")
            .expect("session header auto-injected for opencode.ai");
        assert!(uuid::Uuid::parse_str(&v).is_ok());
    }

    #[test]
    fn other_hosts_get_no_session_header() {
        let cfg = session_cfg("https://api.example.com/v1");
        let headers = session_default_headers(&cfg);
        assert!(headers.get("x-opencode-session").is_none());
    }

    #[test]
    fn explicit_extra_header_wins_over_auto_injection() {
        let mut cfg = session_cfg("https://opencode.ai/zen/go/v1");
        let mut extra = HashMap::new();
        extra.insert("x-opencode-session".to_string(), "explicit".to_string());
        extra.insert("x-custom".to_string(), "1".to_string());
        cfg.extra_headers = Some(extra);
        cfg.session_tag = Some("conv".into());
        let headers = session_default_headers(&cfg);
        assert_eq!(
            header_str(&headers, "x-opencode-session").as_deref(),
            Some("explicit")
        );
        assert_eq!(header_str(&headers, "x-custom").as_deref(), Some("1"));
    }

    #[test]
    fn invalid_extra_header_value_is_skipped_not_fatal() {
        let mut cfg = session_cfg("https://api.example.com/v1");
        let mut extra = HashMap::new();
        // HeaderValue rejects control characters (newline) — a config typo
        // must not panic client construction.
        extra.insert("x-bad".to_string(), "bad\nvalue".to_string());
        cfg.extra_headers = Some(extra);
        let headers = session_default_headers(&cfg);
        assert!(headers.get("x-bad").is_none());
    }
}
