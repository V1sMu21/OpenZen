use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Mutex;
use std::time::{SystemTime, UNIX_EPOCH};

use crate::core::MemoryContent;

const DEFAULT_DAILY_TOKEN_LIMIT: usize = 256_000;
const DEFAULT_ANNUAL_STORAGE_LIMIT: usize = 10 * 1024 * 1024;

#[derive(Debug, Clone)]
pub struct BudgetConfig {
    pub daily_token_limit: usize,
    pub annual_storage_limit: usize,
}

impl Default for BudgetConfig {
    fn default() -> Self {
        Self {
            daily_token_limit: DEFAULT_DAILY_TOKEN_LIMIT,
            annual_storage_limit: DEFAULT_ANNUAL_STORAGE_LIMIT,
        }
    }
}

fn days_since_epoch() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
        / 86_400
}

pub struct BudgetController {
    config: BudgetConfig,
    tokens_used_today: AtomicUsize,
    current_storage_bytes: AtomicUsize,
    last_reset_day: Mutex<u64>,
}

impl BudgetController {
    pub fn new(config: BudgetConfig) -> Self {
        Self {
            config,
            tokens_used_today: AtomicUsize::new(0),
            current_storage_bytes: AtomicUsize::new(0),
            last_reset_day: Mutex::new(days_since_epoch()),
        }
    }

    pub fn config(&self) -> &BudgetConfig {
        &self.config
    }

    pub fn tokens_used_today(&self) -> usize {
        self.maybe_reset();
        self.tokens_used_today.load(Ordering::Relaxed)
    }

    pub fn storage_bytes(&self) -> usize {
        self.current_storage_bytes.load(Ordering::Relaxed)
    }

    pub fn storage_mb(&self) -> f64 {
        self.current_storage_bytes.load(Ordering::Relaxed) as f64 / (1024.0 * 1024.0)
    }

    /// Check if a memory of estimated token count fits within all budgets.
    /// Returns Ok(()) or an error describing which budget was exceeded.
    pub fn check_budget(
        &self,
        estimated_tokens: usize,
        estimated_bytes: usize,
    ) -> Result<(), BudgetError> {
        self.maybe_reset();

        let tokens_after = self.tokens_used_today.load(Ordering::Relaxed) + estimated_tokens;
        if tokens_after > self.config.daily_token_limit {
            return Err(BudgetError::DailyTokenExceeded {
                limit: self.config.daily_token_limit,
                attempted: tokens_after,
            });
        }

        let bytes_after = self.current_storage_bytes.load(Ordering::Relaxed) + estimated_bytes;
        if bytes_after > self.config.annual_storage_limit {
            return Err(BudgetError::AnnualStorageExceeded {
                limit: self.config.annual_storage_limit,
                attempted: bytes_after,
            });
        }

        Ok(())
    }

    /// Record usage after a successful write.
    pub fn record_usage(&self, estimated_tokens: usize, estimated_bytes: usize) {
        self.maybe_reset();
        self.tokens_used_today
            .fetch_add(estimated_tokens, Ordering::Relaxed);
        self.current_storage_bytes
            .fetch_add(estimated_bytes, Ordering::Relaxed);
    }

    pub fn record_removal(&self, estimated_bytes: usize) {
        self.current_storage_bytes
            .fetch_sub(estimated_bytes, Ordering::Relaxed);
    }

    /// Estimate token count from a memory's text content.
    pub fn estimate_tokens(content: &MemoryContent) -> usize {
        let text = match content {
            MemoryContent::Fact(f) => {
                format!("{} {} {}", f.subject, f.predicate, f.object)
            }
            MemoryContent::Summary(s) => s.clone(),
            MemoryContent::Fingerprint(_) => String::new(),
            MemoryContent::Embedding(_) => String::new(),
        };
        // Rough heuristic: 1 token ≈ 4 chars for English
        text.len() / 4 + 1
    }

    /// Estimate storage bytes for a memory.
    pub fn estimate_bytes(text: &str) -> usize {
        text.len() + 128 // text + overhead (metadata, etc)
    }

    fn maybe_reset(&self) {
        let today = days_since_epoch();
        let mut last = self.last_reset_day.lock().unwrap();
        if today != *last {
            self.tokens_used_today.store(0, Ordering::Relaxed);
            *last = today;
        }
    }

