use std::collections::HashMap;
use std::sync::Arc;

use tokio::task::JoinHandle;

use crate::{PlatformAdapter, PlatformContext};

pub struct PlatformRegistry {
    adapters: HashMap<String, Arc<dyn PlatformAdapter>>,
    handles: Vec<JoinHandle<()>>,
}

impl PlatformRegistry {
    pub fn new() -> Self {
        PlatformRegistry {
            adapters: HashMap::new(),
            handles: Vec::new(),
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

    pub fn start_all(&mut self, ctx: PlatformContext) {
        for adapter in self.adapters.values().cloned() {
            let ctx_clone = ctx.clone();
            let adapter_clone = adapter.clone();
            let handle = tokio::spawn(async move {
                tracing::info!("[platform] starting adapter: {}", adapter_clone.name());
                if let Err(e) = adapter_clone.start(ctx_clone).await {
                    tracing::error!(
                        "[platform] adapter {} exited with error: {}",
                        adapter_clone.name(),
                        e
                    );
                }
                tracing::info!("[platform] adapter stopped: {}", adapter_clone.name());
            });
            self.handles.push(handle);
        }
    }

    pub async fn stop_all(&self) {
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
