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
    /// Wakes the per-task `select!` immediately on `stop()`. The previous
    /// implementation had every task poll `shutdown` with a 100 ms sleep
    /// inside its select branch: three registered tasks meant ~30 wakeups a
    /// second, forever, even while the app was idle (round5 P1-14).
    notify: Arc<tokio::sync::Notify>,
}

impl Scheduler {
    pub fn new() -> Self {
        Scheduler {
            tasks: Vec::new(),
            shutdown: Arc::new(AtomicBool::new(false)),
            notify: Arc::new(tokio::sync::Notify::new()),
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
        // Wake current waiters, and leave a permit for a task that has not
        // reached its `notified()` yet (otherwise it would only notice at
        // its next tick — up to an hour later).
        self.notify.notify_waiters();
        self.notify.notify_one();
    }

    /// Start all registered tasks. Each runs in its own `tokio::spawn` with
    /// `tokio::time::interval`. Missed ticks are skipped (not burst).
    ///
    /// `ctx` must carry the real data paths (sessions file, trust store,
    /// skill/MCP dir) — the built-in tasks silently no-op with the default
    /// context, so 7x24 maintenance would never actually run.
    pub async fn run(self, ctx: TaskContext) {
        let shutdown = self.shutdown.clone();
        let notify = self.notify.clone();
        let mut handles = Vec::new();

        for task in self.tasks {
            let name = task.name().to_string();
            let interval = task.interval();
            let shutdown = shutdown.clone();
            let notify = notify.clone();
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
                        _ = notify.notified() => {
                            break;
                        }
                        // Fallback: `Scheduler::stop()` is not the only way the
                        // shutdown flag is set — `graceful_shutdown` stores
                        // into the same `AtomicBool` through
                        // `shutdown_signal()`, and that path cannot notify.
                        // Re-check the flag every 5s so a stop is observed
                        // within seconds instead of at the next tick (which
                        // is up to 6h for SkillMcpScan), without going back
                        // to the old 100ms-per-task spin (review finding C-F5).
                        _ = tokio::time::sleep(Duration::from_secs(5)) => {
                            if shutdown.load(Ordering::Relaxed) {
                                break;
                            }
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
