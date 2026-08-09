use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use tokio::sync::mpsc;

use crate::phase4::types::TaskPriority;

type TaskFn = Box<dyn FnOnce() + Send + 'static>;

pub struct PriorityTaskScheduler {
    active_critical: Arc<AtomicBool>,
    critical_tx: mpsc::UnboundedSender<(TaskPriority, Option<TaskFn>)>,
}

impl PriorityTaskScheduler {
    pub fn new() -> Self {
        let (critical_tx, mut critical_rx) =
            mpsc::unbounded_channel::<(TaskPriority, Option<TaskFn>)>();
        let active_critical = Arc::new(AtomicBool::new(false));

        let ac = Arc::clone(&active_critical);

        std::thread::spawn(move || {
            let rt = tokio::runtime::Builder::new_current_thread()
                .enable_time()
                .build()
                .expect("PriorityTaskScheduler: failed to create tokio runtime");

            rt.block_on(async move {
                while let Some((priority, task)) = critical_rx.recv().await {
                    match priority {
                        TaskPriority::Critical => {
                            ac.store(true, Ordering::SeqCst);
                            if let Some(f) = task {
                                f();
                            }
                            ac.store(false, Ordering::SeqCst);
                        }
                        TaskPriority::Low => {
                            while ac.load(Ordering::SeqCst) {
                                tokio::time::sleep(tokio::time::Duration::from_millis(50)).await;
                            }
                            if let Some(f) = task {
                                f();
                            }
                        }
                    }
                }
            });
        });

        Self {
            active_critical,
            critical_tx,
        }
    }

    pub fn submit<F>(&self, priority: TaskPriority, task: F)
    where
        F: FnOnce() + Send + 'static,
    {
        let _ = self.critical_tx.send((priority, Some(Box::new(task))));
    }

    pub fn is_critical_active(&self) -> bool {
        self.active_critical.load(Ordering::SeqCst)
    }
}

impl Default for PriorityTaskScheduler {
    fn default() -> Self {
        Self::new()
    }
}

impl Clone for PriorityTaskScheduler {
    fn clone(&self) -> Self {
        Self {
            active_critical: Arc::clone(&self.active_critical),
            critical_tx: self.critical_tx.clone(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_new_scheduler_not_active() {
        let s = PriorityTaskScheduler::new();
        assert!(!s.is_critical_active());
    }

    #[tokio::test]
    async fn test_submit_critical() {
        let s = PriorityTaskScheduler::new();
        let flag = Arc::new(AtomicBool::new(false));
        let f = Arc::clone(&flag);

        s.submit(TaskPriority::Critical, move || {
            f.store(true, Ordering::SeqCst);
        });

        tokio::time::sleep(std::time::Duration::from_millis(200)).await;
        assert!(flag.load(Ordering::SeqCst));
    }

    #[tokio::test]
    async fn test_submit_low_waits_for_critical() {
        let s = PriorityTaskScheduler::new();
        let order = Arc::new(std::sync::Mutex::new(Vec::new()));
        let o1 = Arc::clone(&order);
        let o2 = Arc::clone(&order);

        s.submit(TaskPriority::Critical, move || {
            std::thread::sleep(std::time::Duration::from_millis(100));
            o1.lock().unwrap().push("critical");
        });

        tokio::time::sleep(std::time::Duration::from_millis(50)).await;

        s.submit(TaskPriority::Low, move || {
            o2.lock().unwrap().push("low");
        });

        tokio::time::sleep(std::time::Duration::from_millis(500)).await;
        let result = order.lock().unwrap().clone();
        assert_eq!(result, vec!["critical", "low"]);
    }
}
