use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use anyhow::Context;
use tokio::process::{Child, Command};
use tokio::signal;
use tokio::sync::watch;
use tokio::sync::Mutex;
use tokio::time::sleep;

use crate::upgrade;

/// Commands the daemon can receive from external processes.
#[derive(Debug, Clone)]
#[allow(dead_code)]
pub enum DaemonCommand {
    /// Perform a self-upgrade: build, health check, swap, restart.
    Upgrade {
        /// If set, use this pre-built binary instead of compiling from source.
        binary_path: Option<PathBuf>,
    },
    /// Graceful shutdown.
    Shutdown,
    /// Get daemon status.
    Status,
}

/// Configuration for the daemon process.
pub struct DaemonConfig {
    pub port: u16,
    pub config: PathBuf,
    pub assets: PathBuf,
    pub dir: PathBuf,
    pub frontend_dir: Option<PathBuf>,
    pub max_restarts: u32,
    pub health_check_interval_secs: u64,
    pub pid_file: Option<PathBuf>,
    /// Shared command channel receiver.
    pub cmd_rx: Arc<Mutex<watch::Receiver<Option<DaemonCommand>>>>,
    /// Channel to send command responses.
    #[allow(dead_code)]
    pub cmd_tx: watch::Sender<Option<DaemonCommand>>,
}

impl Default for DaemonConfig {
    fn default() -> Self {
        let (cmd_tx, cmd_rx) = watch::channel(None);
        DaemonConfig {
            port: 18567,
            config: PathBuf::from("config/mykey.toml"),
            assets: PathBuf::from("assets"),
            dir: PathBuf::from("."),
            frontend_dir: None,
            max_restarts: 10,
            health_check_interval_secs: 10,
            pid_file: None,
            cmd_rx: Arc::new(Mutex::new(cmd_rx)),
            cmd_tx,
        }
    }
}

impl DaemonConfig {
    #[allow(clippy::type_complexity)]
    pub fn new_command_channel() -> (
        watch::Sender<Option<DaemonCommand>>,
        Arc<Mutex<watch::Receiver<Option<DaemonCommand>>>>,
    ) {
        let (cmd_tx, cmd_rx) = watch::channel(None);
        (cmd_tx, Arc::new(Mutex::new(cmd_rx)))
    }
}

