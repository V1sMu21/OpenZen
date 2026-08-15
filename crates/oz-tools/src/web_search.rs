use async_trait::async_trait;
use oz_core_types::{ToolContext, ToolError, ToolOutput};
use crate::registry::ToolHandler;

pub struct WebSearchTool;

#[async_trait]
impl ToolHandler for WebSearchTool {
    fn name(&self) -> String { "web_search".to_string() }
    fn description(&self) -> String {
        "Search the web via Bocha (primary engine, works from mainland China, covers both domestic and international sources) with automatic Exa fallback. Returns result titles, URLs, and snippets."
            .to_string()
    }
    fn description_zh(&self) -> String {
        "网络搜索（博查主力引擎，国内直连可用，覆盖国内外来源；Exa 自动回退）。返回结果标题、URL 和摘要。"
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
                    "description": "Max results (default 5)",
                    "default": 5
                },
                "engine": {
                    "type": "string",
                    "enum": ["auto", "bocha", "exa"],
                    "description": "Search engine: auto (Bocha first, Exa fallback), bocha (Bocha only), exa (Exa only). Default auto",
                    "default": "auto"
                }
            },
            "required": ["query"]
        })
    }

    async fn execute(&self, args: serde_json::Value, ctx: &ToolContext) -> Result<ToolOutput, ToolError> {
        let query = args.get("query")
            .and_then(|v| v.as_str())
            .ok_or_else(|| ToolError::Custom("missing 'query' parameter".into()))?;

        let num_results = args.get("num_results")
            .and_then(|v| v.as_u64())
            .unwrap_or(5)
            .min(10) as usize;

        let engine = args.get("engine")
            .and_then(|v| v.as_str())
            .unwrap_or("auto");

        let (results, used_engine) = match engine {
            "bocha" => {
                let key = read_bocha_api_key(&ctx.working_dir)
                    .ok_or_else(|| ToolError::Custom(
                        "Bocha API key not configured. Set BOCHA_API_KEY env var or add [web_search] bocha_api_key to mykey.toml".into()
                    ))?;
                let r = search_bocha(query, num_results, &key)
                    .await
                    .map_err(|e| ToolError::Custom(format!("Bocha search failed: {}", e)))?;
                (r, "bocha")
            }
            "exa" => {
                let r = search_exa(query, num_results, &ctx.working_dir)
                    .map_err(|e| ToolError::Custom(format!("Exa search failed: {}", e)))?;
                (r, "exa")
            }
            _ => {
                match read_bocha_api_key(&ctx.working_dir) {
                    Some(key) => match search_bocha(query, num_results, &key).await {
                        Ok(r) => (r, "bocha"),
                        Err(bocha_err) => match search_exa(query, num_results, &ctx.working_dir) {
                            Ok(r) => (r, "exa"),
                            Err(exa_err) => {
                                return Err(ToolError::Custom(format!(
                                    "all engines failed: bocha: {}; exa: {}",
                                    bocha_err, exa_err
                                )))
                            }
                        },
                    },
                    None => {
                        let r = search_exa(query, num_results, &ctx.working_dir)
                            .map_err(|e| ToolError::Custom(format!(
                                "Bocha key not configured and Exa failed: {}", e)))?;
                        (r, "exa")
                    }
                }
            }
        };

        Ok(ToolOutput::success(serde_json::json!({
            "query": query,
            "engine": used_engine,
            "results": results,
            "total": results.len()
        })))
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
    for candidate in &[
        "/opt/homebrew/bin/mcporter",
        "/usr/local/bin/mcporter",
    ] {
        if std::path::Path::new(candidate).exists() {
            return Ok(candidate.to_string());
        }
    }
    // Fall back: try relative to working_dir, then PATH
    let from_wd = std::path::Path::new(working_dir).join("../target/release/mcporter");
    if from_wd.exists() {
        return Ok(from_wd.to_string_lossy().to_string());
    }
    Err("mcporter not found. Install via: brew install mcporter, or set MCPORTER_PATH env var".to_string())
}

fn search_exa(query: &str, num_results: usize, working_dir: &str) -> Result<Vec<serde_json::Value>, String> {
    let mcporter = find_mcporter(working_dir)?;

    let config = std::env::var("MCPORTER_CONFIG")
        .unwrap_or_else(|_| {
            // Try relative to working_dir first, then fall back to CWD-relative
            let wd_config = std::path::Path::new(working_dir).join("config").join("mcporter.json");
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
            if results.len() >= num_results { break; }
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

async fn search_bocha(
    query: &str,
    num_results: usize,
    api_key: &str,
) -> Result<Vec<serde_json::Value>, String> {
    let client = reqwest::Client::new();
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
        let msg = body.get("msg").and_then(|m| m.as_str()).unwrap_or("unknown error");
        return Err(format!("Bocha API error (code {}): {} (HTTP {})", code, msg, status));
    }

    let pages = body
        .get("data")
        .and_then(|d| d.get("webPages"))
        .and_then(|w| w.get("value"))
        .and_then(|v| v.as_array())
        .ok_or_else(|| "Bocha: no data.webPages.value in response".to_string())?;

    let results: Vec<serde_json::Value> = pages.iter().take(num_results).map(|p| {
        serde_json::json!({
            "title": p.get("name").and_then(|v| v.as_str()).unwrap_or(""),
            "url": p.get("url").and_then(|v| v.as_str()).unwrap_or(""),
            "snippet": p.get("snippet").and_then(|v| v.as_str()).unwrap_or(""),
        })
    }).collect();

    if results.is_empty() {
        Err("no results parsed from Bocha output".into())
    } else {
        Ok(results)
    }
}

fn read_bocha_api_key(working_dir: &str) -> Option<String> {
    if let Ok(key) = std::env::var("BOCHA_API_KEY") {
        let key = key.trim();
        if !key.is_empty() {
            return Some(key.to_string());
        }
    }

    let home = std::env::var("HOME").unwrap_or_else(|_| "/tmp".into());
    let candidates = [
        std::path::PathBuf::from(&home).join(".openzen").join("mykey.toml"),
        std::path::PathBuf::from(&home).join("mykey.toml"),
        std::path::PathBuf::from(working_dir).join("config").join("mykey.toml"),
        std::path::PathBuf::from(working_dir).join("mykey.toml"),
    ];

    for path in candidates {
        if let Ok(content) = std::fs::read_to_string(&path) {
            if let Some(key) = extract_toml_value(&content, "web_search", "bocha_api_key") {
                if !key.is_empty() {
                    return Some(key);
                }
            }
        }
    }
    None
}

fn extract_toml_value(content: &str, section: &str, key: &str) -> Option<String> {
    let section_header = format!("[{}]", section);
    let mut in_section = false;
    for line in content.lines() {
        let line = line.trim();
        if line.starts_with('[') && line.ends_with(']') {
            in_section = line == section_header;
            continue;
        }
        if !in_section {
            continue;
        }
        let Some(eq) = line.find('=') else { continue };
        let k = line[..eq].trim();
        if k != key {
            continue;
        }
        let v = line[eq + 1..].trim();
        if v.starts_with('"') && v.len() >= 2 && v.ends_with('"') {
            return Some(v[1..v.len() - 1].replace("\\\"", "\"").replace("\\\\", "\\"));
        }
        if v.starts_with('\'') && v.len() >= 2 && v.ends_with('\'') {
            return Some(v[1..v.len() - 1].to_string());
        }
        let bare = v.split('#').next().unwrap_or("").trim();
        if !bare.is_empty() {
            return Some(bare.to_string());
        }
    }
    None
}
