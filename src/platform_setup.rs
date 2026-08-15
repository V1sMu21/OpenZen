//! Auto-discovery and startup of platform adapters.
//!
//! Reads `[platforms.*]` from mykey.toml at server startup and
//! automatically creates + starts enabled adapters. No manual
//! wiring required — config-only after this module is compiled in.
//!
//! ## How it works
//!
//! ```text
//! config/mykey.toml [platforms.feishu] → PlatformConfig
//!   → FeishuAdapter::new(config) → PlatformRegistry::register()
//!     → PlatformRegistry::start_all() → tokio::spawn adapter loop
//! ```

use std::collections::HashMap;
use std::path::Path;
use std::sync::atomic::AtomicBool;
use std::sync::{Arc, Mutex};

use oz_platform::{AgentBridge, PlatformAdapter, PlatformContext, PlatformRegistry};

/// Scan `config_path` for `[platforms.*]` sections, create adapters for
/// every enabled platform, and start them.  This is the single entry-point
/// that lets the rest of the app be platform-agnostic.
#[allow(clippy::too_many_arguments, clippy::type_complexity)]
pub async fn discover_and_start_platforms(
    config_path: &str,
    working_dir: &str,
    assets_dir: &str,
    script_dir: &str,
    sessions: Arc<Mutex<oz_server::webui::sessions::SessionStore>>,
    running_agents: Arc<Mutex<HashMap<String, tokio::task::JoinHandle<()>>>>,
    stop_signals: Arc<Mutex<HashMap<String, Arc<AtomicBool>>>>,
    ask_user_rxs: Arc<Mutex<HashMap<String, Arc<Mutex<Option<String>>>>>>,
    approval_handler: Arc<Mutex<Option<Arc<dyn oz_safety::ApprovalHandler>>>>,
    skill_mcp_dir: Option<String>,
    locale: Arc<Mutex<String>>,
) {
    let cfg_path = Path::new(config_path);
    if !cfg_path.exists() {
        tracing::info!("[platform] no config at {config_path}, skipping");
        return;
    }

    let raw: toml::Table = match std::fs::read_to_string(cfg_path)
        .ok()
        .and_then(|s| toml::from_str(&s).ok())
    {
        Some(t) => t,
        None => return,
    };

    let platforms_table = match raw.get("platforms").and_then(|v| v.as_table()) {
        Some(t) => t,
        None => {
            tracing::info!("[platform] no [platforms] section in config");
            return;
        }
    };

    let bridge = Arc::new(AgentBridge {
        sessions,
        running_agents,
        stop_signals,
        config_path: config_path.to_string(),
        working_dir: working_dir.to_string(),
        assets_dir: assets_dir.to_string(),
        script_dir: script_dir.to_string(),
        locale,
        approval_handler,
        ask_user_rxs,
        skill_mcp_dir,
    });

    let mut registry = PlatformRegistry::new();

    for (name, val) in platforms_table {
        let config: oz_platform::PlatformConfig = match val.clone().try_into() {
            Ok(c) => c,
            Err(e) => {
                tracing::warn!("[platform] {name}: bad config — {e}");
                continue;
            }
        };

        if !config.enabled {
            tracing::info!("[platform] {name}: disabled, skipping");
            continue;
        }

        let adapter: Option<Arc<dyn PlatformAdapter>> = match name.as_str() {
            "feishu" => oz_platform_feishu::FeishuAdapter::new(&config)
                .ok()
                .map(|a| Arc::new(a) as Arc<dyn PlatformAdapter>),
            "telegram" => oz_platform_telegram::TelegramAdapter::new(&config)
                .ok()
                .map(|a| Arc::new(a) as Arc<dyn PlatformAdapter>),
            "wechat" => Some(Arc::new(oz_platform_wechat::WechatAdapter::new(&config))
                as Arc<dyn PlatformAdapter>),
            other => {
                tracing::warn!("[platform] unknown platform: {other}");
                None
            }
        };

        if let Some(a) = adapter {
            tracing::info!("[platform] auto-registered: {}", a.name());
            registry.register(a);
        }
    }

    if !registry.is_empty() {
        let ctx = PlatformContext {
            agent: bridge,
            platform_config: oz_platform::PlatformConfig::default(),
            working_dir: std::path::PathBuf::from(working_dir),
        };
        registry.start_all(ctx);
    }
}