/// Run the daemon: spawn, monitor, and handle upgrade/restart cycles.
pub async fn run_daemon(config: DaemonConfig) -> anyhow::Result<()> {
    let mut restart_count = 0u32;
    let max_restarts = config.max_restarts;

    // Start background scheduler
    let mut scheduler = oz_scheduler::Scheduler::new();
    scheduler.register(Box::new(oz_scheduler::SessionCleanup::default()));
    scheduler.register(Box::new(oz_scheduler::TrustDecay::default()));
    if config.dir.join(oz_skill_mcp::SKILL_MCP_DIR).exists() {
        scheduler.register(Box::new(oz_scheduler::SkillMcpScan::default()));
    }
    let task_ctx = oz_scheduler::TaskContext {
        working_dir: Some(config.dir.to_string_lossy().to_string()),
        skill_mcp_dir: Some(
            config
                .dir
                .join(oz_skill_mcp::SKILL_MCP_DIR)
                .to_string_lossy()
                .to_string(),
        ),
        trust_path: Some(
            config
                .dir
                .join("openzen")
                .join("trust.json")
                .to_string_lossy()
                .to_string(),
        ),
        // Daemon mode has no in-memory AppState; the disk-side cleanup
        // path (now case-corrected) applies.
        session_pruner: None,
    };
    tokio::spawn(scheduler.run(task_ctx));

    // Write PID file if requested
    if let Some(ref pid_path) = config.pid_file {
        if let Some(parent) = pid_path.parent() {
            tokio::fs::create_dir_all(parent).await.ok();
        }
        let pid = std::process::id();
        tokio::fs::write(pid_path, pid.to_string())
            .await
            .context("Failed to write PID file")?;
        tracing::info!("PID {} written to {}", pid, pid_path.display());

        let pid_path_cleanup = pid_path.clone();
        let cmd_tx_for_ctrlc = config.cmd_tx.clone();
        tokio::spawn(async move {
            // Remove the pid file, then signal the supervisor loop to shut
            // down gracefully. A raw process::exit(0) here raced the graceful
            // stop path and could orphan the serve child process.
            signal::ctrl_c().await.ok();
            tokio::fs::remove_file(&pid_path_cleanup).await.ok();
            let _ = cmd_tx_for_ctrlc.send(Some(DaemonCommand::Shutdown));
        });
    }

    // Keep a reference to cmd_rx Arc for the loop
    let cmd_rx: Arc<Mutex<watch::Receiver<Option<DaemonCommand>>>> = config.cmd_rx.clone();

    loop {
        tracing::info!(
            "Starting serve process (port={})... restart_count={}/{}",
            config.port,
            restart_count,
            max_restarts
        );

        let mut child = spawn_serve(&config).await?;

        // Wait for child to be ready (health check)
        let health_ok = wait_for_health(config.port, Duration::from_secs(30)).await;
        if !health_ok {
            tracing::warn!("Health check failed, killing unresponsive child");
            child.kill().await?;
            child.wait().await?;
        } else {
            tracing::info!("Serve process is healthy on port {}", config.port);
        }

        let monitor_result = monitor_child_with_commands(
            &mut child,
            config.port,
            config.health_check_interval_secs,
            cmd_rx.clone(),
        )
        .await;

        match monitor_result {
            MonitorOutcome::Exited(status) => {
                tracing::warn!("Serve process exited with status: {status}");
                restart_count += 1;
            }
            MonitorOutcome::Killed => {
                tracing::info!("Serve process killed by signal");
                restart_count += 1;
            }
            MonitorOutcome::Unhealthy => {
                tracing::warn!("Health check failed, restarting...");
                child.kill().await?;
                child.wait().await?;
                restart_count += 1;
            }
            MonitorOutcome::ShutdownRequested => {
                tracing::info!("Shutdown requested, stopping daemon");
                child.kill().await?;
                child.wait().await?;
                return Ok(());
            }
            MonitorOutcome::UpgradePerformed => {
                tracing::info!("Upgrade performed, will restart with new binary");
                child.kill().await?;
                child.wait().await?;
                // Reset restart count on successful upgrade
                restart_count = 0;
                // Fall through to restart the loop which will use the new binary
                // (since atomic_swap already replaced it in-place)
                continue;
            }
            MonitorOutcome::UpgradeFailed => {
                tracing::error!("Upgrade failed, continuing with current binary");
                // Don't increment restart count for failed upgrades
                continue;
            }
        }

        if restart_count > max_restarts {
            anyhow::bail!("Max restarts ({max_restarts}) exceeded, giving up");
        }

        tracing::info!("Restarting in 2 seconds...");
        sleep(Duration::from_secs(2)).await;
    }
}

#[derive(Debug)]
enum MonitorOutcome {
    Exited(i32),
    Killed,
    Unhealthy,
    ShutdownRequested,
    UpgradePerformed,
    UpgradeFailed,
}

/// Spawn the `openzen serve` process.
async fn spawn_serve(config: &DaemonConfig) -> anyhow::Result<Child> {
    let exe = std::env::current_exe().context("Failed to get current executable path")?;

    let mut cmd = Command::new(&exe);
    cmd.arg("serve")
        .arg("--port")
        .arg(config.port.to_string())
        .arg("--config")
        .arg(config.config.to_string_lossy().as_ref())
        .arg("--assets")
        .arg(config.assets.to_string_lossy().as_ref())
        .arg("--dir")
        .arg(config.dir.to_string_lossy().as_ref());

    if let Some(ref frontend) = config.frontend_dir {
        cmd.arg("--frontend-dir")
            .arg(frontend.to_string_lossy().as_ref());
    }

    cmd.stdout(std::process::Stdio::inherit())
        .stderr(std::process::Stdio::inherit());

    let child = cmd.spawn().context("Failed to spawn serve process")?;
    Ok(child)
}

