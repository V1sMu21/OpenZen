pub mod autonomous;
pub mod goal_mode;
pub mod scheduler;
pub mod auto_fetch;

use std::path::PathBuf;
use std::time::Duration;

/// Shared reflect module trait.
#[async_trait::async_trait]
pub trait ReflectModule: Send + Sync {
    /// Check if the module has something to trigger.
    /// Returns `Some(prompt)` if a trigger condition is met.
    async fn check(&self) -> Option<String>;

    /// Human-readable name of this module.
    fn name(&self) -> &'static str;
}

/// Reflect runner — polls all modules and triggers actions.
pub struct ReflectRunner {
    modules: Vec<Box<dyn ReflectModule>>,
    base_dir: PathBuf,
    interval: Duration,
}

impl ReflectRunner {
    pub fn new(base_dir: PathBuf) -> Self {
        ReflectRunner {
            modules: Vec::new(),
            base_dir,
            interval: Duration::from_secs(60),
        }
    }

    pub fn with_interval(mut self, interval: Duration) -> Self {
        self.interval = interval;
        self
    }

    pub fn add_module<T: ReflectModule + 'static>(&mut self, module: T) {
        self.modules.push(Box::new(module));
    }

    pub fn base_dir(&self) -> &PathBuf {
        &self.base_dir
    }

    /// Run a single check cycle across all modules.
    pub async fn check_all(&self) -> Vec<(String, String)> {
        let mut triggers = Vec::new();
        for module in &self.modules {
            if let Some(prompt) = module.check().await {
                triggers.push((module.name().to_string(), prompt));
            }
        }
        triggers
    }

    /// Start the polling loop (runs forever).
    pub async fn run_forever(&self) -> ! {
        tracing::info!(
            "Reflect runner started with {} module(s), interval={:?}",
            self.modules.len(),
            self.interval
        );
        loop {
            for (name, prompt) in self.check_all().await {
                tracing::info!(module = %name, "Reflect trigger: {prompt}");
            }
            tokio::time::sleep(self.interval).await;
        }
    }
}
