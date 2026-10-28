//! Background job registry — the single seam for "work that outlives a turn".
//!
//! Producers (background `code_run`, background `spawn_agent`) register work
//! here and get a stable `<kind>-N` id. The owning agent session can read the
//! output (consuming cursor), wait with a timeout, list its jobs, and request
//! cancellation. The registry owns identity/lifecycle; producers own execution
//! and resources.
//!
//! Design mirrors DeepSeek Harness `ctx.jobs` (packages/jobs/jobs) with the
//! parts OpenZen actually needs: owner-fenced access, first-wins settlement,
//! settlement only after the producer released its resources, and a single
//! process-wide listener for UI/event fan-out.
//!
//! See `docs/subagents-and-background-jobs.md` §6.

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use tokio::sync::oneshot;

pub type JobId = String;

/// Producer kind — also the id prefix (`bash-1`, `agent-2`).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum JobKind {
    Bash,
    Subagent,
    Tool,
}

impl JobKind {
    pub fn prefix(self) -> &'static str {
        match self {
            JobKind::Bash => "bash",
            JobKind::Subagent => "agent",
            JobKind::Tool => "tool",
        }
    }
    pub fn as_str(self) -> &'static str {
        self.prefix()
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum JobStatus {
    Running,
    Stopping,
    Completed,
    Killed,
    Failed,
}

impl JobStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            JobStatus::Running => "running",
            JobStatus::Stopping => "stopping",
            JobStatus::Completed => "completed",
            JobStatus::Killed => "killed",
            JobStatus::Failed => "failed",
        }
    }
    pub fn is_terminal(self) -> bool {
        matches!(
            self,
            JobStatus::Completed | JobStatus::Killed | JobStatus::Failed
        )
    }
}

/// Read-only projection handed to tools/listeners. Never live registry state.
#[derive(Clone, Debug)]
pub struct JobSnapshot {
    pub id: JobId,
    pub kind: JobKind,
    pub label: String,
    pub status: JobStatus,
    pub detail: Option<String>,
    pub owner_session: String,
    pub started_at: i64,
    pub finished_at: Option<i64>,
}

impl JobSnapshot {
    /// Stable JSON shape for Tauri events and tool results.
    pub fn to_json(&self) -> serde_json::Value {
        serde_json::json!({
            "id": self.id,
            "kind": self.kind.as_str(),
            "label": self.label,
            "status": self.status.as_str(),
            "detail": self.detail,
            "owner_session": self.owner_session,
            "started_at": self.started_at,
            "finished_at": self.finished_at,
        })
    }
}

/// Terminal result supplied by a producer through `JobHooks::done`.
#[derive(Clone, Debug)]
pub struct JobOutcome {
    pub status: JobStatus,
    pub detail: Option<String>,
}

/// Hooks through which the runtime observes and controls producer work.
pub struct JobHooks {
    /// Idempotent cancellation request. Must return quickly; settlement comes
    /// through `done` once the producer actually stopped.
    pub cancel: Box<dyn Fn(Option<String>) + Send + Sync>,
    /// Consuming cursor: returns output produced since the previous call.
    /// `None` marks a final-output-only job.
    pub read_output: Option<Box<dyn FnMut() -> String + Send>>,
    /// Resolves after the producer released its resources. Must not reject.
    pub done: oneshot::Receiver<JobOutcome>,
}

/// Producer declaration.
pub struct JobStart {
    pub kind: JobKind,
    pub label: String,
    pub owner_session: String,
    /// Called once, after preflight. A throw leaves nothing registered.
    pub run: Box<dyn FnOnce(JobCtx) -> Result<JobHooks, String> + Send>,
}

/// Context handed to `run` so producers can tag their logs.
pub struct JobCtx {
    pub id: JobId,
}

#[derive(Clone, Debug)]
pub enum JobEvent {
    Started(JobSnapshot),
    Finished(JobSnapshot),
}

pub type JobListener = Arc<dyn Fn(JobEvent) + Send + Sync>;

/// Cancellation handle stored by the registry (idempotent producer callback).
pub type JobCancelFn = Arc<dyn Fn(Option<String>) + Send + Sync>;
/// Consuming output cursor shared between the registry and the consumer.
pub type JobOutputCursor = Arc<Mutex<Box<dyn FnMut() -> String + Send>>>;

