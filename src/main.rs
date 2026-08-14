use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::atomic::AtomicBool;
use std::sync::{Arc, Mutex};

use clap::{Parser, Subcommand};

mod daemon;
mod platform_setup;
mod upgrade;

use oz_config::mykey::{MyKeyConfig, SessionConfig, SessionType};
use oz_core::handler::LoopConfig;
use oz_core_types::ToolContext;
use oz_memory::MemorySystem;
use oz_tools::handler::ToolRegistryHandler;
use oz_tools::registry::ToolRegistry;
use oz_agent::Agent;

#[derive(Parser)]
#[command(name = "openzen", version, about = "OpenZen — Rust rewrite")]
struct Cli {
    #[command(subcommand)]
    command: Commands,

    #[arg(short, long, default_value = "config/mykey.toml")]
    config: PathBuf,

    #[arg(short, long, default_value = "assets")]
    assets: PathBuf,

    #[arg(short, long, default_value = ".")]
    dir: PathBuf,

    #[arg(long, default_value_t = false)]
    json_log: bool,

    /// Directory containing SOP (.md) files for context injection.
    /// Deprecated: use --skill-mcp-dir for unified skill/MCP registry management.
    #[arg(long)]
    sop_dir: Option<PathBuf>,

    /// Directory containing the .skill_mcp/ registry (skills, SOPs, facts).
    #[arg(long)]
    skill_mcp_dir: Option<PathBuf>,

    /// Directory containing WASM plugin (.wasm) files to auto-load.
    #[arg(long)]
    plugin_dir: Option<PathBuf>,
}

#[derive(Subcommand)]
enum Commands {
    /// Ask the agent a question and get a response
    Ask {
        prompt: String,
        #[arg(short, long, default_value = "default")]
        session: String,
        #[arg(short, long, default_value_t = 30)]
        turns: u32,
        /// Use smart routing (cheap model for simple tasks, flagship for complex)
        #[arg(long, default_value_t = false)]
        smart: bool,
    },
    /// Start the WebUI server
    Serve {
        #[arg(short, long, default_value_t = 18567)]
        port: u16,
        #[arg(long)]
        frontend_dir: Option<PathBuf>,
    },
    /// Reflection modes (goal/autonomous)
    Reflect {
        #[arg(short, long)]
        goal: Option<String>,
        #[arg(short, long, default_value_t = 60.0)]
        budget: f64,
        #[arg(short, long)]
        autonomous: bool,
    },
    /// Start the MCP server
    Mcp {
        #[arg(short, long, default_value_t = 8080)]
        port: u16,
    },
    /// Start the Terminal UI (TUI) chat interface
    Tui {
        #[arg(short, long, default_value = "default")]
        session: String,
    },
    /// Manage WASM plugins
    Plugin {
        #[command(subcommand)]
        action: PluginAction,
    },
    /// Self-upgrade: build, health-check, and swap the binary atomically.
    /// When run inside a daemon, sends an upgrade signal to it.
    /// When run standalone, performs the upgrade and prints instructions.
    Upgrade {
        /// Path to a pre-built binary (skip cargo build).
        #[arg(long)]
        binary: Option<PathBuf>,
        /// Port to health-check the new binary on (default: 18567).
        #[arg(long, default_value_t = 18567)]
        health_port: u16,
        /// If true, force the upgrade without health-check prompt.
        #[arg(long, default_value_t = false)]
        force: bool,
    },
    /// Run the web server in daemon mode with auto-restart
    Daemon {
        #[arg(short, long, default_value_t = 18567)]
        port: u16,
        #[arg(long)]
        frontend_dir: Option<PathBuf>,
        #[arg(long, default_value_t = 10)]
        max_restarts: u32,
        #[arg(long)]
        pid_file: Option<PathBuf>,
    },
    /// Manage AI Agents (aichat-style: instructions + tools + documents)
    Agent {
        /// Agent name to load from ~/.openzen/agents/<name>/
        name: Option<String>,
        /// List all available agents
        #[arg(long, default_value_t = false)]
        list: bool,
    },
    /// Manage messaging platforms (one-step config)
    Platform {
        #[command(subcommand)]
        action: PlatformAction,
    },
}

