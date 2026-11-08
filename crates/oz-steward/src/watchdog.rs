//! Dead-man / stale-ticket watchdog.
//!
//! The watchdog is a separate clock from the worker: if a ticket stops
//! heartbeating, it returns a `StopWorker` action and marks the ticket
//! stalled. The runtime owns the actual stop signal; this module never kills
//! a process itself.

use std::sync::atomic::{AtomicI64, Ordering};
use std::sync::Arc;

use serde::Serialize;

use crate::error::Result;
use crate::store::Store;

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum WatchdogAction {
    MarkedStalled {
        ticket_id: String,
        session_id: String,
    },
    StopWorker {
        ticket_id: String,
        session_id: String,
        reason: String,
    },
}

pub struct Watchdog {
    store: Arc<Store>,
    stale_ms: i64,
}

impl Watchdog {
    pub fn new(store: Arc<Store>, stale_ms: i64) -> Self {
        Self {
            store,
            stale_ms: stale_ms.max(1),
        }
    }

    /// Sweep once. Stale active tickets are marked stalled and produce both a
    /// MarkedStalled and a StopWorker action.
    pub fn tick(&self, now: i64) -> Result<Vec<WatchdogAction>> {
        let mut out = Vec::new();
        for ticket in self.store.sweep_stalled(now, self.stale_ms)? {
            out.push(WatchdogAction::MarkedStalled {
                ticket_id: ticket.id.clone(),
                session_id: ticket.session_id.clone(),
            });
            out.push(WatchdogAction::StopWorker {
                ticket_id: ticket.id,
                session_id: ticket.session_id,
                reason: format!("no heartbeat for more than {}s", self.stale_ms / 1000),
            });
        }
        Ok(out)
    }
}

/// A tiny dead-man switch for the steward loop itself. If the loop stops
/// calling `beat`, `expired` becomes true after `timeout_ms`.
pub struct Deadman {
    last_beat: AtomicI64,
    timeout_ms: i64,
}

impl Deadman {
    pub fn new(now: i64, timeout_ms: i64) -> Self {
        Self {
            last_beat: AtomicI64::new(now),
            timeout_ms: timeout_ms.max(1),
        }
    }

    pub fn beat(&self, now: i64) {
        self.last_beat.store(now, Ordering::Relaxed);
    }

    pub fn expired(&self, now: i64) -> bool {
        now.saturating_sub(self.last_beat.load(Ordering::Relaxed)) > self.timeout_ms
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use oz_core_types::supervision::{Phase, StepRecord, StepStatus, TicketStatus};

    fn store_with_step() -> Arc<Store> {
        let store = Arc::new(Store::open_memory().unwrap());
        store.ensure_ticket("t1", "sess", "title").unwrap();
        store
            .record_step(&StepRecord {
                id: "s1".into(),
                ticket_id: "t1".into(),
                session_id: "sess".into(),
                turn: 1,
                phase: Phase::Acting,
                at: 1,
                tool: None,
                status: StepStatus::Succeeded,
                artifacts: vec![],
                targets: vec![],
                plan_hash: None,
                git_sha: None,
                tokens_in: 0,
                tokens_out: 0,
            })
            .unwrap();
        store
    }

    fn make_old(store: &Store) {
        let conn = store.lock_for_test();
        conn.execute(
            "UPDATE ticket SET heartbeat_at=1, updated_at=1 WHERE id='t1'",
            [],
        )
        .unwrap();
    }

    #[test]
    fn stale_ticket_stops_worker_and_marks_stalled() {
        let store = store_with_step();
        make_old(&store);
        let actions = Watchdog::new(store.clone(), 90_000)
            .tick(1_000_000)
            .unwrap();
        assert!(actions
            .iter()
            .any(|a| matches!(a, WatchdogAction::StopWorker { .. })));
        assert_eq!(
            store.snapshot("t1").unwrap().unwrap().status,
            TicketStatus::Stalled
        );
    }

    #[test]
    fn fresh_ticket_is_untouched() {
        let store = store_with_step();
        let actions = Watchdog::new(store, 90_000).tick(1_000).unwrap();
        assert!(actions.is_empty());
    }

    #[test]
    fn deadman_expires_without_beat() {
        let d = Deadman::new(1_000, 30_000);
        assert!(!d.expired(20_000));
        d.beat(20_000);
        assert!(!d.expired(49_000));
        assert!(d.expired(50_001));
    }
}