struct Entry {
    snapshot: JobSnapshot,
    cancel: Option<JobCancelFn>,
    read_output: Option<JobOutputCursor>,
    /// Output drained from the cursor at settlement, served once to a reader
    /// that arrives *after* the job finished (the common wait-then-read path).
    pending_final: Option<String>,
    /// Set when a consumer read/was notified about the terminal state, so a
    /// later completion notice can be suppressed (DSH's `reported`).
    reported: bool,
}

pub struct JobRegistry {
    entries: Mutex<HashMap<JobId, Entry>>,
    seq_bash: AtomicU64,
    seq_agent: AtomicU64,
    seq_tool: AtomicU64,
    listener: Mutex<Option<JobListener>>,
}

fn now_secs() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

impl JobRegistry {
    pub fn new() -> Self {
        Self {
            entries: Mutex::new(HashMap::new()),
            seq_bash: AtomicU64::new(0),
            seq_agent: AtomicU64::new(0),
            seq_tool: AtomicU64::new(0),
            listener: Mutex::new(None),
        }
    }

    pub fn set_listener(&self, listener: JobListener) {
        *self.listener.lock().unwrap() = Some(listener);
    }

    fn next_id(&self, kind: JobKind) -> JobId {
        let seq = match kind {
            JobKind::Bash => &self.seq_bash,
            JobKind::Subagent => &self.seq_agent,
            JobKind::Tool => &self.seq_tool,
        };
        format!(
            "{}-{}",
            kind.prefix(),
            seq.fetch_add(1, Ordering::Relaxed) + 1
        )
    }

