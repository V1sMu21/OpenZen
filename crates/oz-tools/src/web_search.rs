//! `web_search` — multi-engine web search with automatic failover.
//!
//! Engines (all in-process HTTP except `exa`):
//!
//! | engine | key | notes |
//! |---|---|---|
//! | `bocha` | required — `BOCHA_API_KEY` / `[web_search] bocha_api_key` | mainland-China direct, covers CN + global sources |
//! | `tavily` | optional — `TAVILY_API_KEY` / `[web_search] tavily_api_key` | keyless access mode when no key is set |
//! | `firecrawl` | optional — `FIRECRAWL_API_KEY` / `[web_search] firecrawl_api_key` | keyless tier when no key is set |
//! | `exa` | mcporter config | legacy path — spawns the `mcporter` CLI |
//! | `bing-rss` / `bing-html` | none | **opt-in only** — must be listed in `[web_search] engines`; Bing `robots.txt` disallows `/search` for `User-agent: *` |
//!
//! The `tavily` / `firecrawl` / `bing-*` backends are a Rust port of the
//! `dsh-keyless-search` DSH plugin (MIT): same endpoints, same request bodies,
//! same header discipline (the keyless hint is sent ONLY when no key is
//! configured), same parsers, same failover semantics.
//!
//! Chain selection (the default `engine` is `keyless`):
//! * `engine: "keyless"` → `[web_search] keyless_engines` (default `tavily,firecrawl`)
//! * `engine: "auto"`    → `[web_search] engines` (default `tavily,firecrawl,bocha,exa`)
//! * `engine: "<name>"`  → that engine only
//!
//! Run `openzen key set tavily` to store a key without it ever touching shell
//! history or the agent transcript.

use crate::registry::ToolHandler;
use async_trait::async_trait;
use oz_core_types::{ToolContext, ToolError, ToolOutput};
use std::path::PathBuf;
use std::sync::LazyLock;

pub struct WebSearchTool;

/// Every engine this tool can dispatch to, in preferred-default order.
pub const SEARCH_ENGINES: [&str; 6] = ["bocha", "tavily", "firecrawl", "exa", "bing-rss", "bing-html"];

/// Default chain for the tool's default `engine: "keyless"` — the
/// dsh-keyless-search backend order (Tavily first, then Firecrawl).
const DEFAULT_KEYLESS_CHAIN: &str = "tavily,firecrawl";

/// Default `engine: "auto"` chain — the explicit opt-in path to the
/// key-requiring / legacy engines.
const DEFAULT_ENGINE_CHAIN: &str = "tavily,firecrawl,bocha,exa";

/// Engines that scrape a search-results page. Operator opt-in only.
const OPT_IN_ENGINES: [&str; 2] = ["bing-rss", "bing-html"];

const BING_HOST: &str = "cn.bing.com";
const BING_MARKET: &str = "zh-CN";
const KEYLESS_TIMEOUT_SECS: u64 = 15;
const USER_AGENT: &str = "openzen-keyless-search (+https://github.com/openzen)";
const MAX_RESULTS: usize = 20;

/* ------------------------------------------------------------------ *
 * Config / credential resolution
 * ------------------------------------------------------------------ */

/// Environment variable holding an engine's API key, when it has one.
fn engine_env_var(engine: &str) -> Option<&'static str> {
    match engine {
        "bocha" => Some("BOCHA_API_KEY"),
        "tavily" => Some("TAVILY_API_KEY"),
        "firecrawl" => Some("FIRECRAWL_API_KEY"),
        _ => None,
    }
}

/// `[web_search]` key holding an engine's API key, when it has one.
fn engine_toml_key(engine: &str) -> Option<&'static str> {
    match engine {
        "bocha" => Some("bocha_api_key"),
        "tavily" => Some("tavily_api_key"),
        "firecrawl" => Some("firecrawl_api_key"),
        _ => None,
    }
}

/// `mykey.toml` candidates in the exact order reads resolve them.
///
/// `OPENZEN_DATA_DIR` wins when set (Plan B data-root override), then the
/// historical `~/.openzen` / `~/` / working-dir locations. The writer
/// (`openzen key set`, `scripts/set-search-key.sh`) resolves through
/// [`search_config_write_path`], so writes and reads always agree.
pub fn search_config_candidates(working_dir: &str) -> Vec<PathBuf> {
    let mut candidates = Vec::new();
    if let Ok(dir) = std::env::var("OPENZEN_DATA_DIR") {
        let dir = dir.trim();
        if !dir.is_empty() {
            candidates.push(PathBuf::from(dir).join("mykey.toml"));
        }
    }
    let home = std::env::var("HOME").unwrap_or_else(|_| "/tmp".into());
    candidates.push(PathBuf::from(&home).join(".openzen").join("mykey.toml"));
    candidates.push(PathBuf::from(&home).join("mykey.toml"));
    candidates.push(PathBuf::from(working_dir).join("config").join("mykey.toml"));
    candidates.push(PathBuf::from(working_dir).join("mykey.toml"));
    candidates
}

/// File `openzen key set` should write: the first candidate that exists, else
/// the first candidate (data root / `~/.openzen/mykey.toml`).
pub fn search_config_write_path(working_dir: &str) -> PathBuf {
    let candidates = search_config_candidates(working_dir);
    candidates
        .iter()
        .find(|path| path.exists())
        .cloned()
        .unwrap_or_else(|| candidates[0].clone())
}

/// A resolved API key plus where it came from (for `openzen key list`).
pub struct SearchKey {
    pub key: String,
    pub source: String,
}

/// Resolve an engine's API key: environment first, then `mykey.toml`.
pub fn search_api_key(engine: &str, working_dir: &str) -> Option<SearchKey> {
    if let Some(env_name) = engine_env_var(engine) {
        if let Ok(value) = std::env::var(env_name) {
            let value = value.trim();
            if !value.is_empty() {
                return Some(SearchKey {
                    key: value.to_string(),
                    source: format!("env {env_name}"),
                });
            }
        }
    }
    search_api_key_from_paths(engine, &search_config_candidates(working_dir))
}