#[derive(Subcommand)]
enum PlatformAction {
    /// Add or update a platform integration (one command, no TOML editing)
    Add {
        /// Platform name: feishu, telegram, qq, wechat
        name: String,
        /// App ID (feishu, qq)
        #[arg(long)]
        app_id: Option<String>,
        /// App Secret (feishu, qq)
        #[arg(long)]
        app_secret: Option<String>,
        /// Bot Token (telegram)
        #[arg(long)]
        bot_token: Option<String>,
        /// LLM model name to use (default: local)
        #[arg(long, default_value = "local")]
        model: String,
        /// Allowed user IDs (comma-separated, default: * = all)
        #[arg(long, default_value = "*")]
        allowed_users: String,
        /// HTTP proxy URL
        #[arg(long)]
        proxy: Option<String>,
    },
    /// List configured platforms
    List,
}

#[derive(Subcommand)]
enum PluginAction {
    /// Load a WASM plugin from a .wasm file
    Load {
        /// Path to the .wasm file
        path: PathBuf,
    },
    /// List loaded plugins
    List,
}

fn init_tracing(json: bool) {
    let filter = tracing_subscriber::EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| "info".into());
    if json {
        tracing_subscriber::fmt()
            .with_env_filter(filter)
            .json()
            .with_current_span(true)
            .with_span_list(true)
            .init();
    } else {
        tracing_subscriber::fmt()
            .with_env_filter(filter)
            .init();
    }
}

async fn shutdown_signal() {
    let ctrl_c = tokio::signal::ctrl_c();
    tokio::select! {
        _ = ctrl_c => {
            tracing::info!("Ctrl+C received, shutting down...");
        }
    }
}

