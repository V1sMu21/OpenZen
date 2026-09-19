use std::sync::Mutex;

use async_trait::async_trait;
use oz_browser::BrowserClient;
use oz_core_types::{ToolContext, ToolError, ToolOutput};

use crate::registry::ToolHandler;

/// Blocked IP ranges for SSRF prevention.
const BLOCKED_IP_RANGES: &[&str] = &[
    "127.0.0.1",
    "localhost",
    "0.0.0.0",
    "[::1]",
    "10.",
    "172.16.",
    "172.17.",
    "172.18.",
    "172.19.",
    "172.20.",
    "172.21.",
    "172.22.",
    "172.23.",
    "172.24.",
    "172.25.",
    "172.26.",
    "172.27.",
    "172.28.",
    "172.29.",
    "172.30.",
    "172.31.",
    "192.168.",
    "169.254.",
    "metadata.google.internal",
];

/// Literal-only check (no DNS): blocks the substrings AND any IP literal
/// that parses to a private/loopback/link-local/ULA address. Used for
/// redirect hops where an async DNS lookup is not possible.
pub fn is_url_safe_literal(url: &str) -> bool {
    let lower = url.to_lowercase();
    for blocked in BLOCKED_IP_RANGES {
        if lower.contains(&blocked.to_lowercase()) {
            return false;
        }
    }
    // Parse the host as a literal IP when possible: decimal/hex encodings
    // (http://2130706433/), IPv4-mapped IPv6, and bracketed forms all
    // bypassed the substring list. Unparseable URLs fail closed.
    let Ok(parsed) = reqwest::Url::parse(url) else {
        return false;
    };
    if let Some(host) = parsed.host_str() {
        let bare = host.trim_start_matches('[').trim_end_matches(']');
        if let Ok(ip) = bare.parse::<std::net::IpAddr>() {
            return is_public_ip(&ip);
        }
    }
    true
}

/// Full check: literal rules PLUS DNS resolution — every resolved address
/// must be public. A DNS name pointing at 127.0.0.1 or 169.254.169.254
/// (metadata) is rejected. Unresolvable hosts are rejected (fail closed).
pub async fn is_url_safe(url: &str) -> bool {
    if !is_url_safe_literal(url) {
        return false;
    }
    let Ok(parsed) = reqwest::Url::parse(url) else {
        return false;
    };
    let Some(host) = parsed.host_str() else {
        return false;
    };
    // Owned copy: `parsed` must be droppable before the .await below.
    let bare = host
        .trim_start_matches('[')
        .trim_end_matches(']')
        .to_string();
    if bare.parse::<std::net::IpAddr>().is_ok() {
        // Already validated as a literal above.
        return true;
    }
    let port = parsed.port_or_known_default().unwrap_or(443);
    let lookup = tokio::net::lookup_host((bare.as_str(), port)).await;
    match lookup {
        Ok(addrs) => {
            let resolved: Vec<std::net::SocketAddr> = addrs.collect();
            if resolved.is_empty() {
                return false;
            }
            resolved.iter().all(|sa| is_public_ip(&sa.ip()))
        }
        Err(e) => {
            tracing::warn!("[ssrf] DNS resolution failed for {bare}: {e}");
            false
        }
    }
}

/// Public (routable, non-internal) address check.
fn is_public_ip(ip: &std::net::IpAddr) -> bool {
    match ip {
        std::net::IpAddr::V4(v4) => {
            let o = v4.octets();
            !(v4.is_loopback()
                || v4.is_private()
                || v4.is_link_local()
                || v4.is_broadcast()
                || v4.is_unspecified()
                || v4.is_documentation()
                || o[0] == 0
                // CGNAT 100.64.0.0/10
                || (o[0] == 100 && (64..=127).contains(&o[1]))
                // 198.18.0.0/15 benchmarking
                || (o[0] == 198 && (o[1] == 18 || o[1] == 19))
                // 192.0.0.0/24 IETF protocol assignments
                || (o[0] == 192 && o[1] == 0 && o[2] == 0))
        }
        std::net::IpAddr::V6(v6) => {
            if let Some(mapped) = v6.to_ipv4_mapped() {
                return is_public_ip(&std::net::IpAddr::V4(mapped));
            }
            !(v6.is_loopback() || v6.is_unspecified() || v6.is_unique_local())
        }
    }
}