/// Wait for the health endpoint to respond successfully.
async fn wait_for_health(port: u16, timeout: Duration) -> bool {
    let url = format!("http://127.0.0.1:{}/api/health", port);
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(5))
        .build()
        .ok()
        .unwrap_or_default();

    let start = std::time::Instant::now();
    while start.elapsed() < timeout {
        match client.get(&url).send().await {
            Ok(resp) if resp.status().is_success() => return true,
            Ok(resp) if resp.status().as_u16() == 404 => {
                return true;
            }
            _ => {}
        }
        sleep(Duration::from_millis(500)).await;
    }
    false
}

/// Monitor a child process, checking health periodically and handling signals + commands.
async fn monitor_child_with_commands(
    child: &mut Child,
    port: u16,
    check_interval_secs: u64,
    cmd_rx: Arc<Mutex<watch::Receiver<Option<DaemonCommand>>>>,
) -> MonitorOutcome {
    let health_url = format!("http://127.0.0.1:{}/api/health", port);
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(5))
        .build()
        .ok()
        .unwrap_or_default();

    let mut shutdown = false;

    // Spawn a task to catch Ctrl+C
    let shutdown_flag = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let flag = shutdown_flag.clone();
    tokio::spawn(async move {
        signal::ctrl_c().await.ok();
        flag.store(true, std::sync::atomic::Ordering::Relaxed);
    });

    loop {
        tokio::select! {
            // Check if child exited
            status = child.wait() => {
                return match status {
                    Ok(s) => MonitorOutcome::Exited(s.code().unwrap_or(-1)),
                    Err(_) => MonitorOutcome::Killed,
                };
            }
            // Periodic health check + command polling
            _ = sleep(Duration::from_secs(check_interval_secs)) => {
                if shutdown_flag.load(std::sync::atomic::Ordering::Relaxed) {
                    shutdown = true;
                }

                if shutdown {
                    return MonitorOutcome::ShutdownRequested;
                }

                // Poll for daemon commands
                let cmd = {
                    let mut guard = cmd_rx.lock().await;
                    // Check if there's a new value without blocking
                    if guard.has_changed().unwrap_or(false) {
                        guard.borrow_and_update().clone()
                    } else {
                        None
                    }
                };

                match cmd {
                    Some(DaemonCommand::Upgrade { .. }) => {
                        tracing::info!("Daemon received upgrade command");

                        let upgrade_cfg = upgrade::UpgradeConfig {
                            current_exe: std::env::current_exe()
                                .unwrap_or_else(|_| PathBuf::from("ga")),
                            project_dir: PathBuf::from("."),
                            health_check_port: port,
                            ..Default::default()
                        };

                        let result = upgrade::perform_upgrade(&upgrade_cfg).await;
                        if result.success {
                            tracing::info!("Upgrade successful, restarting server");
                            return MonitorOutcome::UpgradePerformed;
                        } else {
                            tracing::error!("Upgrade failed: {:?}", result.error);
                            return MonitorOutcome::UpgradeFailed;
                        }
                    }
                    Some(DaemonCommand::Shutdown) => {
                        tracing::info!("Daemon received shutdown command");
                        return MonitorOutcome::ShutdownRequested;
                    }
                    Some(DaemonCommand::Status) => {
                        tracing::info!("Daemon status: running, child alive");
                    }
                    None => {}
                }

                // Health check
                match client.get(&health_url).send().await {
                    Ok(resp) if resp.status().is_success() => {}
                    Ok(resp) if resp.status().as_u16() == 404 => {}
                    _ => {
                        tracing::warn!("Health check failed for port {}", port);
                        return MonitorOutcome::Unhealthy;
                    }
                }
            }
        }
    }
}

/// Send a command to a running daemon via its watch channel.
#[allow(dead_code)]
pub async fn send_command(
    cmd_tx: &watch::Sender<Option<DaemonCommand>>,
    cmd: DaemonCommand,
) -> anyhow::Result<()> {
    cmd_tx
        .send(Some(cmd))
        .map_err(|_| anyhow::anyhow!("Failed to send command to daemon (receiver dropped)"))
}