fn resolve_assets_dir(cli_assets: &PathBuf, _cli_dir: &PathBuf) -> PathBuf {
    if cli_assets.is_absolute() {
        cli_assets.clone()
    } else if cli_assets.exists() {
        std::env::current_dir().unwrap_or_default().join(cli_assets)
    } else {
        let candidates = [
            cli_assets.clone(),
            PathBuf::from("assets"),
            PathBuf::from("../assets"),
        ];
        for c in &candidates {
            if c.join("tools_schema.json").exists() {
                return c.clone();
            }
        }
        cli_assets.clone()
    }
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();
    init_tracing(cli.json_log);

    let assets_dir = resolve_assets_dir(&cli.assets, &cli.dir);

    let result = match &cli.command {
        Commands::Ask { prompt, session, turns, smart } => {
            let config_path = cli.config.as_path();
            let cfg = MyKeyConfig::from_file(config_path)
                .map_err(|e| anyhow::anyhow!("Failed to load config from {}: {}", config_path.display(), e))?;

            let sess_config = cfg.get(session)
                .ok_or_else(|| anyhow::anyhow!("Session '{}' not found in config", session))?;
            let sess_type = cfg.session_type(session);

            run_ask(prompt, sess_config, sess_type, *turns, &assets_dir, &cli.dir, &cli.config, cli.sop_dir.clone(), cli.plugin_dir.clone(), cli.skill_mcp_dir.clone(), *smart).await
        }
        Commands::Serve { port, frontend_dir } => {
            let config_path = resolve_path(&cli.config, &cli.dir);
            let assets_str = assets_dir.to_string_lossy().to_string();
            let dir_str = cli.dir.to_string_lossy().to_string();
            let frontend_str = frontend_dir.as_ref().map(|p| p.to_string_lossy().to_string());

            let sessions = Arc::new(Mutex::new(
                oz_server::webui::sessions::SessionStore::persisted(
                    std::path::PathBuf::from(&dir_str).join("openzen/sessions.json"),
                ),
            ));
            let running_agents = Arc::new(Mutex::new(HashMap::new()));
            let stop_signals = Arc::new(Mutex::new(HashMap::new()));
            let ask_user_rxs = Arc::new(Mutex::new(HashMap::new()));
            let approval_handler = Arc::new(Mutex::new(None::<Arc<dyn oz_safety::ApprovalHandler>>));
            let locale = Arc::new(Mutex::new(
                std::env::var("OZ_LANG").unwrap_or_else(|_| "zh".into()),
            ));
            platform_setup::discover_and_start_platforms(
                &config_path,
                &dir_str,
                &assets_str,
                &assets_str,
                sessions,
                running_agents,
                stop_signals,
                ask_user_rxs,
                approval_handler,
                cli.skill_mcp_dir
                    .as_ref()
                    .map(|p| p.to_string_lossy().to_string()),
                locale,
            )
            .await;

            tokio::select! {
                result = oz_server::webui::serve_webui(
                    *port,
                    config_path,
                    assets_str,
                    dir_str,
                    frontend_str,
                    None,
                ) => result,
                _ = shutdown_signal() => {
                    tracing::info!("Server shut down gracefully");
                    Ok(())
                }
            }
        }
        Commands::Reflect { goal, budget, autonomous } => {
            run_reflect(goal.clone(), *budget, *autonomous, &cli.dir).await
        }
        Commands::Mcp { port } => {
            let ctx = ToolContext {
                working_dir: cli.dir.to_string_lossy().to_string(),
                assets_dir: assets_dir.to_string_lossy().to_string(),
                script_dir: assets_dir.to_string_lossy().to_string(),
                lang: std::env::var("OZ_LANG").unwrap_or_default(),
                skill_mcp_dir: None,
                harness_dir: None,
                session_id: String::new(),
            };
            let registry = ToolRegistry::build_default();
            let state = Arc::new(tokio::sync::Mutex::new(
                oz_server::McpState::new(registry, ctx)
            ));
            tracing::info!("Starting MCP server on port {port}");
            oz_server::sse::serve(state, *port).await
        }
        Commands::Tui { session } => {
            let config_path = cli.config.as_path();
            let cfg = MyKeyConfig::from_file(config_path)
                .map_err(|e| anyhow::anyhow!("Failed to load config: {}", e))?;
            let sess_config = cfg.get(session)
                .ok_or_else(|| anyhow::anyhow!("Session '{}' not found", session))?;
            let sess_type = cfg.session_type(session);
            let assets_str = assets_dir.to_string_lossy().to_string();
            let dir_str = cli.dir.to_string_lossy().to_string();
            oz_tui::run_tui(sess_config.clone(), sess_type, &assets_str, &dir_str, config_path.to_str().unwrap_or_default()).await
        }
        Commands::Plugin { action } => {
            run_plugin(action).await
        }
        Commands::Upgrade { binary, health_port, force } => {
            // Check if there's a daemon running by trying the watch channel
            // In standalone mode, just perform the upgrade
            if *force {
                tracing::info!("Forcing upgrade (standalone mode)");

                let upgrade_cfg = upgrade::UpgradeConfig {
                    current_exe: std::env::current_exe()
                        .unwrap_or_else(|_| PathBuf::from("ga")),
                    project_dir: cli.dir.clone(),
                    health_check_port: *health_port,
                    provided_binary: binary.clone(),
                    ..Default::default()
                };

                let result = upgrade::perform_upgrade(&upgrade_cfg).await;
                if result.success {
                    tracing::info!(
                        "Upgrade successful! Restart the server to use the new binary."
                    );
                    Ok(())
                } else {
                    anyhow::bail!("Upgrade failed: {:?}", result.error);
                }
            } else {
                // Signal the daemon (if running) to perform the upgrade
                tracing::info!("Upgrade via daemon signal requested");
                tracing::info!("Run with --force to upgrade without a running daemon");

                // Try to find the daemon PID and signal it via file
                let pid_file = cli.dir.join("ga-daemon.pid");
                if pid_file.exists() {
                    let pid_str = tokio::fs::read_to_string(&pid_file).await
                        .unwrap_or_default();
                    let pid: u32 = pid_str.trim().parse().unwrap_or(0);
                    if pid > 0 {
                        tracing::info!("Found daemon PID {pid}, signaling upgrade...");
                        // We use SIGUSR1 or similar - but for simplicity,
                        // just print instructions for now
                        tracing::info!("Send 'ga upgrade --force' after daemon restarts");
                    }
                }

                // Perform standalone upgrade anyway
                let upgrade_cfg = upgrade::UpgradeConfig {
                    current_exe: std::env::current_exe()
                        .unwrap_or_else(|_| PathBuf::from("ga")),
                    project_dir: cli.dir.clone(),
                    health_check_port: *health_port,
                    provided_binary: binary.clone(),
                    ..Default::default()
                };

                let result = upgrade::perform_upgrade(&upgrade_cfg).await;
                if result.success {
                    tracing::info!("Upgrade successful! Restart the server.");
                    Ok(())
                } else {
                    anyhow::bail!("Upgrade failed: {:?}", result.error);
                }
            }
        }
        Commands::Daemon { port, frontend_dir, max_restarts, pid_file } => {
            let (cmd_tx, cmd_rx) = daemon::DaemonConfig::new_command_channel();
            let config = daemon::DaemonConfig {
                port: *port,
                config: cli.config.clone(),
                assets: cli.assets.clone(),
                dir: cli.dir.clone(),
                frontend_dir: frontend_dir.clone(),
                max_restarts: *max_restarts,
                pid_file: pid_file.clone().or_else(|| {
                    Some(cli.dir.join("ga-daemon.pid"))
                }),
                health_check_interval_secs: 10,
                cmd_rx,
                cmd_tx,
            };
            daemon::run_daemon(config).await
        }
        Commands::Platform { action } => {
            platform_setup::handle_platform_command(action, &cli.config).await
        }
        Commands::Agent { name, list } => {
            let agents_dir = oz_agent::agents_dir();
            if *list {
                match Agent::list(&agents_dir) {
                    Ok(names) if names.is_empty() => {
                        println!("No agents found. Create one at {}/<name>/config.yaml", agents_dir.display());
                        Ok(())
                    }
                    Ok(names) => {
                        println!("Available agents:");
                        for (i, n) in names.iter().enumerate() {
                            if let Ok(a) = Agent::load(n, &agents_dir) {
                                let tools = a.config.use_tools.as_deref().unwrap_or("all");
                                let model = if a.config.model.is_empty() { "default" } else { &a.config.model };
                                println!("  {}. {:<30} model: {:<20} tools: {}", i + 1, n, model, tools);
                            } else {
                                println!("  {}. {:<30} [config error]", i + 1, n);
                            }
                        }
                        Ok(())
                    }
                    Err(e) => {
                        eprintln!("Error listing agents: {e}");
                        Ok(())
                    }
                }
            } else if let Some(agent_name) = name.as_deref() {
                let agent = Agent::load(agent_name, &agents_dir)?;
                let instructions = agent.interpolate_instructions("");
                let tools = agent.config.use_tools.as_deref().unwrap_or("all");
                let model = if agent.config.model.is_empty() { "default" } else { &agent.config.model };
                println!("Agent: {}\ntools: {}\nmodel: {}\n\n{}",
                    agent_name, tools, model, instructions.trim());
                Ok(())
            } else {
                eprintln!("Usage: ga agent <name>   or   ga agent --list");
                Ok(())
            }
        }
    };

    result
}

