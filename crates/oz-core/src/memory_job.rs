//! Memory job scheduler — async two-stage distillation queue.
//!
//! Sessions end by enqueueing a distillation job and returning immediately;
//! a background worker extracts knowledge and persists it. Jobs carry a lease
//! (crash-safe takeover), retry budget, and error tracking — mirroring Codex's
//! `jobs` / `stage1_outputs` memory pipeline.

use std::sync::Arc;
use std::time::{Duration, Instant};

use tokio::sync::Mutex;

/// Lifecycle of a memory distillation job.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MemoryJobStatus {
    Queued,
    Running,
    Done,
    Failed,
}

/// A queued session-to-memory distillation task.
#[derive(Debug, Clone)]
pub struct MemoryJob {
    pub session_id: String,
    pub transcript: String,
    pub status: MemoryJobStatus,
    pub lease_until: Option<Instant>,
    pub retry_remaining: u32,
    pub last_error: Option<String>,
}

impl MemoryJob {
    fn new(session_id: String, transcript: String, max_retries: u32) -> Self {
        MemoryJob {
            session_id,
            transcript,
            status: MemoryJobStatus::Queued,
            lease_until: None,
            retry_remaining: max_retries,
            last_error: None,
        }
    }
}

/// A background worker that distills a session transcript into memory.
#[async_trait::async_trait]
pub trait MemoryDistiller: Send + Sync {
    /// Extract and persist knowledge from a session transcript.
    /// Returns the number of items stored.
    async fn distill(&self, session_id: &str, transcript: &str) -> Result<usize, String>;
}

/// Scheduler owning the job queue and driving the background worker.
pub struct MemoryJobScheduler {
    queue: Mutex<Vec<MemoryJob>>,
    distiller: Arc<dyn MemoryDistiller>,
    max_retries: u32,
    lease_secs: u64,
}

impl MemoryJobScheduler {
    pub fn new(distiller: Arc<dyn MemoryDistiller>) -> Self {
        MemoryJobScheduler {
            queue: Mutex::new(Vec::new()),
            distiller,
            max_retries: 3,
            lease_secs: 300,
        }
    }

    /// Enqueue a session for distillation. Returns immediately.
    pub async fn submit(&self, session_id: String, transcript: String) {
        let mut queue = self.queue.lock().await;
        if queue.iter().any(|j| j.session_id == session_id && j.status != MemoryJobStatus::Done) {
            return; // already queued or running
        }
        queue.push(MemoryJob::new(session_id, transcript, self.max_retries));
    }

    /// Number of jobs not yet completed.
    pub async fn pending_count(&self) -> usize {
        let queue = self.queue.lock().await;
        queue.iter().filter(|j| j.status != MemoryJobStatus::Done).count()
    }

    /// Drive one queue pass: pick a queued job, run it under a lease,
    /// retry on failure with backoff. Returns true if any job was processed.
    pub async fn run_cycle(&self) -> bool {
        let mut queue = self.queue.lock().await;

        // Take the next eligible job: queued, a failed one past its retry wait,
        // or a running one whose lease expired (crashed worker takeover).
        let now = Instant::now();
        let idx = queue.iter().position(|j| match j.status {
            MemoryJobStatus::Queued => true,
            MemoryJobStatus::Failed => j
                .lease_until
                .map(|t| t <= now)
                .unwrap_or(false),
            MemoryJobStatus::Running => j
                .lease_until
                .map(|t| t <= now)
                .unwrap_or(false),
            _ => false,
        });

        let Some(idx) = idx else {
            return false;
        };

        let mut job = queue.remove(idx);
        job.status = MemoryJobStatus::Running;
        job.lease_until = Some(Instant::now() + Duration::from_secs(self.lease_secs));
        drop(queue); // release the lock while distilling

        let result = self.distiller.distill(&job.session_id, &job.transcript).await;

        let mut queue = self.queue.lock().await;
        match result {
            Ok(_) => {
                job.status = MemoryJobStatus::Done;
                job.last_error = None;
                tracing::info!("memory job '{}' distilled", job.session_id);
                return true;
            }
            Err(e) if job.retry_remaining > 0 => {
                job.retry_remaining -= 1;
                job.status = MemoryJobStatus::Failed;
                let backoff = 15u64 * (self.max_retries - job.retry_remaining) as u64;
                job.lease_until = Some(Instant::now() + Duration::from_secs(backoff));
                job.last_error = Some(e.clone());
                tracing::warn!("memory job '{}' failed ({} retries left): {e}", job.session_id, job.retry_remaining);
            }
            Err(e) => {
                job.status = MemoryJobStatus::Failed;
                job.last_error = Some(e.clone());
                tracing::error!("memory job '{}' exhausted retries: {e}", job.session_id);
                return true;
            }
        }
        queue.push(job);
        true
    }