/// File-only key lookup, split out so tests can pass an explicit path list.
fn search_api_key_from_paths(engine: &str, paths: &[PathBuf]) -> Option<SearchKey> {
    let toml_key = engine_toml_key(engine)?;
    for path in paths {
        if let Ok(content) = std::fs::read_to_string(path) {
            if let Some(key) = extract_toml_value(&content, "web_search", toml_key) {
                if !key.is_empty() {
                    return Some(SearchKey {
                        key,
                        source: path.display().to_string(),
                    });
                }
            }
        }
    }
    None
}

/// Read a `[web_search]` value as a comma-separated lowercase list.
fn read_engine_list(paths: &[PathBuf], key: &str) -> Option<Vec<String>> {
    for path in paths {
        if let Ok(content) = std::fs::read_to_string(path) {
            if let Some(value) = extract_toml_value(&content, "web_search", key) {
                let list: Vec<String> = value
                    .split(',')
                    .map(|entry| entry.trim().to_lowercase())
                    .filter(|entry| !entry.is_empty())
                    .collect();
                if !list.is_empty() {
                    return Some(list);
                }
            }
        }
    }
    None
}

/// Opt-in (Bing) engines the operator explicitly enabled in `mykey.toml`.
pub fn bing_engines_enabled(paths: &[PathBuf]) -> Vec<String> {
    let mut enabled = Vec::new();
    for key in ["engines", "keyless_engines"] {
        if let Some(list) = read_engine_list(paths, key) {
            for engine in list {
                if OPT_IN_ENGINES.contains(&engine.as_str()) && !enabled.contains(&engine) {
                    enabled.push(engine);
                }
            }
        }
    }
    enabled
}

/// Resolve the failover chain for the `engine` tool argument.
fn resolve_chain(engine: &str, working_dir: &str) -> Result<Vec<String>, String> {
    let paths = search_config_candidates(working_dir);
    resolve_chain_from(
        engine,
        read_engine_list(&paths, "engines"),
        read_engine_list(&paths, "keyless_engines"),
        &bing_engines_enabled(&paths),
    )
}

/// Pure chain resolver: the configured lists are injected, not read from disk,
/// so the selection rules are testable without touching the user's config.
fn resolve_chain_from(
    engine: &str,
    configured: Option<Vec<String>>,
    keyless_configured: Option<Vec<String>>,
    bing_enabled: &[String],
) -> Result<Vec<String>, String> {
    let engine = engine.trim().to_lowercase();
    let chain: Vec<String> = match engine.as_str() {
        "" | "auto" => configured.unwrap_or_else(|| split_chain(DEFAULT_ENGINE_CHAIN)),
        "keyless" => keyless_configured.unwrap_or_else(|| split_chain(DEFAULT_KEYLESS_CHAIN)),
        single => vec![single.to_string()],
    };

    for name in &chain {
        if !SEARCH_ENGINES.contains(&name.as_str()) {
            return Err(format!(
                "unknown engine '{name}'. Valid: auto, keyless, {}",
                SEARCH_ENGINES.join(", ")
            ));
        }
        if OPT_IN_ENGINES.contains(&name.as_str()) && !bing_enabled.contains(name) {
            return Err(format!(
                "'{name}' is opt-in and not enabled: Bing robots.txt disallows /search for \
                 User-agent: *. Add it to `[web_search] engines` in mykey.toml \
                 (e.g. engines = \"tavily,firecrawl,{name}\") to accept that trade-off."
            ));
        }
    }
    Ok(chain)
}

fn split_chain(value: &str) -> Vec<String> {
    value
        .split(',')
        .map(|entry| entry.trim().to_lowercase())
        .filter(|entry| !entry.is_empty())
        .collect()
}

#[async_trait]
impl ToolHandler for WebSearchTool {
    fn name(&self) -> String {
        "web_search".to_string()
    }
    fn description(&self) -> String {
        "Search the web through a failover chain of engines. Default chain is keyless: Tavily \
         (keyless or keyed) then Firecrawl (keyless or keyed) — no API key required. Bocha \
         (mainland China, needs a key) and Exa (mcporter) remain available via `engine: \"auto\"` \
         or by name; Bing backends are opt-in only. Returns result titles, URLs, and snippets."
            .to_string()
    }
    fn description_zh(&self) -> String {
        "网络搜索（默认 keyless 链：Tavily → Firecrawl 依次回退，免密钥即可用；\
         需要博查/Exa 时用 engine=\"auto\" 或直接指定引擎名）。返回结果标题、URL 和摘要。"
            .to_string()
    }
    fn parameters(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "query": {
                    "type": "string",
                    "description": "Search query string"
                },
                "num_results": {
                    "type": "integer",
                    "description": "Max results (default 5, max 20)",
                    "default": 5
                },
                "engine": {
                    "type": "string",
                    "enum": ["keyless", "auto", "bocha", "tavily", "firecrawl", "exa", "bing-rss", "bing-html"],
                    "description": "keyless (default) = tavily,firecrawl, works without any API key; auto = [web_search] engines (default tavily,firecrawl,bocha,exa); or one engine by name. Bing engines require operator opt-in in mykey.toml.",
                    "default": "keyless"
                }
            },
            "required": ["query"]
        })
    }

    async fn execute(
        &self,
        args: serde_json::Value,
        ctx: &ToolContext,
    ) -> Result<ToolOutput, ToolError> {
        let query = args
            .get("query")
            .and_then(|v| v.as_str())
            .ok_or_else(|| ToolError::Custom("missing 'query' parameter".into()))?
            .trim()
            .to_string();

        if query.is_empty() {
            return Err(ToolError::Custom("empty 'query' parameter".into()));
        }

        let num_results = args
            .get("num_results")
            .and_then(|v| v.as_u64())
            .unwrap_or(5)
            .clamp(1, MAX_RESULTS as u64) as usize;

        let engine = args
            .get("engine")
            .and_then(|v| v.as_str())
            .unwrap_or("keyless");

        let chain = resolve_chain(engine, &ctx.working_dir).map_err(ToolError::Custom)?;

        let mut failures: Vec<String> = Vec::new();
        for name in &chain {
            match run_engine(name, &query, num_results, &ctx.working_dir, None).await {
                Ok(results) if !results.is_empty() => {
                    return Ok(ToolOutput::success(serde_json::json!({
                        "query": query,
                        "engine": name,
                        "chain": chain,
                        "results": results,
                        "total": results.len()
                    })));
                }
                Ok(_) => failures.push(format!("{name}: no results")),
                Err(err) => failures.push(format!("{name}: {err}")),
            }
        }

        Err(ToolError::Custom(format!(
            "all engines failed for \"{query}\" — {}",
            failures.join("; ")
        )))
    }
}