fn resolve_path(path: &PathBuf, base: &PathBuf) -> String {
    if path.is_absolute() {
        path.to_string_lossy().to_string()
    } else if path.exists() {
        base.join(path).to_string_lossy().to_string()
    } else {
        path.to_string_lossy().to_string()
    }
}

fn build_session(
    sess_type: SessionType,
    config: &SessionConfig,
    cfg: Option<&MyKeyConfig>,
    session_name: Option<&str>,
) -> anyhow::Result<Box<dyn oz_llm::Session>> {
    match sess_type {
        SessionType::Claude => Ok(Box::new(oz_llm::ClaudeSession::new(config.clone()))),
        SessionType::Oai => Ok(Box::new(oz_llm::OaiSession::new(config.clone()))),
        SessionType::NativeClaude => Ok(Box::new(oz_llm::NativeClaudeSession::new(config.clone()))),
        SessionType::NativeOai => Ok(Box::new(oz_llm::NativeOAISession::new(config.clone()))),
        SessionType::Mixin => {
            let full_cfg = cfg.ok_or_else(|| anyhow::anyhow!("Mixin requires full config"))?;
            let mut session_list: Vec<(String, &SessionConfig)> = full_cfg.sessions.iter()
                .filter(|(name, _)| !name.to_lowercase().contains("mixin"))
                .map(|(k, v)| (k.clone(), v))
                .collect();
            session_list.sort_by(|a, b| a.0.cmp(&b.0));

            // Choose sessions to mix: use llm_nos if specified, otherwise all
            let indices: Vec<usize> = if let Some(ref nos) = config.llm_nos {
                nos.iter().map(|i| *i).collect()
            } else {
                (0..session_list.len()).collect()
            };

            if indices.is_empty() {
                anyhow::bail!("Mixin session '{}' has no referenced sessions", session_name.unwrap_or("?"));
            }

            let mut sessions: Vec<Box<dyn oz_llm::Session>> = Vec::new();
            for &idx in &indices {
                if idx >= session_list.len() {
                    anyhow::bail!("Mixin index {idx} out of range (max {})", session_list.len() - 1);
                }
                let (name, sess_cfg) = &session_list[idx];
                let st = SessionType::from_key_name(name);
                let session: Box<dyn oz_llm::Session> = match st {
                    SessionType::Claude => Box::new(oz_llm::ClaudeSession::new((*sess_cfg).clone())),
                    SessionType::Oai => Box::new(oz_llm::OaiSession::new((*sess_cfg).clone())),
                    SessionType::NativeClaude => Box::new(oz_llm::NativeClaudeSession::new((*sess_cfg).clone())),
                    SessionType::NativeOai => Box::new(oz_llm::NativeOAISession::new((*sess_cfg).clone())),
                    _ => anyhow::bail!("Mixin cannot reference another mixin session '{name}'"),
                };
                sessions.push(session);
            }

            let max_retries = config.max_retries;
            let base_delay = config.base_delay;
            let spring_back = config.spring_back;

            Ok(Box::new(oz_llm::MixinSession::new(
                sessions, None, max_retries, base_delay, spring_back,
            )))
        }
    }
}