    /// Force-reset the daily token counter. Used by stress tests that simulate
    /// many days in real-time seconds, where the day-based auto-reset won't fire.
    pub fn force_reset_daily(&self) {
        self.tokens_used_today.store(0, Ordering::Relaxed);
        *self.last_reset_day.lock().unwrap() = days_since_epoch();
    }

    pub fn stats(&self) -> BudgetStats {
        self.maybe_reset();
        BudgetStats {
            tokens_used_today: self.tokens_used_today.load(Ordering::Relaxed),
            daily_token_limit: self.config.daily_token_limit,
            storage_bytes: self.current_storage_bytes.load(Ordering::Relaxed),
            annual_storage_limit: self.config.annual_storage_limit,
            daily_utilization_pct: self.tokens_used_today.load(Ordering::Relaxed) as f64
                / self.config.daily_token_limit as f64
                * 100.0,
            storage_utilization_pct: self.current_storage_bytes.load(Ordering::Relaxed) as f64
                / self.config.annual_storage_limit as f64
                * 100.0,
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum BudgetError {
    DailyTokenExceeded { limit: usize, attempted: usize },
    AnnualStorageExceeded { limit: usize, attempted: usize },
}

impl std::fmt::Display for BudgetError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            BudgetError::DailyTokenExceeded { limit, attempted } => {
                write!(f, "daily token budget exceeded: {}/{}", attempted, limit)
            }
            BudgetError::AnnualStorageExceeded { limit, attempted } => {
                write!(
                    f,
                    "annual storage budget exceeded: {}/{} bytes",
                    attempted, limit
                )
            }
        }
    }
}

#[derive(Debug, Clone)]
pub struct BudgetStats {
    pub tokens_used_today: usize,
    pub daily_token_limit: usize,
    pub storage_bytes: usize,
    pub annual_storage_limit: usize,
    pub daily_utilization_pct: f64,
    pub storage_utilization_pct: f64,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_new_controller_within_budget() {
        let c = BudgetController::new(BudgetConfig::default());
        assert!(c.check_budget(100, 1000).is_ok());
    }

    #[test]
    fn test_daily_token_exceeded() {
        let c = BudgetController::new(BudgetConfig {
            daily_token_limit: 1000,
            ..Default::default()
        });
        assert!(c.check_budget(1001, 1000).is_err());
    }

    #[test]
    fn test_storage_exceeded() {
        let c = BudgetController::new(BudgetConfig {
            annual_storage_limit: 5000,
            ..Default::default()
        });
        assert!(c.check_budget(100, 6000).is_err());
    }

    #[test]
    fn test_record_usage_tracks_correctly() {
        let c = BudgetController::new(BudgetConfig {
            daily_token_limit: 10000,
            annual_storage_limit: 100000,
        });
        c.record_usage(500, 2000);
        assert_eq!(c.tokens_used_today(), 500);
        assert_eq!(c.storage_bytes(), 2000);
    }

    #[test]
    fn test_record_removal() {
        let c = BudgetController::new(BudgetConfig::default());
        c.record_usage(100, 5000);
        c.record_removal(2000);
        assert_eq!(c.storage_bytes(), 3000);
    }

    #[test]
    fn test_estimate_tokens_fact() {
        let content = MemoryContent::Fact(crate::core::Fact::new("hello", "world", "test"));
        let tokens = BudgetController::estimate_tokens(&content);
        assert!(tokens > 0);
    }

    #[test]
    fn test_estimate_bytes() {
        let bytes = BudgetController::estimate_bytes("hello world");
        assert!(bytes > 128); // "hello world".len() + 128
    }

    #[test]
    fn test_stats_report() {
        let c = BudgetController::new(BudgetConfig {
            daily_token_limit: 1000,
            annual_storage_limit: 10000,
        });
        c.record_usage(250, 2500);
        let s = c.stats();
        assert_eq!(s.daily_utilization_pct, 25.0);
        assert_eq!(s.storage_utilization_pct, 25.0);
    }

    #[test]
    fn test_budget_error_display() {
        let e = BudgetError::DailyTokenExceeded {
            limit: 1000,
            attempted: 1500,
        };
        let msg = e.to_string();
        assert!(msg.contains("1000"));
        assert!(msg.contains("1500"));
    }
}
