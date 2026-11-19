//! Worker wall-budget enforcement shared by the desktop scheduler.
//!
//! Extracted from the scheduler so a scenario can prove the exact timeout
//! behaviour without booting Tauri.

use std::future::Future;
use std::time::Duration;

/// Run `fut` with a wall-clock budget. The error string is what the scheduler
/// records on the task/run, so it is part of the observable contract.
pub async fn run_with_timeout<F, T>(budget: Duration, fut: F) -> Result<T, String>
where
    F: Future<Output = T>,
{
    match tokio::time::timeout(budget, fut).await {
        Ok(value) => Ok(value),
        Err(_) => Err(format!(
            "worker wall budget exhausted ({}s)",
            budget.as_secs()
        )),
    }
}