async fn run_ask(
    prompt: &str,
    sess_config: &SessionConfig,
    sess_type: SessionType,
    max_turns: u32,
    assets_dir: &PathBuf,
    working_dir: &PathBuf,
    config_path: &PathBuf,
    sop_dir: Option<PathBuf>,
    plugin_dir: Option<PathBuf>,
    skill_mcp_dir: Option<PathBuf>,
    smart: bool,
) -> anyhow::Result<()> {
    let lang = std::env::var("OZ_LANG").unwrap_or_default();
    let sys_prompt_path = if lang == "en" { "sys_prompt_en.txt" } else { "sys_prompt.txt" };
    let ctx = ToolContext {
        working_dir: working_dir.to_string_lossy().to_string(),
        assets_dir: assets_dir.to_string_lossy().to_string(),
        script_dir: assets_dir.to_string_lossy().to_string(),
        lang: lang.clone(),
        skill_mcp_dir: skill_mcp_dir.as_ref().map(|p| p.to_string_lossy().to_string()),
        harness_dir: None,
        session_id: String::new(),
    };

    let memory = MemorySystem::new(working_dir, &lang);
    let memory_context = memory.get_global_memory().await.unwrap_or_default();

    let mut registry = ToolRegistry::build_default();

    if let Some(ref pdir) = plugin_dir {
        if pdir.exists() && pdir.is_dir() {
            if let Ok(entries) = std::fs::read_dir(pdir) {
                for entry in entries.flatten() {
                    let path = entry.path();
                    if path.extension().and_then(|e| e.to_str()) == Some("wasm") {
                        match oz_plugin::WasmPlugin::from_file(&path) {
                            Ok(plugin) => {
                                let name = plugin.name.clone();
                                let handler = oz_plugin::WasmPluginHandler::new(plugin);
                                registry.register_with_name(&name, handler);
                                tracing::info!("Loaded WASM plugin: {name}");
                            }
                            Err(e) => tracing::warn!("Failed to load WASM plugin {}: {e}", path.display()),
                        }
                    }
                }
            }
        }
    }

    let definitions = registry.to_schema("en");
    let mut handler = ToolRegistryHandler::new(registry);

    // Load full config for Mixin session resolution
    let full_cfg = MyKeyConfig::from_file(config_path)
        .map_err(|e| anyhow::anyhow!("Failed to load config: {}", e))?;
    let backend: Box<dyn oz_llm::Session> = build_session(
        sess_type, sess_config, Some(&full_cfg), Some("ask")
    )?;

    let backend: Box<dyn oz_llm::Session> = if smart {
        // Try to find a "cheap" session for smart routing
        let cheap_session = if let Some(cheap_cfg) = full_cfg.get("cheap") {
            let cheap_type = full_cfg.session_type("cheap");
            build_session(cheap_type, cheap_cfg, Some(&full_cfg), Some("cheap")).ok()
        } else {
            None
        };

        match cheap_session {
            Some(cheap) => {
                tracing::info!("Smart routing enabled: cheap + flagship");
                Box::new(oz_llm::smart_router::SmartRouterSession::new(cheap, backend))
            }
            None => {
                tracing::warn!("--smart flag used but no 'cheap' session found in config. Using single session.");
                backend
            }
        }
    } else {
        backend
    };

    let mut client = oz_llm::NativeToolClient::new(backend);

    let sys_prompt_path = assets_dir.join(sys_prompt_path);
    let mut system_prompt = if sys_prompt_path.exists() {
        std::fs::read_to_string(&sys_prompt_path)?
    } else {
        String::new()
    };

    if !memory_context.is_empty() {
        system_prompt.push_str("\n\n## Persistent Memory Context\n\n");
        system_prompt.push_str(&memory_context);
    }

    let mut loop_config = LoopConfig::default();
    loop_config.max_turns = max_turns;
    loop_config.sop_dir = sop_dir.map(|p| p.to_string_lossy().to_string());
    loop_config.skill_mcp_dir = skill_mcp_dir.map(|p| p.to_string_lossy().to_string());
    loop_config.working_dir = working_dir.to_string_lossy().to_string();
    let stop_signal = AtomicBool::new(false);

    tracing::info!("Starting agent loop (max_turns={max_turns})");

    let outcome = oz_core::agent_loop::run_agent_loop(
        &mut client,
        system_prompt,
        prompt.to_string(),
        vec![],
        &mut handler,
        &definitions,
        &ctx,
        &loop_config,
        &stop_signal,
    )
    .await;

    if let Some(ref data) = outcome.data {
        if let Some(full_response) = data.get("full_response").and_then(|v| v.as_str()) {
            if !full_response.is_empty() {
                let transcript = format!(
                    "# Session Transcript\n\n**Prompt:** {}\n\n**Turns:** {}\n\n**Exit:** {}\n\n---\n\n{}",
                    prompt,
                    outcome.turn,
                    outcome.exit_reason,
                    full_response
                );
                match memory.archive_session(&transcript).await {
                    Ok(path) => tracing::info!("Session archived to {:?}", path),
                    Err(e) => tracing::warn!("Failed to archive session: {e}"),
                }
            }
        }
    }

    match outcome.exit_reason.as_str() {
        "exited" | "max_turns" | "end_turn" => {
            if let Some(ref data) = outcome.data {
                println!("{}", serde_json::to_string_pretty(data).unwrap_or_default());
            }
        }
        reason => {
            eprintln!("Agent loop finished: {reason} ({} turns)", outcome.turn);
        }
    }

    Ok(())
}