/// Dispatch one engine. Bing engines are gated by [`resolve_chain`], not here,
/// so `probe_engine_with_key` can also exercise them for diagnostics.
///
/// `key_override` short-circuits key resolution for the one keyed engine being
/// probed, so `openzen key set` verifies the key it just stored even when it
/// was written to a path the regular resolution order does not visit.
async fn run_engine(
    engine: &str,
    query: &str,
    num_results: usize,
    working_dir: &str,
    key_override: Option<&str>,
) -> Result<Vec<serde_json::Value>, String> {
    let key = |name: &str| {
        key_override
            .map(str::to_string)
            .or_else(|| search_api_key(name, working_dir).map(|found| found.key))
    };

    match engine {
        "bocha" => {
            let api_key = key("bocha").ok_or_else(|| {
                "API key not configured — run `openzen key set bocha`, or set BOCHA_API_KEY, \
                 or add [web_search] bocha_api_key to mykey.toml"
                    .to_string()
            })?;
            search_bocha(query, num_results, &api_key).await
        }
        "tavily" => search_tavily(query, num_results, key("tavily").as_deref()).await,
        "firecrawl" => search_firecrawl(query, num_results, key("firecrawl").as_deref()).await,
        "exa" => search_exa(query, num_results, working_dir),
        "bing-rss" => search_bing_rss(query, num_results).await,
        "bing-html" => search_bing_html(query, num_results).await,
        other => Err(format!("unknown engine '{other}'")),
    }
}

/// Live-check one engine and report how many results it returned.
///
/// Used by `openzen key set` so a newly stored key is proven against the real
/// API instead of merely being written to disk. Without `key`, the engine runs
/// with whatever key resolution finds (keyless tiers stay keyless).
pub async fn probe_engine(engine: &str, working_dir: &str) -> Result<usize, String> {
    probe_engine_with_key(engine, None, working_dir).await
}

/// [`probe_engine`] against an explicit key — the key just written to disk, so
/// a typo'd key is reported as invalid instead of silently probing keyless.
pub async fn probe_engine_with_key(
    engine: &str,
    key: Option<&str>,
    working_dir: &str,
) -> Result<usize, String> {
    let results =
        run_engine(engine, "openzen web search connectivity check", 3, working_dir, key).await?;
    Ok(results.len())
}

/* ------------------------------------------------------------------ *
 * HTTP helpers
 * ------------------------------------------------------------------ */

// Shared client for search API calls (pooling + TLS session reuse).
static SEARCH_CLIENT: LazyLock<reqwest::Client> = LazyLock::new(reqwest::Client::new);

// Keyless backends get a per-request timeout, matching the DSH plugin's
// `timeoutMs: 15000` — a hung vendor endpoint must not stall the tool chain.
static KEYLESS_CLIENT: LazyLock<reqwest::Client> = LazyLock::new(|| {
    reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(KEYLESS_TIMEOUT_SECS))
        .build()
        .unwrap_or_default()
});

/// POST a JSON body and parse the JSON response, failing loudly on any non-2xx.
async fn post_search_json(
    endpoint: &str,
    body: serde_json::Value,
    headers: Vec<(&str, String)>,
    engine: &str,
) -> Result<serde_json::Value, String> {
    let mut request = KEYLESS_CLIENT
        .post(endpoint)
        .header("content-type", "application/json")
        .header("user-agent", USER_AGENT)
        .json(&body);
    for (name, value) in headers {
        request = request.header(name, value);
    }

    let response = request
        .send()
        .await
        .map_err(|e| format!("{engine} request failed: {e}"))?;
    let status = response.status();
    if !status.is_success() {
        let body = response.text().await.unwrap_or_default();
        let detail: String = body.chars().take(200).collect();
        return Err(format!("{engine} returned HTTP {status}: {detail}"));
    }
    response
        .json()
        .await
        .map_err(|e| format!("{engine} response parse failed: {e}"))
}

/// GET a URL as text with per-request timeout, failing loudly on any non-2xx.
async fn get_text(
    url: &str,
    headers: Vec<(&str, &str)>,
    engine: &str,
) -> Result<String, String> {
    let mut request = KEYLESS_CLIENT
        .get(url)
        .header("user-agent", USER_AGENT);
    for (name, value) in headers {
        request = request.header(name, value);
    }

    let response = request
        .send()
        .await
        .map_err(|e| format!("{engine} request failed: {e}"))?;
    let status = response.status();
    if !status.is_success() {
        return Err(format!("{engine} returned HTTP {status}"));
    }
    response
        .text()
        .await
        .map_err(|e| format!("{engine} response read failed: {e}"))
}

/* ------------------------------------------------------------------ *
 * Text helpers
 * ------------------------------------------------------------------ */

static TAG_RE: LazyLock<regex::Regex> =
    LazyLock::new(|| regex::Regex::new(r"<[^>]*>").expect("valid tag regex"));
