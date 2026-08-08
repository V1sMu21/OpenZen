use std::time::Instant;

use async_trait::async_trait;

use crate::ReflectModule;

/// Autonomous reflect module — detects user idle time and triggers self-directed work.
pub struct AutonomousModule {
    last_interaction: Instant,
    idle_threshold_secs: u64,
}

impl AutonomousModule {
    pub fn new(idle_threshold_secs: u64, _interval_secs: u64) -> Self {
        AutonomousModule {
            last_interaction: Instant::now(),
            idle_threshold_secs,
        }
    }

    /// Record user interaction to reset the idle timer.
    pub fn record_interaction(&mut self) {
        self.last_interaction = Instant::now();
    }
}

impl Default for AutonomousModule {
    fn default() -> Self {
        AutonomousModule::new(1800, 300) // 30 min idle, check every 5 min
    }
}

#[async_trait]
impl ReflectModule for AutonomousModule {
    fn name(&self) -> &'static str {
        "autonomous"
    }

    async fn check(&self) -> Option<String> {
        let elapsed = self.last_interaction.elapsed().as_secs();
        if elapsed >= self.idle_threshold_secs {
            Some(format!(
                "[AUTO] User has been away for {} min. Checking for pending tasks...",
                elapsed / 60
            ))
        } else {
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[tokio::test]
    async fn test_autonomous_not_idle() {
        let module = AutonomousModule::new(3600, 60);
        assert!(module.check().await.is_none());
    }

    #[tokio::test]
    async fn test_autonomous_idle() {
        let module = AutonomousModule::new(0, 60);
        assert!(module.check().await.is_some());
    }

    #[test]
    fn test_record_interaction() {
        let mut module = AutonomousModule::new(1, 60);
        // Sleep briefly to let timer advance
        std::thread::sleep(Duration::from_millis(10));
        module.record_interaction();
        // After recording interaction, should not be idle
        assert!(module.last_interaction.elapsed().as_millis() < 100);
    }
}
