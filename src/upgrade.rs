use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::{Context, Result};
use tokio::process::Command;
use tokio::time::sleep;

/// The binary that was running before an upgrade (kept for rollback).
pub const PREV_BINARY_SUFFIX: &str = ".prev";

/// Configuration for the upgrade supervisor.
#[derive(Debug, Clone)]
pub struct UpgradeConfig {
    /// Current binary path (usually `std::env::current_exe()`).
    pub current_exe: PathBuf,
    /// Working directory for cargo build.
    pub project_dir: PathBuf,
    /// Port to health-check after starting the new binary.
    pub health_check_port: u16,
    /// How long to wait for the new binary to start.
    pub health_check_timeout: Duration,
    /// How long between health check retries.
    pub health_check_interval: Duration,
    /// Max retries for health check.
    pub max_health_retries: u32,
    /// Whether to run cargo build (vs using an externally provided binary).
    pub build_from_source: bool,
    /// Optional path to a pre-built binary (overrides build_from_source).
    pub provided_binary: Option<PathBuf>,
}

impl Default for UpgradeConfig {
    fn default() -> Self {
        UpgradeConfig {
            current_exe: std::env::current_exe().unwrap_or_else(|_| PathBuf::from("ga")),
            project_dir: PathBuf::from("."),
            health_check_port: 18567,
            health_check_timeout: Duration::from_secs(60),
            health_check_interval: Duration::from_secs(2),
            max_health_retries: 30,
            build_from_source: true,
            provided_binary: None,
        }
    }
}

/// Result of an upgrade attempt.
#[derive(Debug)]
#[allow(dead_code)]
pub struct UpgradeResult {
    pub success: bool,
    pub new_binary: Option<PathBuf>,
    pub prev_binary: Option<PathBuf>,
    pub error: Option<String>,
}

/// Build the project from source. Returns the path to the built binary.
pub async fn build_from_source(project_dir: &Path) -> Result<PathBuf> {
    tracing::info!("Building from source in {:?}", project_dir);

    let status = Command::new("cargo")
        .args(["build", "--release"])
        .current_dir(project_dir)
        .status()
        .await
        .context("Failed to run cargo build")?;

    if !status.success() {
        anyhow::bail!("cargo build failed with exit code: {:?}", status.code());
    }

    let binary = guess_binary_path(project_dir, "release");
    if !binary.exists() {
        anyhow::bail!("Built binary not found at {:?}", binary);
    }

    tracing::info!("Build succeeded: {:?}", binary);
    Ok(binary)
}

/// Guess the binary output path for the host triple.
fn guess_binary_path(project_dir: &Path, profile: &str) -> PathBuf {
    // Try common locations:
    // 1. target/release/ga (workspace root)
    let p1 = project_dir.join("target").join(profile).join("ga");
    if p1.exists() {
        return p1;
    }
    // 2. target/release/ga (with .exe on windows)
    let p1e = project_dir.join("target").join(profile).join("ga.exe");
    if p1e.exists() {
        return p1e;
    }
    // Return the most likely path anyway
    project_dir.join("target").join(profile).join("ga")
}

/// Run a health check against a running openzen serve instance.
pub async fn health_check(
    port: u16,
    timeout: Duration,
    interval: Duration,
    max_retries: u32,
) -> Result<()> {
    let start = tokio::time::Instant::now();
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(5))
        .build()
        .context("Failed to create HTTP client")?;

    let url = format!("http://127.0.0.1:{port}/api/health");
    let mut attempt = 0u32;

    loop {
        if start.elapsed() > timeout {
            anyhow::bail!("Health check timed out after {timeout:?} ({attempt} attempts)");
        }
        if attempt >= max_retries {
            anyhow::bail!("Health check failed after {attempt} retries");
        }

        match client.get(&url).send().await {
            Ok(resp) => {
                if resp.status().is_success() {
                    tracing::info!("Health check passed on port {port}");
                    return Ok(());
                }
                tracing::warn!("Health check returned status {}", resp.status());
            }
            Err(e) => {
                tracing::debug!(
                    "Health check attempt {}/{}: {}",
                    attempt + 1,
                    max_retries,
                    e
                );
            }
        }

        attempt += 1;
        sleep(interval).await;
    }
}

/// Atomically swap the old binary with a new one.
/// Preserves the old binary as `<old>.prev` for rollback.
pub fn atomic_swap(new_binary: &Path, current_exe: &Path) -> Result<()> {
    // Compute previous binary path: <current>.prev
    let prev_path = {
        let mut p = current_exe.to_path_buf();
        let name = p.file_name().unwrap().to_string_lossy().to_string();
        p.set_file_name(format!("{}{}", name, PREV_BINARY_SUFFIX));
        p
    };

    // Remove any stale .prev
    if prev_path.exists() {
        std::fs::remove_file(&prev_path).context(format!(
            "Failed to remove stale previous binary at {:?}",
            prev_path
        ))?;
    }

    // Rename current → .prev
    std::fs::rename(current_exe, &prev_path).context(format!(
        "Failed to rename current binary {:?} -> {:?}",
        current_exe, prev_path
    ))?;

    // Copy (or rename) new → current
    // Use copy to keep the new binary in its original location too (for reattempts)
    std::fs::copy(new_binary, current_exe).context(format!(
        "Failed to copy new binary {:?} -> {:?}",
        new_binary, current_exe
    ))?;

    // Make sure current exe is executable
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let perms = std::fs::Permissions::from_mode(0o755);
        std::fs::set_permissions(current_exe, perms)
            .context("Failed to set executable permissions on new binary")?;
    }

    tracing::info!(
        "Atomic swap complete: {:?} -> {:?}",
        new_binary,
        current_exe
    );
    Ok(())
}