use std::path::PathBuf;

pub async fn handle_platform_command(
    action: &crate::PlatformAction,
    config_path: &Path,
) -> anyhow::Result<()> {
    match action {
        crate::PlatformAction::Add {
            name,
            app_id,
            app_secret,
            bot_token,
            model,
            allowed_users,
            proxy,
        } => {
            let path = resolve_config_path(config_path);
            let mut raw: toml::Table = if path.exists() {
                let content = std::fs::read_to_string(&path)?;
                toml::from_str(&content).unwrap_or_default()
            } else {
                toml::Table::new()
            };

            let allowed: toml::Value = if allowed_users == "*" {
                toml::Value::Array(vec![toml::Value::String("*".into())])
            } else {
                toml::Value::Array(
                    allowed_users
                        .split(',')
                        .map(|s| toml::Value::String(s.trim().into()))
                        .collect(),
                )
            };

            let mut platform = toml::Table::new();
            platform.insert("enabled".into(), toml::Value::Boolean(true));
            platform.insert("default_model".into(), toml::Value::String(model.clone()));
            platform.insert("allowed_users".into(), allowed);
            if let Some(ref id) = *app_id {
                platform.insert("app_id".into(), toml::Value::String(id.clone()));
            }
            if let Some(ref s) = *app_secret {
                platform.insert("app_secret".into(), toml::Value::String(s.clone()));
            }
            if let Some(ref t) = *bot_token {
                platform.insert("bot_token".into(), toml::Value::String(t.clone()));
            }
            if let Some(ref p) = *proxy {
                platform.insert("proxy".into(), toml::Value::String(p.clone()));
            }

            let existing_platforms = raw
                .remove("platforms")
                .and_then(|v| v.try_into::<toml::Table>().ok())
                .unwrap_or_default();
            let mut platforms = existing_platforms;
            platforms.insert(name.clone(), toml::Value::Table(platform));
            raw.insert("platforms".into(), toml::Value::Table(platforms));

            if let Some(parent) = path.parent() {
                std::fs::create_dir_all(parent)?;
            }
            std::fs::write(&path, toml::to_string_pretty(&raw)?)?;
            println!("✅ Platform '{name}' configured in {}", path.display());
            println!("   enabled=true  model={model}  allowed_users={allowed_users}");
            Ok(())
        }
        crate::PlatformAction::List => {
            let path = resolve_config_path(config_path);
            if !path.exists() {
                println!("No config file at {}", path.display());
                return Ok(());
            }
            let raw: toml::Table = toml::from_str(&std::fs::read_to_string(&path)?)?;
            match raw.get("platforms").and_then(|v| v.as_table()) {
                Some(t) if !t.is_empty() => {
                    println!("Configured platforms:");
                    for (name, val) in t {
                        let enabled = val
                            .get("enabled")
                            .and_then(|v| v.as_bool())
                            .unwrap_or(false);
                        let model = val
                            .get("default_model")
                            .and_then(|v| v.as_str())
                            .unwrap_or("?");
                        println!("  {}  enabled={}  model={}", name, enabled, model);
                    }
                }
                _ => println!("No platforms configured."),
            }
            Ok(())
        }
    }
}

fn resolve_config_path(cli_config: &Path) -> PathBuf {
    if cli_config.exists() {
        return cli_config.to_path_buf();
    }
    let home = std::env::var("HOME").unwrap_or_else(|_| "/tmp".into());
    let candidates = [
        PathBuf::from("config/mykey.toml"),
        PathBuf::from(&home).join(".openzen/mykey.toml"),
        PathBuf::from(&home).join("mykey.toml"),
    ];
    for c in &candidates {
        if c.exists() {
            return c.clone();
        }
    }
    PathBuf::from("config/mykey.toml")
}