    /// Run cycles until the queue drains (bounded by max iterations).
    pub async fn drain(&self) {
        for _ in 0..100 {
            if !self.run_cycle().await {
                break;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct FakeDistiller;

    #[async_trait::async_trait]
    impl MemoryDistiller for FakeDistiller {
        async fn distill(&self, _session_id: &str, transcript: &str) -> Result<usize, String> {
            if transcript.contains("fail") {
                Err("boom".into())
            } else {
                Ok(transcript.split_whitespace().count())
            }
        }
    }

    #[tokio::test]
    async fn test_submit_and_drain() {
        let sched = MemoryJobScheduler::new(Arc::new(FakeDistiller));
        sched.submit("s1".into(), "hello world".into()).await;
        assert_eq!(sched.pending_count().await, 1);
        sched.drain().await;
        assert_eq!(sched.pending_count().await, 0);
    }

    #[tokio::test]
    async fn test_retry_then_succeed() {
        let sched = MemoryJobScheduler::new(Arc::new(FakeDistiller));
        sched.submit("s2".into(), "fail me".into()).await;
        sched.run_cycle().await;
        assert_eq!(sched.pending_count().await, 1); // retry queued
        sched.drain().await;
        assert_eq!(sched.pending_count().await, 1); // still failing, retries exhausted
    }

    #[tokio::test]
    async fn test_no_duplicate_submit() {
        let sched = MemoryJobScheduler::new(Arc::new(FakeDistiller));
        sched.submit("s3".into(), "a".into()).await;
        sched.submit("s3".into(), "b".into()).await;
        assert_eq!(sched.pending_count().await, 1);
    }

    #[tokio::test]
    async fn test_lease_expired_running_is_taken_over() {
        let sched = MemoryJobScheduler::new(Arc::new(FakeDistiller));
        sched.submit("s4".into(), "hello".into()).await;
        // Simulate a crashed worker: mark the job Running with an expired lease.
        {
            let mut queue = sched.queue.lock().await;
            let job = queue.first_mut().unwrap();
            job.status = MemoryJobStatus::Running;
            job.lease_until = Some(Instant::now() - Duration::from_secs(1));
        }
        assert!(sched.run_cycle().await); // taken over and distilled
        assert_eq!(sched.pending_count().await, 0);
    }

    #[tokio::test]
    async fn test_exhausted_retries_removed() {
        let sched = MemoryJobScheduler::new(Arc::new(FakeDistiller));
        sched.submit("s5".into(), "fail me".into()).await;
        // Initial run: fails with 3 retries left.
        assert!(sched.run_cycle().await);
        // Simulate time passing past each backoff, then retry until exhausted.
        for _ in 0..3 {
            {
                let mut queue = sched.queue.lock().await;
                if let Some(job) = queue.first_mut() {
                    job.lease_until = Some(Instant::now() - Duration::from_secs(1));
                }
            }
            sched.run_cycle().await;
        }
        assert_eq!(sched.pending_count().await, 0); // removed, not retried forever
    }
}
