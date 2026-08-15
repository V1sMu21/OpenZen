//! ga-scheduler — Lightweight scheduled task runner for OpenZen.
//!
//! Powered by `tokio::time::interval`. No external cron dependencies.
//!
//! Built-in tasks:
//! - [`SessionCleanup`] — removes expired idle sessions
//! - [`SkillMcpScan`] — marks stale knowledge entries
//! - [`TrustDecay`] — decays inactive workspace trust entries

pub mod task;
pub mod tasks;

pub use task::{ScheduledTask, TaskContext, TaskError};
pub use tasks::{SessionCleanup, SkillMcpScan, TrustDecay};

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

pub struct Scheduler {
    tasks: Vec<Box<dyn ScheduledTask>>,
    shutdown: Arc<AtomicBool>,
}

impl Scheduler {
    pub fn new() -> Self {
        Scheduler {
            tasks: Vec::new(),
            shutdown: Arc::new(AtomicBool::new(false)),
        }
    }

    pub fn register(&mut self, task: Box<dyn ScheduledTask>) {
        self.tasks.push(task);
    }

    pub fn shutdown_signal(&self) -> Arc<AtomicBool> {
        self.shutdown.clone()
    }

    pub fn stop(&self) {
        self.shutdown.store(true, Ordering::Relaxed);
    }

    /// Start all registered tasks. Each runs in its own `tokio::spawn` with
    /// `tokio::time::interval`. Missed ticks are skipped (not burst).
    ///
    /// `ctx` must carry the real data paths (sessions file, trust store,
    /// skill/MCP dir) — the built-in tasks silently no-op with the default
    /// context, so 7x24 maintenance would never actually run.
    pub async fn run(self, ctx: TaskContext) {
        let shutdown = self.shutdown.clone();
        let mut handles = Vec::new();

        for task in self.tasks {
            let name = task.name().to_string();
            let interval = task.interval();
            let shutdown = shutdown.clone();
            let task_ctx = ctx.clone();

            tracing::info!("[scheduler] starting task `{name}` every {interval:?}");

            let handle = tokio::spawn(async move {
                let mut timer = tokio::time::interval(interval);
                timer.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);

                loop {
                    tokio::select! {
                        _ = timer.tick() => {
                            if shutdown.load(Ordering::Relaxed) { break; }
                            match task.execute(&task_ctx).await {
                                Ok(()) => tracing::debug!("[scheduler] `{name}` ok"),
                                Err(e) => tracing::warn!("[scheduler] `{name}` failed: {e}"),
                            }
                        }
                        _ = async {
                            while !shutdown.load(Ordering::Relaxed) {
                                tokio::time::sleep(Duration::from_millis(100)).await;
                            }
                        } => {
                            break;
                        }
                    }
                }
                tracing::info!("[scheduler] task `{name}` stopped");
            });
            handles.push(handle);
        }

        // Wait for all tasks to complete (only on shutdown)
        for h in handles {
            let _ = h.await;
        }
    }
}

impl Default for Scheduler {
    fn default() -> Self {
        Self::new()
    }
}