    fn emit(&self, event: JobEvent) {
        let listener = self.listener.lock().unwrap().clone();
        if let Some(l) = listener {
            // A panicking UI listener must never break job lifecycle.
            let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| l(event)));
        }
    }

    /// Register work. Preflight (id allocation) happens before `run`; a `run`
    /// error leaves no record behind.
    pub fn start(self: &Arc<Self>, spec: JobStart) -> Result<JobId, String> {
        let id = self.next_id(spec.kind);
        let snapshot = JobSnapshot {
            id: id.clone(),
            kind: spec.kind,
            label: spec.label,
            status: JobStatus::Running,
            detail: None,
            owner_session: spec.owner_session,
            started_at: now_secs(),
            finished_at: None,
        };
        let hooks = (spec.run)(JobCtx { id: id.clone() })?;
        let cancel = Arc::new(hooks.cancel);
        let read_output = hooks.read_output.map(|f| Arc::new(Mutex::new(f)));
        let done = hooks.done;

        {
            let mut entries = self.entries.lock().unwrap();
            entries.insert(
                id.clone(),
                Entry {
                    snapshot: snapshot.clone(),
                    cancel: Some(cancel),
                    read_output,
                    pending_final: None,
                    reported: false,
                },
            );
        }
        self.emit(JobEvent::Started(snapshot));

        // Settlement task: waits for the producer to release resources, then
        // commits the terminal record and announces it exactly once.
        let registry = Arc::clone(self);
        let settle_id = id.clone();
        tokio::spawn(async move {
            let outcome = match done.await {
                Ok(o) => o,
                Err(_) => JobOutcome {
                    status: JobStatus::Failed,
                    detail: Some("producer dropped without reporting".into()),
                },
            };
            let finished = {
                let mut entries = registry.entries.lock().unwrap();
                let Some(entry) = entries.get_mut(&settle_id) else {
                    return;
                };
                // Cancellation requested while running settles as `killed`
                // even if the producer reports `completed` (a killed process
                // usually exits with a signal, not success).
                let status = if entry.snapshot.status == JobStatus::Stopping
                    && outcome.status != JobStatus::Failed
                {
                    JobStatus::Killed
                } else {
                    outcome.status
                };
                entry.snapshot.status = status;
                entry.snapshot.detail = outcome.detail;
                entry.snapshot.finished_at = Some(now_secs());
                entry.cancel = None;
                // Drain whatever the producer still holds into a one-shot
                // final buffer. Dropping the cursor here used to make a
                // settle-then-read return empty — losing exactly the tail of
                // a failing build.
                let tail = match entry.read_output.take() {
                    Some(cursor) => cursor.lock().map(|mut f| f()).unwrap_or_default(),
                    None => String::new(),
                };
                if !tail.is_empty() {
                    entry.pending_final = Some(tail);
                }
                entry.snapshot.clone()
            };
            registry.emit(JobEvent::Finished(finished));
        });

        Ok(id)
    }

    pub fn list(&self, owner: &str) -> Vec<JobSnapshot> {
        let entries = self.entries.lock().unwrap();
        let mut out: Vec<_> = entries
            .values()
            .filter(|e| e.snapshot.owner_session == owner)
            .map(|e| e.snapshot.clone())
            .collect();
        out.sort_by_key(|s| s.started_at);
        out
    }

    pub fn get(&self, id: &str) -> Option<JobSnapshot> {
        self.entries
            .lock()
            .unwrap()
            .get(id)
            .map(|e| e.snapshot.clone())
    }

    /// Consume output produced since the previous read. Owner-fenced: a
    /// foreign session gets `NotFound`, never an authorization hint.
    pub fn read(&self, id: &str, owner: &str) -> Result<(String, JobSnapshot), String> {
        let mut entries = self.entries.lock().unwrap();
        let entry = entries
            .get_mut(id)
            .ok_or_else(|| format!("job {id} not found"))?;
        if entry.snapshot.owner_session != owner {
            return Err(format!("job {id} not found"));
        }
        let text = match &entry.read_output {
            Some(r) => {
                let mut f = r.lock().unwrap();
                f()
            }
            // Settlement drained the cursor into this one-shot buffer; a
            // reader arriving after the job finished still gets the tail.
            None => entry.pending_final.take().unwrap_or_default(),
        };
        Ok((text, entry.snapshot.clone()))
    }

    /// Request cancellation. Returns immediately with `stopping`; use
    /// `wait` to observe settlement.
    pub fn cancel(
        &self,
        id: &str,
        owner: &str,
        reason: Option<String>,
    ) -> Result<JobSnapshot, String> {
        let cancel = {
            let mut entries = self.entries.lock().unwrap();
            let entry = entries
                .get_mut(id)
                .ok_or_else(|| format!("job {id} not found"))?;
            if entry.snapshot.owner_session != owner {
                return Err(format!("job {id} not found"));
            }
            if entry.snapshot.status.is_terminal() {
                return Ok(entry.snapshot.clone());
            }
            entry.snapshot.status = JobStatus::Stopping;
            entry.cancel.clone()
        };
        if let Some(c) = cancel {
            c(reason);
        }
        Ok(self.get(id).unwrap())
    }

    /// Poll until terminal or timeout. A timeout leaves the job running.
    pub async fn wait(
        &self,
        id: &str,
        owner: &str,
        timeout: Duration,
    ) -> Result<JobSnapshot, String> {
        let deadline = tokio::time::Instant::now() + timeout;
        loop {
            let snap = {
                let entries = self.entries.lock().unwrap();
                let entry = entries
                    .get(id)
                    .ok_or_else(|| format!("job {id} not found"))?;
                if entry.snapshot.owner_session != owner {
                    return Err(format!("job {id} not found"));
                }
                entry.snapshot.clone()
            };
            if snap.status.is_terminal() {
                return Ok(snap);
            }
            if tokio::time::Instant::now() >= deadline {
                return Ok(snap);
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    }

    pub fn is_reported(&self, id: &str) -> bool {
        self.entries
            .lock()
            .unwrap()
            .get(id)
            .map(|e| e.reported)
            .unwrap_or(true)
    }

    /// Cancel every job owned by `owner` (Stop / app quit / child disposal).
    /// Returns how many were still running.
    pub fn cancel_owner(&self, owner: &str, reason: Option<String>) -> usize {
        let cancels: Vec<JobCancelFn> = {
            let mut entries = self.entries.lock().unwrap();
            let mut v = Vec::new();
            for entry in entries.values_mut() {
                if entry.snapshot.owner_session != owner || entry.snapshot.status.is_terminal() {
                    continue;
                }
                entry.snapshot.status = JobStatus::Stopping;
                if let Some(c) = entry.cancel.clone() {
                    v.push(c);
                }
            }
            v
        };
        let n = cancels.len();
        for c in cancels {
            c(reason.clone());
        }
        n
    }

    /// Cancel every non-terminal job (application quit). Returns the count.
    pub fn cancel_all(&self, reason: Option<String>) -> usize {
        let cancels: Vec<JobCancelFn> = {
            let mut entries = self.entries.lock().unwrap();
            let mut v = Vec::new();
            for entry in entries.values_mut() {
                if entry.snapshot.status.is_terminal() {
                    continue;
                }
                entry.snapshot.status = JobStatus::Stopping;
                if let Some(c) = entry.cancel.clone() {
                    v.push(c);
                }
            }
            v
        };
        let n = cancels.len();
        for c in cancels {
            c(reason.clone());
        }
        n
    }

    /// Mark the terminal state as already reported so the completion notice
    /// is suppressed. Returns true when this call won the race.
    pub fn mark_reported(&self, id: &str) -> bool {
        let mut entries = self.entries.lock().unwrap();
        if let Some(e) = entries.get_mut(id) {
            if e.snapshot.status.is_terminal() && !e.reported {
                e.reported = true;
                return true;
            }
        }
        false
    }

    /// Drop finished records older than `max_age` (housekeeping).
    pub fn prune(&self, max_age: Duration) {
        let cutoff = now_secs() - max_age.as_secs() as i64;
        let mut entries = self.entries.lock().unwrap();
        entries.retain(|_, e| match e.snapshot.finished_at {
            Some(t) => t >= cutoff,
            None => true,
        });
    }
}

impl Default for JobRegistry {
    fn default() -> Self {
        Self::new()
    }
}

static GLOBAL: OnceLock<Arc<JobRegistry>> = OnceLock::new();

/// Process-wide registry. The app installs its own instance at startup so it
/// can attach an event listener; tests get a fresh lazy instance.
pub fn set_global(registry: Arc<JobRegistry>) {
    let _ = GLOBAL.set(registry);
}

pub fn try_global() -> Option<Arc<JobRegistry>> {
    GLOBAL.get().cloned()
}

pub fn global() -> Arc<JobRegistry> {
    GLOBAL.get_or_init(|| Arc::new(JobRegistry::new())).clone()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn start_job(
        reg: &Arc<JobRegistry>,
        owner: &str,
        label: &str,
        cancel_flag: Arc<std::sync::atomic::AtomicBool>,
        output: Arc<Mutex<String>>,
    ) -> (JobId, oneshot::Sender<JobOutcome>) {
        let (tx, rx) = oneshot::channel();
        let out = output.clone();
        let id = reg
            .start(JobStart {
                kind: JobKind::Bash,
                label: label.into(),
                owner_session: owner.into(),
                run: Box::new(move |_ctx| {
                    Ok(JobHooks {
                        cancel: Box::new(move |_reason| {
                            cancel_flag.store(true, Ordering::SeqCst);
                        }),
                        read_output: Some(Box::new(move || {
                            let mut s = out.lock().unwrap();
                            std::mem::take(&mut *s)
                        })),
                        done: rx,
                    })
                }),
            })
            .unwrap();
        (id, tx)
    }

    #[tokio::test]
    async fn ids_are_prefixed_and_monotonic() {
        let reg = Arc::new(JobRegistry::new());
        let flag = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let out = Arc::new(Mutex::new(String::new()));
        let (a, _txa) = start_job(&reg, "s1", "one", flag.clone(), out.clone());
        let (b, _txb) = start_job(&reg, "s1", "two", flag.clone(), out.clone());
        assert_eq!(a, "bash-1");
        assert_eq!(b, "bash-2");
    }

    #[tokio::test]
    async fn owner_fence_blocks_foreign_reads() {
        let reg = Arc::new(JobRegistry::new());
        let flag = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let out = Arc::new(Mutex::new("hello".into()));
        let (id, _tx) = start_job(&reg, "s1", "one", flag, out);
        assert!(reg.read(&id, "s2").is_err());
        assert!(reg.cancel(&id, "s2", None).is_err());
        let (text, snap) = reg.read(&id, "s1").unwrap();
        assert_eq!(text, "hello");
        assert_eq!(snap.status, JobStatus::Running);
    }

    #[tokio::test]
    async fn output_read_is_consuming() {
        let reg = Arc::new(JobRegistry::new());
        let flag = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let out = Arc::new(Mutex::new("abc".into()));
        let (id, _tx) = start_job(&reg, "s1", "one", flag, out.clone());
        assert_eq!(reg.read(&id, "s1").unwrap().0, "abc");
        assert_eq!(reg.read(&id, "s1").unwrap().0, "");
        *out.lock().unwrap() = "def".into();
        assert_eq!(reg.read(&id, "s1").unwrap().0, "def");
    }

    #[tokio::test]
    async fn cancel_sets_stopping_then_settles_killed() {
        let reg = Arc::new(JobRegistry::new());
        let flag = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let out = Arc::new(Mutex::new("hello".to_string()));
        let (id, tx) = start_job(&reg, "s1", "one", flag.clone(), out);
        let snap = reg.cancel(&id, "s1", Some("user".into())).unwrap();
        assert_eq!(snap.status, JobStatus::Stopping);
        assert!(flag.load(Ordering::SeqCst));
        tx.send(JobOutcome {
            status: JobStatus::Completed,
            detail: Some("exit 0".into()),
        })
        .unwrap();
        let done = reg.wait(&id, "s1", Duration::from_secs(2)).await.unwrap();
        assert_eq!(done.status, JobStatus::Killed);
        assert_eq!(done.detail.as_deref(), Some("exit 0"));
        assert!(done.finished_at.is_some());
    }

    #[tokio::test]
    async fn cancel_owner_stops_every_job_of_that_session() {
        let reg = Arc::new(JobRegistry::new());
        let f1 = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let f2 = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let (a, _ta) = start_job(
            &reg,
            "s1",
            "one",
            f1.clone(),
            Arc::new(Mutex::new(String::new())),
        );
        let (b, _tb) = start_job(
            &reg,
            "s1",
            "two",
            f2.clone(),
            Arc::new(Mutex::new(String::new())),
        );
        let (c, _tc) = start_job(
            &reg,
            "s2",
            "three",
            Arc::new(std::sync::atomic::AtomicBool::new(false)),
            Arc::new(Mutex::new(String::new())),
        );
        assert_eq!(reg.cancel_owner("s1", None), 2);
        assert!(f1.load(Ordering::SeqCst) && f2.load(Ordering::SeqCst));
        assert_eq!(reg.get(&a).unwrap().status, JobStatus::Stopping);
        assert_eq!(reg.get(&b).unwrap().status, JobStatus::Stopping);
        assert_eq!(reg.get(&c).unwrap().status, JobStatus::Running);
    }

    #[tokio::test]
    async fn wait_times_out_without_killing() {
        let reg = Arc::new(JobRegistry::new());
        let flag = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let out = Arc::new(Mutex::new(String::new()));
        let (id, _tx) = start_job(&reg, "s1", "one", flag, out);
        let snap = reg
            .wait(&id, "s1", Duration::from_millis(60))
            .await
            .unwrap();
        assert_eq!(snap.status, JobStatus::Running);
    }

    #[tokio::test]
    async fn completion_delivers_once_and_list_is_owner_scoped() {
        let reg = Arc::new(JobRegistry::new());
        let flag = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let out = Arc::new(Mutex::new("hello".to_string()));
        let (id, tx) = start_job(&reg, "s1", "one", flag.clone(), out);
        let (_id2, _tx2) = start_job(&reg, "s2", "two", flag, Arc::new(Mutex::new(String::new())));
        assert_eq!(reg.list("s1").len(), 1);
        assert_eq!(reg.list("s2").len(), 1);
        tx.send(JobOutcome {
            status: JobStatus::Completed,
            detail: None,
        })
        .unwrap();
        let done = reg.wait(&id, "s1", Duration::from_secs(2)).await.unwrap();
        assert_eq!(done.status, JobStatus::Completed);
        // Regression: a settle-then-read must still return the tail the
        // producer had not been asked for yet (the failing build's last lines).
        assert_eq!(reg.read(&id, "s1").unwrap().0, "hello");
        assert_eq!(reg.read(&id, "s1").unwrap().0, "");
        assert!(reg.mark_reported(&id));
        assert!(!reg.mark_reported(&id));
    }
}