static ENTITY_RE: LazyLock<regex::Regex> = LazyLock::new(|| {
    regex::Regex::new(r"&(#x?[0-9a-fA-F]+|[a-zA-Z]+);").expect("valid entity regex")
});
static DATE_PREFIX_RE: LazyLock<regex::Regex> = LazyLock::new(|| {
    regex::Regex::new(r"^\s*\d{4}[年/-]\s?\d{1,2}[月/-]\s?\d{1,2}日?\s*[·|—-]?\s*")
        .expect("valid date regex")
});
static BING_TRACKING_RE: LazyLock<regex::Regex> = LazyLock::new(|| {
    regex::Regex::new(r#"[?&]u=a1([^&"]+)"#).expect("valid bing tracking regex")
});

/// Decode the small HTML entity set search snippets actually contain.
fn decode_entities(value: &str) -> String {
    ENTITY_RE
        .replace_all(value, |caps: &regex::Captures| {
            let body = &caps[1];
            if let Some(hex) = body.strip_prefix("#x").or_else(|| body.strip_prefix("#X")) {
                return u32::from_str_radix(hex, 16)
                    .ok()
                    .and_then(char::from_u32)
                    .map(String::from)
                    .unwrap_or_else(|| caps[0].to_string());
            }
            if let Some(dec) = body.strip_prefix('#') {
                return dec
                    .parse::<u32>()
                    .ok()
                    .and_then(char::from_u32)
                    .map(String::from)
                    .unwrap_or_else(|| caps[0].to_string());
            }
            match body.to_ascii_lowercase().as_str() {
                "amp" => "&".to_string(),
                "lt" => "<".to_string(),
                "gt" => ">".to_string(),
                "quot" => "\"".to_string(),
                "apos" | "#39" => "'".to_string(),
                "nbsp" => " ".to_string(),
                _ => caps[0].to_string(),
            }
        })
        .into_owned()
}

/// Strip markup from a snippet and collapse whitespace.
fn plain_text(value: &str) -> String {
    let stripped = TAG_RE.replace_all(value, " ");
    decode_entities(&stripped)
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

/// Drop a leading localized date prefix Bing puts inside RSS descriptions.
fn strip_leading_date(value: &str) -> String {
    DATE_PREFIX_RE.replace(value, "").trim().to_string()
}

/// Resolve Bing's `/ck/a?...&u=a1<base64url>` click-tracking link to its target.
/// Non-tracking URLs pass through unchanged.
fn unwrap_bing_url(href: &str) -> String {
    let url = decode_entities(href).trim().to_string();
    if !url.to_ascii_lowercase().contains("bing.com/ck/a") {
        return url;
    }
    let Some(caps) = BING_TRACKING_RE.captures(&url) else {
        return url;
    };
    let Some(decoded) = decode_base64url(&caps[1]) else {
        return url;
    };
    if decoded.starts_with("http") {
        decoded
    } else {
        url
    }
}

/// Decode a base64url payload Bing uses in click-tracking links.
///
/// Bing omits padding, and older links use the `+`/`/` alphabet, so try the
/// url-safe alphabets first and fall back to padded standard base64.
fn decode_base64url(value: &str) -> Option<String> {
    use base64::engine::general_purpose;
    use base64::Engine as _;
    let candidates = [
        general_purpose::URL_SAFE_NO_PAD.decode(value.as_bytes()),
        general_purpose::URL_SAFE.decode(value.as_bytes()),
        general_purpose::STANDARD.decode(value.as_bytes()),
    ];
    for decoded in candidates.into_iter().flatten() {
        if let Ok(text) = String::from_utf8(decoded) {
            return Some(text);
        }
    }
    None
}

/// Normalize a URL for de-duplication; invalid URLs return `None`.
///
/// `&amp;` inside an attribute is decoded first, then the fragment dropped, so
/// the same page reached through two Bing tracking links collapses to one.
fn normalize_url(href: &str) -> Option<String> {
    let unwrapped = unwrap_bing_url(href);
    let mut url = reqwest::Url::parse(&unwrapped).ok()?;
    url.set_fragment(None);
    Some(url.to_string())
}

/// Build a result row, omitting an absent publish date.
fn source_row(
    url: String,
    title: String,
    snippet: String,
    published_at: Option<String>,
) -> serde_json::Value {
    let mut row = serde_json::json!({ "title": title, "url": url, "snippet": snippet });
    if let Some(published) = published_at {
        row["published_at"] = serde_json::Value::String(published);
    }
    row
}

/* ------------------------------------------------------------------ *
 * Backends
 * ------------------------------------------------------------------ */

/// Headers for a Tavily request: bearer when keyed, keyless hint otherwise.
///
/// The hint is sent ONLY when no key is configured — a keyed request carrying
/// it would be rejected by the vendor.
fn tavily_headers(api_key: Option<&str>) -> Vec<(&'static str, String)> {
    match api_key {
        Some(key) => vec![("authorization", format!("Bearer {key}"))],
        None => vec![("x-tavily-access-mode", "keyless".to_string())],
    }
}

/// `tavily` — api.tavily.com, documented keyless access mode when no key.
async fn search_tavily(
    query: &str,
    num_results: usize,
    api_key: Option<&str>,
) -> Result<Vec<serde_json::Value>, String> {
    let payload = post_search_json(
        "https://api.tavily.com/search",
        serde_json::json!({
            "query": query,
            "max_results": num_results.min(MAX_RESULTS),
            "search_depth": "basic",
        }),
        tavily_headers(api_key),
        "tavily",
    )
    .await?;

    let rows = payload
        .get("results")
        .and_then(|v| v.as_array())
        .cloned()
        .unwrap_or_default();

    let mut results = Vec::new();
    for row in rows {
        let Some(url) = row.get("url").and_then(|v| v.as_str()).and_then(normalize_url) else {
            continue;
        };
        results.push(source_row(
            url,
            plain_text(row.get("title").and_then(|v| v.as_str()).unwrap_or("")),
            plain_text(row.get("content").and_then(|v| v.as_str()).unwrap_or("")),
            None,
        ));
    }

    if results.is_empty() {
        Err("no results parsed from tavily output".into())
    } else {
        Ok(results)
    }
}

/// `firecrawl` — api.firecrawl.dev v2 search, keyless tier when no key.
async fn search_firecrawl(
    query: &str,
    num_results: usize,
    api_key: Option<&str>,
) -> Result<Vec<serde_json::Value>, String> {
    let headers = match api_key {
        Some(key) => vec![("authorization", format!("Bearer {key}"))],
        None => Vec::new(),
    };

    let payload = post_search_json(
        "https://api.firecrawl.dev/v2/search",
        serde_json::json!({
            "query": query,
            "limit": num_results.min(MAX_RESULTS),
            "sources": ["web"],
        }),
        headers,
        "firecrawl",
    )
    .await?;

    if payload.get("success").and_then(|v| v.as_bool()) == Some(false) {
        let detail = payload
            .get("error")
            .and_then(|v| v.as_str())
            .unwrap_or("request failed");
        return Err(format!("firecrawl: {detail}"));
    }

    let data = payload.get("data");
    let mut rows: Vec<serde_json::Value> = Vec::new();
    for bucket in ["web", "news"] {
        if let Some(list) = data.and_then(|d| d.get(bucket)).and_then(|v| v.as_array()) {
            rows.extend(list.iter().cloned());
        }
    }

    let mut results = Vec::new();
    for row in rows {
        let Some(url) = row.get("url").and_then(|v| v.as_str()).and_then(normalize_url) else {
            continue;
        };
        let snippet = row
            .get("description")
            .and_then(|v| v.as_str())
            .or_else(|| row.get("snippet").and_then(|v| v.as_str()))
            .unwrap_or("");
        results.push(source_row(
            url,
            plain_text(row.get("title").and_then(|v| v.as_str()).unwrap_or("")),
            plain_text(snippet),
            None,
        ));
    }

    if results.is_empty() {
        Err("no results parsed from firecrawl output".into())
    } else {
        Ok(results)
    }
}

/// `bing-rss` — Bing's result RSS feed. Operator opt-in only.
async fn search_bing_rss(query: &str, num_results: usize) -> Result<Vec<serde_json::Value>, String> {
    let url = format!(
        "https://{BING_HOST}/search?q={}&format=rss&count={}&mkt={}",
        urlencode(query),
        num_results.min(MAX_RESULTS),
        urlencode(BING_MARKET)
    );
    let xml = get_text(
        &url,
        vec![("accept", "application/rss+xml, text/xml, */*")],
        "bing-rss",
    )
    .await?;

    let item_re = regex::Regex::new(r"(?is)<item>(.*?)</item>").map_err(|e| e.to_string())?;
    let link_re = regex::Regex::new(r"(?is)<link>(.*?)</link>").map_err(|e| e.to_string())?;
    let title_re = regex::Regex::new(r"(?is)<title>(.*?)</title>").map_err(|e| e.to_string())?;
    let desc_re =
        regex::Regex::new(r"(?is)<description>(.*?)</description>").map_err(|e| e.to_string())?;
    let date_re = regex::Regex::new(r"(?is)<pubDate>(.*?)</pubDate>").map_err(|e| e.to_string())?;

    let mut results = Vec::new();
    for item in item_re.captures_iter(&xml) {
        let block = &item[1];
        let Some(url) = link_re
            .captures(block)
            .and_then(|c| normalize_url(c[1].trim()))
        else {
            continue;
        };
        let title = title_re
            .captures(block)
            .map(|c| plain_text(&c[1]))
            .unwrap_or_default();
        let snippet = desc_re
            .captures(block)
            .map(|c| strip_leading_date(&plain_text(&c[1])))
            .unwrap_or_default();
        let published = date_re
            .captures(block)
            .and_then(|c| chrono::DateTime::parse_from_rfc2822(c[1].trim()).ok())
            .map(|dt| dt.to_rfc3339());
        results.push(source_row(url, title, snippet, published));
        if results.len() >= num_results {
            break;
        }
    }

    if results.is_empty() {
        Err("no results parsed from bing-rss output".into())
    } else {
        Ok(results)
    }
}

/// `bing-html` — parses `<li class="b_algo">` blocks. Operator opt-in only.
async fn search_bing_html(
    query: &str,
    num_results: usize,
) -> Result<Vec<serde_json::Value>, String> {
    let url = format!(
        "https://{BING_HOST}/search?q={}&count={}&mkt={}",
        urlencode(query),
        num_results.min(MAX_RESULTS),
        urlencode(BING_MARKET)
    );
    let accept_language = if BING_MARKET == "zh-CN" {
        "zh-CN,zh;q=0.9,en;q=0.8"
    } else {
        "en-US,en;q=0.9"
    };
    let html = get_text(
        &url,
        vec![
            ("accept", "text/html,application/xhtml+xml"),
            ("accept-language", accept_language),
        ],
        "bing-html",
    )
    .await?;

    let split_re = regex::Regex::new(r#"(?i)<li class="b_algo""#).map_err(|e| e.to_string())?;
    let anchor_re = regex::Regex::new(r#"(?is)<h2[^>]*>\s*<a[^>]*href="([^"]+)"[^>]*>(.*?)</a>"#)
        .map_err(|e| e.to_string())?;
    let snippet_re =
        regex::Regex::new(r#"(?is)<p class="b_lineclamp[^"]*"[^>]*>(.*?)</p>"#).map_err(|e| e.to_string())?;
    let paragraph_re = regex::Regex::new(r"(?is)<p[^>]*>(.*?)</p>").map_err(|e| e.to_string())?;

    let mut results = Vec::new();
    for block in split_re.split(&html).skip(1) {
        let Some(anchor) = anchor_re.captures(block) else {
            continue;
        };
        let Some(url) = normalize_url(anchor[1].trim()) else {
            continue;
        };
        let snippet = snippet_re
            .captures(block)
            .or_else(|| paragraph_re.captures(block))
            .map(|c| strip_leading_date(&plain_text(&c[1])))
            .unwrap_or_default();
        results.push(source_row(url, plain_text(&anchor[2]), snippet, None));
        if results.len() >= num_results {
            break;
        }
    }

    if results.is_empty() {
        Err("no results parsed from bing-html output".into())
    } else {
        Ok(results)
    }
}

/// Minimal percent-encoding for query/market values (RFC 3986 unreserved kept).
fn urlencode(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    for byte in value.as_bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(*byte as char)
            }
            _ => out.push_str(&format!("%{byte:02X}")),
        }
    }
    out
}

/* ------------------------------------------------------------------ *
 * Bocha + Exa (pre-existing backends)
 * ------------------------------------------------------------------ */

async fn search_bocha(
    query: &str,
    num_results: usize,
    api_key: &str,
) -> Result<Vec<serde_json::Value>, String> {
    let client = &*SEARCH_CLIENT;
    let resp = client
        .post("https://api.bochaai.com/v1/web-search")
        .header("Authorization", format!("Bearer {}", api_key))
        .header("Content-Type", "application/json")
        .json(&serde_json::json!({
            "query": query,
            "count": num_results,
        }))
        .send()
        .await
        .map_err(|e| format!("Bocha request failed: {}", e))?;

    let status = resp.status();
    let body: serde_json::Value = resp
        .json()
        .await
        .map_err(|e| format!("Bocha response parse failed: {}", e))?;

    let code = body.get("code").and_then(|c| c.as_i64()).unwrap_or(-1);
    if code != 200 {
        let msg = body
            .get("msg")
            .and_then(|m| m.as_str())
            .unwrap_or("unknown error");
        return Err(format!(
            "Bocha API error (code {}): {} (HTTP {})",
            code, msg, status
        ));
    }

    let pages = body
        .get("data")
        .and_then(|d| d.get("webPages"))
        .and_then(|w| w.get("value"))
        .and_then(|v| v.as_array())
        .ok_or_else(|| "Bocha: no data.webPages.value in response".to_string())?;

    let results: Vec<serde_json::Value> = pages
        .iter()
        .take(num_results)
        .map(|p| {
            serde_json::json!({
                "title": p.get("name").and_then(|v| v.as_str()).unwrap_or(""),
                "url": p.get("url").and_then(|v| v.as_str()).unwrap_or(""),
                "snippet": p.get("snippet").and_then(|v| v.as_str()).unwrap_or(""),
            })
        })
        .collect();

    if results.is_empty() {
        Err("no results parsed from Bocha output".into())
    } else {
        Ok(results)
    }
}

fn find_mcporter(working_dir: &str) -> Result<String, String> {
    // Check MCPORTER_PATH env var first
    if let Ok(path) = std::env::var("MCPORTER_PATH") {
        if std::path::Path::new(&path).exists() {
            return Ok(path);
        }
    }
    // Check known install locations
    for candidate in &["/opt/homebrew/bin/mcporter", "/usr/local/bin/mcporter"] {
        if std::path::Path::new(candidate).exists() {
            return Ok(candidate.to_string());
        }
    }
    // Fall back: try relative to working_dir, then PATH
    let from_wd = std::path::Path::new(working_dir).join("../target/release/mcporter");
    if from_wd.exists() {
        return Ok(from_wd.to_string_lossy().to_string());
    }
    Err(
        "mcporter not found. Install via: brew install mcporter, or set MCPORTER_PATH env var"
            .to_string(),
    )
}

fn search_exa(
    query: &str,
    num_results: usize,
    working_dir: &str,
) -> Result<Vec<serde_json::Value>, String> {
    let mcporter = find_mcporter(working_dir)?;

    let config = std::env::var("MCPORTER_CONFIG").unwrap_or_else(|_| {
        // Try relative to working_dir first, then fall back to CWD-relative
        let wd_config = std::path::Path::new(working_dir)
            .join("config")
            .join("mcporter.json");
        if wd_config.exists() {
            return wd_config.to_string_lossy().to_string();
        }
        std::env::current_dir()
            .map(|d| d.join("config").join("mcporter.json"))
            .map(|p| p.to_string_lossy().to_string())
            .unwrap_or_else(|_| "config/mcporter.json".to_string())
    });

    let expr = format!(
        "exa.web_search_exa(query: {:?}, numResults: {})",
        query, num_results
    );

    let output = std::process::Command::new(&mcporter)
        .args(["--config", &config, "call", &expr])
        .output()
        .map_err(|e| format!("mcporter spawn failed: {}", e))?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(format!("mcporter exit {}: {}", output.status, stderr));
    }

    let stdout = String::from_utf8_lossy(&output.stdout);
    let mut results: Vec<serde_json::Value> = Vec::new();
    let mut title = String::new();
    let mut url = String::new();

    for line in stdout.lines() {
        let trimmed = line.trim();
        if trimmed.starts_with("Title: ") {
            title = trimmed.trim_start_matches("Title: ").trim().to_string();
        } else if trimmed.starts_with("URL: ") {
            url = trimmed.trim_start_matches("URL: ").trim().to_string();
        } else if trimmed == "---" && !title.is_empty() && !url.is_empty() {
            results.push(serde_json::json!({"title": title, "url": url, "snippet": ""}));
            title.clear();
            url.clear();
            if results.len() >= num_results {
                break;
            }
        }
    }
    if !title.is_empty() && !url.is_empty() {
        results.push(serde_json::json!({"title": title, "url": url, "snippet": ""}));
    }

    if results.is_empty() {
        Err("no results parsed from Exa output".into())
    } else {
        Ok(results)
    }
}

/* ------------------------------------------------------------------ *
 * `[web_search]` TOML reader (shared with `openzen key set`)
 * ------------------------------------------------------------------ */

/// Read a `key = "value"` from `[section]` with a hand-rolled scanner.
///
/// Deliberately not `toml::from_str` into a typed struct: this file also holds
/// session entries, `[providers.*]` and `[platforms.*]`, and the tools crate
/// must tolerate sections it does not understand.
pub fn extract_toml_value(content: &str, section: &str, key: &str) -> Option<String> {
    let section_header = format!("[{}]", section);
    let mut in_section = false;
    for line in content.lines() {
        let line = line.trim();
        if line.starts_with('[') && line.ends_with(']') {
            in_section = line == section_header;
            continue;
        }
        if !in_section || line.starts_with('#') {
            continue;
        }
        let Some(eq) = line.find('=') else { continue };
        if line[..eq].trim() != key {
            continue;
        }

        let raw = line[eq + 1..].trim();
        // Quoted value: close on the matching quote so a trailing `# comment`
        // (or a `#` inside the value) cannot corrupt the key.
        if let Some(quote) = raw.chars().next().filter(|c| *c == '"' || *c == '\'') {
            let rest = &raw[quote.len_utf8()..];
            let Some(end) = rest.rfind(quote) else { continue };
            let value = &rest[..end];
            return Some(if quote == '"' {
                value.replace("\\\"", "\"").replace("\\\\", "\\")
            } else {
                value.to_string()
            });
        }

        let bare = raw.split('#').next().unwrap_or("").trim();
        if !bare.is_empty() {
            return Some(bare.to_string());
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    /// Write a `[web_search]` config into a temp dir and return its path list.
    fn config_with(body: &str) -> (TempDir, Vec<PathBuf>) {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("mykey.toml");
        std::fs::write(&path, body).unwrap();
        let paths = vec![path];
        (dir, paths)
    }

    #[test]
    fn engine_env_and_toml_keys_are_paired() {
        assert_eq!(engine_env_var("tavily"), Some("TAVILY_API_KEY"));
        assert_eq!(engine_toml_key("tavily"), Some("tavily_api_key"));
        assert_eq!(engine_env_var("firecrawl"), Some("FIRECRAWL_API_KEY"));
        assert_eq!(engine_toml_key("firecrawl"), Some("firecrawl_api_key"));
        assert_eq!(engine_env_var("bocha"), Some("BOCHA_API_KEY"));
        assert_eq!(engine_toml_key("bocha"), Some("bocha_api_key"));
        // Keyless/opt-in engines have no env var at all.
        assert_eq!(engine_env_var("bing-rss"), None);
        assert_eq!(engine_toml_key("bing-html"), None);
    }

    #[test]
    fn resolve_chain_defaults_and_single_engine() {
        // Default engine is keyless: Tavily → Firecrawl, no key required.
        let keyless = resolve_chain_from("keyless", None, None, &[]).unwrap();
        assert_eq!(keyless, split_chain(DEFAULT_KEYLESS_CHAIN));
        assert_eq!(keyless, vec!["tavily", "firecrawl"]);

        let defaults = resolve_chain_from("auto", None, None, &[]).unwrap();
        assert_eq!(defaults, split_chain(DEFAULT_ENGINE_CHAIN));
        assert_eq!(defaults, vec!["tavily", "firecrawl", "bocha", "exa"]);
        assert_eq!(
            resolve_chain_from("", None, None, &[]).unwrap(),
            split_chain(DEFAULT_ENGINE_CHAIN)
        );
        assert_eq!(
            resolve_chain_from("Tavily", None, None, &[]).unwrap(),
            vec!["tavily".to_string()]
        );
        assert!(resolve_chain_from("nope", None, None, &[]).is_err());
    }

    #[test]
    fn tool_schema_advertises_keyless_as_the_default_engine() {
        let params = WebSearchTool.parameters();
        assert_eq!(params["properties"]["engine"]["default"], "keyless");
        let enum_values = params["properties"]["engine"]["enum"].as_array().unwrap();
        assert_eq!(enum_values[0], "keyless");
        assert_eq!(enum_values[1], "auto");
    }

    #[test]
    fn configured_chain_overrides_default_and_is_lowercased() {
        let configured = Some(vec!["tavily".to_string(), "bocha".to_string()]);
        assert_eq!(
            resolve_chain_from("auto", configured, None, &[]).unwrap(),
            vec!["tavily", "bocha"]
        );
        assert_eq!(
            resolve_chain_from("keyless", None, Some(vec!["firecrawl".into(), "tavily".into()]), &[])
                .unwrap(),
            vec!["firecrawl", "tavily"]
        );
    }

    #[test]
    fn bing_engines_require_operator_opt_in() {
        // Explicit request without config: refused, with a pointer to the fix.
        let err = resolve_chain_from("bing-rss", None, None, &[]).unwrap_err();
        assert!(err.contains("opt-in"), "unexpected error: {err}");

        // Listed in config: allowed, both alone and inside the auto chain.
        let enabled = vec!["bing-rss".to_string()];
        assert_eq!(
            resolve_chain_from("bing-rss", None, None, &enabled).unwrap(),
            vec!["bing-rss"]
        );
        assert_eq!(
            resolve_chain_from(
                "auto",
                Some(vec!["tavily".into(), "bing-rss".into()]),
                None,
                &enabled
            )
            .unwrap(),
            vec!["tavily", "bing-rss"]
        );
    }

    #[test]
    fn bing_opt_in_is_read_from_config_lists() {
        let (_dir, paths) = config_with(
            "[web_search]\nengines = \"tavily, firecrawl ,BING-RSS\"\nkeyless_engines = \"tavily,bing-html\"\n",
        );
        assert_eq!(
            read_engine_list(&paths, "engines").unwrap(),
            vec!["tavily", "firecrawl", "bing-rss"]
        );
        let enabled = bing_engines_enabled(&paths);
        assert!(enabled.contains(&"bing-rss".to_string()));
        assert!(enabled.contains(&"bing-html".to_string()));
    }

    #[test]
    fn config_candidates_keep_working_dir_last() {
        let dir = TempDir::new().unwrap();
        let wd = dir.path().to_string_lossy().to_string();
        let candidates = search_config_candidates(&wd);
        assert!(candidates.len() >= 4);
        assert!(candidates.last().unwrap().ends_with("mykey.toml"));
        // Fallback write target must be the data root, never the repo tree.
        let write = search_config_write_path(&wd);
        assert!(write.ends_with("mykey.toml"));
    }

    #[test]
    fn api_key_reads_toml_and_reports_source() {
        let (_dir, paths) = config_with(
            "[web_search]\nbocha_api_key = \"bk-123\"\ntavily_api_key = 'tv-456'\nfirecrawl_api_key = \"fc-789\" # comment\n",
        );

        let bocha = search_api_key_from_paths("bocha", &paths).expect("bocha key");
        assert_eq!(bocha.key, "bk-123");
        assert!(bocha.source.ends_with("mykey.toml"));

        assert_eq!(
            search_api_key_from_paths("tavily", &paths).unwrap().key,
            "tv-456"
        );
        assert_eq!(
            search_api_key_from_paths("firecrawl", &paths).unwrap().key,
            "fc-789"
        );
        assert!(search_api_key_from_paths("bing-rss", &paths).is_none());

        let (_empty, empty_paths) = config_with("[web_search]\nbocha_api_key = \"bk\"\n");
        assert!(search_api_key_from_paths("tavily", &empty_paths).is_none());
    }

    #[test]
    fn entities_and_markup_are_normalized() {
        assert_eq!(plain_text("<b>hello</b>   world"), "hello world");
        assert_eq!(plain_text("a &amp; b &#39;c&#39;"), "a & b 'c'");
        assert_eq!(plain_text("x&nbsp;y"), "x y");
        assert_eq!(strip_leading_date("2025年9月22日 · 正文"), "正文");
        assert_eq!(strip_leading_date("2025-09-22 — body"), "body");
        assert_eq!(strip_leading_date("plain"), "plain");
    }

    #[test]
    fn bing_tracking_urls_are_unwrapped() {
        use base64::Engine as _;
        let target = "https://example.com/article?a=1#frag";
        let encoded = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(target);
        let tracking = format!("https://www.bing.com/ck/a?!&&p=abc&u=a1{encoded}&ntb=1");
        assert_eq!(unwrap_bing_url(&tracking), target);
        assert_eq!(
            normalize_url(&tracking).unwrap(),
            "https://example.com/article?a=1"
        );

        // Padded standard base64 (the older link shape) also resolves.
        let padded = base64::engine::general_purpose::STANDARD.encode(target);
        let tracking2 = format!("https://cn.bing.com/ck/a?u=a1{padded}&ntb=1");
        assert_eq!(unwrap_bing_url(&tracking2), target);

        // A tracking link whose payload is not a URL is left alone.
        let not_a_url = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode("hello");
        let tracking3 = format!("https://www.bing.com/ck/a?u=a1{not_a_url}");
        assert_eq!(unwrap_bing_url(&tracking3), tracking3);

        assert_eq!(
            normalize_url("https://example.com/x#y").unwrap(),
            "https://example.com/x"
        );
        assert!(normalize_url("not a url").is_none());
    }

    #[test]
    fn urlencode_keeps_unreserved_only() {
        assert_eq!(urlencode("a b/c?d=e"), "a%20b%2Fc%3Fd%3De");
        assert_eq!(urlencode("zh-CN"), "zh-CN");
    }

    #[test]
    fn tavily_keyless_hint_is_only_sent_without_a_key() {
        // The vendor rejects a keyed request that still carries the keyless
        // hint, so the header decision is asserted here rather than at runtime.
        let keyless = tavily_headers(None);
        assert_eq!(keyless.len(), 1);
        assert_eq!(keyless[0].0, "x-tavily-access-mode");
        assert_eq!(keyless[0].1, "keyless");

        let keyed = tavily_headers(Some("tvly-dev-abc"));
        assert_eq!(keyed.len(), 1);
        assert_eq!(keyed[0].0, "authorization");
        assert_eq!(keyed[0].1, "Bearer tvly-dev-abc");
        assert!(!keyed.iter().any(|(name, _)| *name == "x-tavily-access-mode"));
    }

    /* ---------------------------------------------------------------- *
     * Live network checks — opt in explicitly:
     *   cargo test -p oz-tools --lib live_ -- --ignored --nocapture
     * ---------------------------------------------------------------- */

    #[tokio::test]
    #[ignore = "hits the live Tavily/Firecrawl APIs (keyless tier)"]
    async fn live_keyless_engines_return_results() {
        for engine in ["tavily", "firecrawl"] {
            let count = probe_engine(engine, "/tmp")
                .await
                .unwrap_or_else(|err| panic!("{engine} keyless probe failed: {err}"));
            assert!(count > 0, "{engine} returned no results");
            println!("{engine} (keyless): {count} results");
        }
    }

    #[tokio::test]
    #[ignore = "hits the live Tavily API with a deliberately invalid key"]
    async fn live_invalid_key_is_reported_not_silently_keyless() {
        let err = probe_engine_with_key("tavily", Some("tvly-dev-definitely-invalid"), "/tmp")
            .await
            .expect_err("an invalid key must fail, not fall back to the keyless tier");
        println!("tavily invalid key -> {err}");
        assert!(
            err.contains("401") || err.contains("Unauthorized"),
            "unexpected error text: {err}"
        );
    }
}