#[cfg(test)]
mod ssrf_tests {
    use super::*;

    #[test]
    fn literal_bypasses_rejected() {
        assert!(!is_url_safe_literal("http://127.0.0.1/admin"));
        assert!(!is_url_safe_literal("http://localhost:8000/"));
        assert!(!is_url_safe_literal("http://2130706433/")); // decimal 127.0.0.1
        assert!(!is_url_safe_literal("http://0x7f000001/"));
        assert!(!is_url_safe_literal("http://[::ffff:127.0.0.1]/"));
        assert!(!is_url_safe_literal(
            "http://169.254.169.254/latest/meta-data"
        ));
        assert!(!is_url_safe_literal("http://192.168.1.1/"));
        assert!(!is_url_safe_literal("http://[::1]/"));
        assert!(is_url_safe_literal("https://example.com/path"));
        assert!(is_url_safe_literal("https://8.8.8.8/"));
    }

    #[test]
    fn garbage_rejected() {
        assert!(!is_url_safe_literal("not a url"));
    }
}

/// Fetch and simplify HTML from a URL using the browser.
pub struct WebScanTool {
    browser: Mutex<Option<BrowserClient>>,
}

impl WebScanTool {
    pub fn new() -> Self {
        WebScanTool {
            browser: Mutex::new(None),
        }
    }

    fn get_browser(&self) -> BrowserClient {
        let mut b = self
            .browser
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if b.is_none() {
            *b = Some(BrowserClient::new("http://127.0.0.1:18765"));
        }
        b.as_ref().unwrap().clone()
    }
}

impl Default for WebScanTool {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl ToolHandler for WebScanTool {
    fn name(&self) -> String {
        "web_scan".to_string()
    }
    fn description(&self) -> String {
        "Open a URL in the browser, get simplified HTML content. Use for reading web pages."
            .to_string()
    }
    fn parameters(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "url": {
                    "type": "string",
                    "description": "The URL to open and read"
                },
                "max_chars": {
                    "type": "integer",
                    "description": "Max chars (default 5000)",
                    "default": 5000
                }
            },
            "required": ["url"]
        })
    }

    async fn execute(
        &self,
        args: serde_json::Value,
        _ctx: &ToolContext,
    ) -> Result<ToolOutput, ToolError> {
        let url = args["url"]
            .as_str()
            .ok_or_else(|| ToolError::Custom("missing url".into()))?;
        if !is_url_safe(url).await {
            return Ok(ToolOutput::bad_json(format!(
                "web_scan: URL `{url}` targets a blocked address for security reasons."
            )));
        }
        let max_chars = args
            .get("max_chars")
            .and_then(|v| v.as_u64())
            .unwrap_or(5000) as usize;

        let mut browser = self.get_browser();
        let title = browser.navigate(url).await?;
        let html = browser.get_simplified_html(max_chars).await?;

        Ok(ToolOutput::success(serde_json::json!({
            "title": title,
            "url": url,
            "html": html,
            "char_count": html.len(),
        })))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_web_scan_missing_url() {
        let tool = WebScanTool::new();
        let result = tool
            .execute(serde_json::json!({}), &ToolContext::default())
            .await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn test_web_scan_connect_error() {
        let tool = WebScanTool::new();
        let result = tool
            .execute(
                serde_json::json!({"url": "http://localhost:1", "max_chars": 100}),
                &ToolContext::default(),
            )
            .await;
        // localhost is blocked by SSRF protection
        assert!(result.is_ok());
        let output = result.unwrap();
        assert!(
            output.next_prompt.unwrap_or_default().contains("blocked"),
            "expected blocked URL message"
        );
    }
}

#[linkme::distributed_slice(crate::registry::TOOL_FACTORIES)]
fn register_web_scan(reg: &mut crate::registry::ToolRegistry) {
    reg.register(crate::web_scan::WebScanTool::new());
}
