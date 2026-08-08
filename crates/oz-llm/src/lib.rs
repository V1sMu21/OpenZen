pub mod session;
pub mod claude;
pub mod openai;
pub mod native_claude;
pub mod native_oai;
pub mod mixin;
pub mod stream;
pub mod retry;
pub mod message_format;
pub mod client;
pub mod smart_router;

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
    let mut builder = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(timeout_secs))
        .connect_timeout(std::time::Duration::from_secs(15))
        .no_gzip()
        .no_brotli()
        .no_deflate();
    if is_local_apibase(apibase) {
        builder = builder.no_proxy();
    }
    builder.build().unwrap_or_default()
}

pub use session::*;
pub use client::*;
pub use claude::ClaudeSession;
pub use openai::OaiSession;
pub use native_claude::NativeClaudeSession;
pub use native_oai::NativeOAISession;
pub use mixin::MixinSession;