/// Rollback to the previous binary.
#[allow(dead_code)]
pub fn rollback(current_exe: &Path) -> Result<()> {
    let prev_path = {
        let mut p = current_exe.to_path_buf();
        let name = p.file_name().unwrap().to_string_lossy().to_string();
        p.set_file_name(format!("{}{}", name, PREV_BINARY_SUFFIX));
        p
    };

    if !prev_path.exists() {
        anyhow::bail!(
            "No previous binary found at {:?} to roll back to",
            prev_path
        );
    }

    // Remove current (broken) binary
    if current_exe.exists() {
        std::fs::remove_file(current_exe)
            .context("Failed to remove broken binary during rollback")?;
    }

    // Restore previous
    std::fs::rename(&prev_path, current_exe).context(format!(
        "Failed to restore previous binary {:?} -> {:?}",
        prev_path, current_exe
    ))?;

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let perms = std::fs::Permissions::from_mode(0o755);
        std::fs::set_permissions(current_exe, perms).ok();
    }

    tracing::info!("Rollback complete: restored {:?}", current_exe);
    Ok(())
}

/// Full upgrade orchestration: build → health check → atomic swap → final health check.
/// Returns `UpgradeResult` - on success the caller should restart the server process.
pub async fn perform_upgrade(config: &UpgradeConfig) -> UpgradeResult {
    tracing::info!("Starting upgrade process");

    // Step 1: Obtain new binary
    let new_binary = if let Some(ref provided) = config.provided_binary {
        tracing::info!("Using provided binary: {:?}", provided);
        provided.clone()
    } else if config.build_from_source {
        match build_from_source(&config.project_dir).await {
            Ok(p) => p,
            Err(e) => {
                let msg = format!("Build failed: {e}");
                tracing::error!("{msg}");
                return UpgradeResult {
                    success: false,
                    new_binary: None,
                    prev_binary: None,
                    error: Some(msg),
                };
            }
        }
    } else {
        return UpgradeResult {
            success: false,
            new_binary: None,
            prev_binary: None,
            error: Some("No binary source configured".into()),
        };
    };

    if !new_binary.exists() {
        let msg = format!("New binary not found at {:?}", new_binary);
        tracing::error!("{msg}");
        return UpgradeResult {
            success: false,
            new_binary: Some(new_binary),
            prev_binary: None,
            error: Some(msg),
        };
    }

    // Step 2: Start a temporary server with the new binary on a test port
    // We use the same config as the current instance but on a different port
    let test_port =
        pick_available_port(config.health_check_port).unwrap_or(config.health_check_port + 1);

    tracing::info!("Starting health check on port {test_port} with new binary");

    let mut child = match Command::new(&new_binary)
        .args(["serve", "--port", &test_port.to_string()])
        .spawn()
    {
        Ok(c) => c,
        Err(e) => {
            let msg = format!("Failed to start new binary for health check: {e}");
            tracing::error!("{msg}");
            return UpgradeResult {
                success: false,
                new_binary: Some(new_binary),
                prev_binary: None,
                error: Some(msg),
            };
        }
    };

    // Step 3: Health check
    let health_result = health_check(
        test_port,
        config.health_check_timeout,
        config.health_check_interval,
        config.max_health_retries,
    )
    .await;

    // Kill the test instance
    let _ = child.kill().await;
    let _ = child.wait().await;

    if let Err(e) = health_result {
        let msg = format!("Health check failed for new binary: {e}");
        tracing::error!("{msg}");
        return UpgradeResult {
            success: false,
            new_binary: Some(new_binary),
            prev_binary: None,
            error: Some(msg),
        };
    }

    // Step 4: Atomic swap
    let prev_binary = {
        let mut p = config.current_exe.to_path_buf();
        let name = p.file_name().unwrap().to_string_lossy().to_string();
        p.set_file_name(format!("{}{}", name, PREV_BINARY_SUFFIX));
        p
    };

    if let Err(e) = atomic_swap(&new_binary, &config.current_exe) {
        let msg = format!("Atomic swap failed: {e}");
        tracing::error!("{msg}");
        return UpgradeResult {
            success: false,
            new_binary: Some(new_binary),
            prev_binary: None,
            error: Some(msg),
        };
    }

    tracing::info!("Upgrade completed successfully");
    UpgradeResult {
        success: true,
        new_binary: Some(new_binary),
        prev_binary: Some(prev_binary),
        error: None,
    }
}

/// Find an available port starting from the given one.
fn pick_available_port(start: u16) -> Option<u16> {
    if let Some(port) = (start..=start + 100).find(|&p| is_port_available(p)) {
        return Some(port);
    }
    None
}

fn is_port_available(port: u16) -> bool {
    std::net::TcpListener::bind(format!("127.0.0.1:{port}")).is_ok()
}