async fn run_reflect(
    goal: Option<String>,
    budget: f64,
    autonomous: bool,
    working_dir: &PathBuf,
) -> anyhow::Result<()> {
    use oz_reflect::ReflectRunner;

    let mut runner = ReflectRunner::new(working_dir.clone());

    if let Some(goal_text) = goal {
        let goal_module = oz_reflect::goal_mode::GoalModeModule::new(working_dir, budget);
        goal_module.start_goal(goal_text).await?;
        runner.add_module(goal_module);
        tracing::info!("Goal mode started with budget={budget}min");
    }

    if autonomous {
        runner.add_module(oz_reflect::autonomous::AutonomousModule::default());
        tracing::info!("Autonomous mode enabled");
    }

    // Auto-fetch module: periodically fetches URLs and saves to memory
    runner.add_module(oz_reflect::auto_fetch::AutoFetchModule::new(working_dir));
    tracing::info!("Auto-fetch module enabled");

    let triggers = runner.check_all().await;
    if triggers.is_empty() {
        println!("No reflect triggers. System idle.");
    } else {
        for (module, prompt) in &triggers {
            println!("[{module}] {prompt}");
        }
    }

    Ok(())
}

async fn run_plugin(action: &PluginAction) -> anyhow::Result<()> {
    match action {
        PluginAction::Load { path } => {
            let canonical = if path.is_absolute() {
                path.clone()
            } else {
                std::env::current_dir()
                    .unwrap_or_default()
                    .join(path)
            };
            tracing::info!("Loading WASM plugin from: {}", canonical.display());
            let plugin = oz_plugin::WasmPlugin::from_file(&canonical)
                .map_err(|e| anyhow::anyhow!("Failed to load plugin: {e}"))?;
            let def = plugin.to_definition();
            println!("Loaded plugin: {} ({})", def.function.name, def.function.description);
            println!("  Parameters schema: {}", serde_json::to_string_pretty(&def.function.parameters)?);
            Ok(())
        }
        PluginAction::List => {
            println!("Plugin list: use `ga plugin load <path>` to load a .wasm plugin");
            Ok(())
        }
    }
}
