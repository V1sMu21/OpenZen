use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

use tokio::task::JoinHandle;

use crate::{PlatformAdapter, PlatformContext};

pub struct PlatformRegistry {
    adapters: HashMap<String, Arc<dyn PlatformAdapter>>,
    handles: Vec<JoinHandle<()>>,
    /// Set by `stop_all`; supervisor loops observe it and stop restarting.
    shutdown: Arc<AtomicBool>,
}

impl Default for PlatformRegistry {
    fn default() -> Self {
        Self::new()
    }
}

impl PlatformRegistry {
    pub fn new() -> Self {
        PlatformRegistry {
            adapters: HashMap::new(),
            handles: Vec::new(),
            shutdown: Arc::new(AtomicBool::new(false)),
        }
    }

    pub fn register(&mut self, adapter: Arc<dyn PlatformAdapter>) {
        self.adapters.insert(adapter.id().to_string(), adapter);
    }

    pub fn is_empty(&self) -> bool {
        self.adapters.is_empty()
    }

    pub fn adapter_ids(&self) -> Vec<String> {
        self.adapters.keys().cloned().collect()
    }

    /// Start every registered adapter under a supervisor: an adapter that
    /// fails or panics is restarted with exponential backoff (5s→60s,
    /// reset to 5s after 5 minutes of healthy uptime) instead of taking
    /// the channel offline until the whole app restarts. 7x24 safety net.
    /// Each adapter also gets a health poll: a channel that reports
    /// unhealthy (without returning) is visible in the logs instead of
    /// silently wedging.
    pub fn start_all(&mut self, ctx: PlatformContext) {
        for adapter in self.adapters.values() {
            let ctx_clone = ctx.clone();
            let adapter_clone = adapter.clone();
            let shutdown = self.shutdown.clone();
            let health_adapter = adapter.clone();
            let health_shutdown = self.shutdown.clone();
            let health_handle = tokio::spawn(async move {
                let mut was_healthy = true;
                loop {
                    tokio::time::sleep(Duration::from_secs(60)).await;
                    if health_shutdown.load(Ordering::Relaxed) {
                        break;
                    }
                    // Bound a wedged health() probe so the poll itself
                    // cannot hang.
                    let probe = health_adapter.health();
                    let healthy = match tokio::time::timeout(Duration::from_secs(10), probe).await {
                        Ok(h) => h.connected,
                        Err(_) => false,
                    };
                    if healthy != was_healthy {
                        tracing::warn!(
                            "[platform] adapter {} health transitioned to {}",
                            health_adapter.name(),
                            if healthy { "healthy" } else { "UNHEALTHY" }
                        );
                        was_healthy = healthy;
                    }
                }
            });
            self.handles.push(health_handle);
            let handle = tokio::spawn(async move {
                let mut backoff_secs: u64 = 5;
                loop {
                    if shutdown.load(Ordering::Relaxed) {
                        break;
                    }
                    tracing::info!("[platform] starting adapter: {}", adapter_clone.name());
                    let started_at = tokio::time::Instant::now();
                    // Run start() in a child task so a panic surfaces as a
                    // JoinError instead of killing the supervisor itself.
                    let a = adapter_clone.clone();
                    let c = ctx_clone.clone();
                    let outcome = match tokio::spawn(async move { a.start(c).await }).await {
                        Ok(Ok(())) => "stopped cleanly".to_string(),
                        Ok(Err(e)) => format!("exited with error: {e}"),
                        Err(e) => format!("panicked: {e}"),
                    };
                    if shutdown.load(Ordering::Relaxed) {
                        break;
                    }
                    tracing::warn!(
                        "[platform] adapter {} {} — restarting in {}s",
                        adapter_clone.name(),
                        outcome,
                        backoff_secs
                    );
                    tokio::time::sleep(Duration::from_secs(backoff_secs)).await;
                    backoff_secs = if started_at.elapsed() >= Duration::from_secs(300) {
                        5
                    } else {
                        (backoff_secs * 2).min(60)
                    };
                }
                tracing::info!("[platform] supervisor for {} exited", adapter_clone.name());
            });
            self.handles.push(handle);
        }
    }

    pub async fn stop_all(&self) {
        self.shutdown.store(true, Ordering::Relaxed);
        for adapter in self.adapters.values() {
            if let Err(e) = adapter.stop().await {
                tracing::warn!(
                    "[platform] error stopping adapter {}: {}",
                    adapter.name(),
                    e
                );
            }
        }
    }
}
