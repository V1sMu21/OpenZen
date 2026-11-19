//! SQLite persistence for the butler task graph.
//!
//! Same conventions as `goal/store.rs`: WAL, one connection behind a mutex,
//! JSON columns for variable shapes, timestamps in milliseconds.

use std::path::Path;
use std::sync::{Mutex, MutexGuard};

use rusqlite::{params, Connection, OptionalExtension};
use uuid::Uuid;

use oz_core_types::supervision::now_ms;

use crate::error::{Result, StewardError};
use crate::goal::spec::{GateResult, GoalGate};

use super::spec::{Approval, ApprovalStatus, Run, Task, TaskEvent, TaskStatus};

pub struct ButlerStore {
    conn: Mutex<Connection>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TaskCounts {
    pub total: usize,
    pub verified: usize,
    pub unverified: usize,
    pub failed: usize,
    pub open: usize,
}

impl ButlerStore {
    pub fn open(path: impl AsRef<Path>) -> Result<Self> {
        let conn = Connection::open(path)?;
        conn.busy_timeout(std::time::Duration::from_secs(5))?;
        Self::init(conn)
    }

    pub fn open_memory() -> Result<Self> {
        Self::init(Connection::open_in_memory()?)
    }

    fn init(conn: Connection) -> Result<Self> {
        conn.pragma_update(None, "journal_mode", "WAL")?;
        conn.execute_batch(
            r#"
            CREATE TABLE IF NOT EXISTS task (
                id             TEXT PRIMARY KEY,
                project_id     TEXT NOT NULL,
                project_name   TEXT,
                butler_session TEXT NOT NULL,
                goal           TEXT NOT NULL,
                detail         TEXT NOT NULL DEFAULT '',
                deps_json      TEXT NOT NULL DEFAULT '[]',
                require_verified_deps INTEGER NOT NULL DEFAULT 0,
                accept_json    TEXT NOT NULL DEFAULT '[]',
                status         TEXT NOT NULL,
                owner_session  TEXT,
                attempt        INTEGER NOT NULL DEFAULT 0,
                max_attempts   INTEGER NOT NULL DEFAULT 3,
                budget_json    TEXT NOT NULL DEFAULT '{}',
                model          TEXT,
                reversibility  TEXT NOT NULL DEFAULT 'workspace_write',
                evidence_json  TEXT NOT NULL DEFAULT '[]',
                assumptions_json TEXT NOT NULL DEFAULT '[]',
                skipped_json   TEXT NOT NULL DEFAULT '[]',
                created_at     INTEGER NOT NULL,
                updated_at     INTEGER NOT NULL
            );
            CREATE INDEX IF NOT EXISTS task_status_idx ON task(status);
            CREATE INDEX IF NOT EXISTS task_project_idx ON task(project_id);

            CREATE TABLE IF NOT EXISTS task_event (
                id TEXT PRIMARY KEY,
                task_id TEXT NOT NULL,
                at INTEGER NOT NULL,
                kind TEXT NOT NULL,
                detail TEXT NOT NULL DEFAULT ''
            );
            CREATE INDEX IF NOT EXISTS task_event_idx ON task_event(task_id, at);

            CREATE TABLE IF NOT EXISTS run (
                run_id TEXT PRIMARY KEY,
                task_id TEXT NOT NULL,
                project_id TEXT NOT NULL,
                child_id TEXT NOT NULL,
                model TEXT NOT NULL DEFAULT '',
                started_at INTEGER NOT NULL,
                finished_at INTEGER,
                status TEXT NOT NULL DEFAULT 'running',
                summary TEXT NOT NULL DEFAULT ''
            );
            CREATE INDEX IF NOT EXISTS run_project_idx ON run(project_id, started_at DESC);
            CREATE TABLE IF NOT EXISTS digest (
                project_id TEXT PRIMARY KEY,
                sent_at INTEGER,
                last_error TEXT NOT NULL DEFAULT ''
            );
            CREATE TABLE IF NOT EXISTS approval (
                id TEXT PRIMARY KEY,
                task_id TEXT NOT NULL,
                action TEXT NOT NULL,
                impact TEXT NOT NULL DEFAULT '',
                reversibility TEXT NOT NULL DEFAULT 'unknown',
                status TEXT NOT NULL DEFAULT 'pending',
                created_at INTEGER NOT NULL,
                decided_at INTEGER
            );
            CREATE INDEX IF NOT EXISTS approval_status_idx ON approval(status);
            "#,
        )?;
        // Additive migration for databases created before project binding.
        let _ = conn.execute("ALTER TABLE task ADD COLUMN project_name TEXT", []);
        let _ = conn.execute(
            "ALTER TABLE task ADD COLUMN require_verified_deps INTEGER NOT NULL DEFAULT 0",
            [],
        );
        Ok(Self {
            conn: Mutex::new(conn),
        })
    }

    #[doc(hidden)]
    pub fn lock_for_test(&self) -> MutexGuard<'_, Connection> {
        self.conn.lock().unwrap_or_else(|p| p.into_inner())
    }

    fn lock(&self) -> Result<MutexGuard<'_, Connection>> {
        self.conn
            .lock()
            .map_err(|_| StewardError::Invalid("butler db lock poisoned".into()))
    }

    pub fn create_task(&self, task: &Task) -> Result<()> {
        task.validate()?;
        let conn = self.lock()?;
        conn.execute(
            "INSERT INTO task(id,project_id,project_name,butler_session,goal,detail,deps_json,
                              require_verified_deps,accept_json,status,owner_session,attempt,max_attempts,
                              budget_json,model,reversibility,evidence_json,assumptions_json,skipped_json,
                              created_at,updated_at)
             VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,?15,?16,?17,?18,?19,?20,?21)",
            params![
                task.id,
                task.project_id,
                task.project_name,
                task.butler_session,
                task.goal,
                task.detail,
                serde_json::to_string(&task.deps)?,
                task.require_verified_deps,
                serde_json::to_string(&task.accept)?,
                task.status.as_str(),
                task.owner_session,
                task.attempt,
                task.max_attempts,
                serde_json::to_string(&task.budget)?,
                task.model,
                task.reversibility.as_str(),
                serde_json::to_string(&task.evidence)?,
                serde_json::to_string(&task.assumptions)?,
                serde_json::to_string(&task.skipped)?,
                task.created_at,
                task.updated_at,
            ],
        )?;
        Ok(())
    }

    pub fn get(&self, id: &str) -> Result<Option<Task>> {
        let conn = self.lock()?;
        let row = conn
            .query_row(
                "SELECT id,project_id,project_name,butler_session,goal,detail,deps_json,require_verified_deps,
                        accept_json,status,owner_session,attempt,max_attempts,budget_json,model,
                        reversibility,evidence_json,assumptions_json,skipped_json,created_at,updated_at
                 FROM task WHERE id=?1",
                params![id],
                task_row,
            )
            .optional()?;
        Ok(row)
    }

    pub fn list(&self, project_id: Option<&str>, limit: usize) -> Result<Vec<Task>> {
        let conn = self.lock()?;
        let limit = limit.clamp(1, 10_000) as i64;
        let mut out = Vec::new();
        match project_id {
            Some(p) => {
                let mut stmt = conn.prepare(
                    "SELECT id,project_id,project_name,butler_session,goal,detail,deps_json,require_verified_deps,
                            accept_json,status,owner_session,attempt,max_attempts,budget_json,model,
                            reversibility,evidence_json,assumptions_json,skipped_json,created_at,updated_at
                     FROM task WHERE project_id=?1 ORDER BY created_at ASC LIMIT ?2",
                )?;
                let rows = stmt.query_map(params![p, limit], task_row)?;
                for r in rows {
                    out.push(r?);
                }
            }
            None => {
                let mut stmt = conn.prepare(
                    "SELECT id,project_id,project_name,butler_session,goal,detail,deps_json,require_verified_deps,
                            accept_json,status,owner_session,attempt,max_attempts,budget_json,model,
                            reversibility,evidence_json,assumptions_json,skipped_json,created_at,updated_at
                     FROM task ORDER BY created_at DESC LIMIT ?1",
                )?;
                let rows = stmt.query_map(params![limit], task_row)?;
                for r in rows {
                    out.push(r?);
                }
            }
        }
        Ok(out)
    }

    /// Tasks that are allowed to start now: `ready`, plus `queued` whose
    /// dependencies are all verified. Promotes queued → ready as a side effect
    /// so the scheduler has one place to look.
    pub fn promote_and_list_ready(&self, project_id: &str) -> Result<Vec<Task>> {
        let all = self.list(Some(project_id), 5000)?;
        let by_id: std::collections::HashMap<&str, &Task> =
            all.iter().map(|t| (t.id.as_str(), t)).collect();
        let mut ready = Vec::new();
        for t in &all {
            if t.status == TaskStatus::Blocked {
                continue;
            }
            // A dependency that ended Unverified still produced usable work;
            // treating it as unsatisfied left every downstream task queued
            // forever (observed 2026-11-17: two tasks stuck behind one
            // unverified parent). A Failed dependency fails the chain; a
            // Blocked dependency leaves the child queued until the human
            // approves or denies it.
            if t.status == TaskStatus::Blocked {
                // A blocked task may be waiting for a human. If the decision
                // is already Denied/Expired, terminalize it; otherwise leave
                // it pending so approval can resume the chain.
                if let Ok(Some(a)) = self.latest_approval_for(&t.id) {
                    if matches!(a.status, ApprovalStatus::Denied | ApprovalStatus::Expired) {
                        let _ = self.finish(&t.id, TaskStatus::Failed, &[], &[], &[]);
                        let _ = self.append_event(
                            &t.id,
                            "approval_terminal",
                            "approval denied/expired",
                        );
                    }
                }
                continue;
            }
            let mut unverified_deps = Vec::new();
            let mut strict_unverified = Vec::new();
            let mut dead_deps = Vec::new();
            for d in &t.deps {
                let dep = match by_id.get(d.as_str()) {
                    Some(dep) => Some((*dep).clone()),
                    None => self.get(d).ok().flatten(),
                };
                let Some(dep) = dep else {
                    dead_deps.push(format!("{d} (missing)"));
                    continue;
                };
                if dep.project_id != t.project_id {
                    dead_deps.push(format!("{d} (other project)"));
                    continue;
                }
                match dep.status {
                    TaskStatus::Verified => {}
                    TaskStatus::Unverified => {
                        if t.require_verified_deps {
                            strict_unverified.push(dep.id);
                        } else {
                            unverified_deps.push(dep.id);
                        }
                    }
                    // Blocked means "waiting for a human", not dead: killing
                    // the whole downstream chain here made an approval
                    // useless.
                    TaskStatus::Failed => dead_deps.push(dep.id),
                    _ => {}
                }
            }
            let deps_ok = t.deps.iter().all(|d| match by_id.get(d.as_str()) {
                None => false,
                Some(dep) => {
                    if t.require_verified_deps {
                        dep.status == TaskStatus::Verified
                    } else {
                        matches!(dep.status, TaskStatus::Verified | TaskStatus::Unverified)
                    }
                }
            });
            // A cycle has no progress path; fail it explicitly instead of
            // leaving it queued forever (which suppresses the project digest).
            let cyclic = t.status == TaskStatus::Queued && Self::has_cycle(&t.id, &by_id);
            if t.status == TaskStatus::Queued && (!dead_deps.is_empty() || cyclic) {
                let detail = if cyclic {
                    "dependency cycle detected".to_string()
                } else {
                    format!("unusable dependencies: {}", dead_deps.join(", "))
                };
                let _ = self.finish(&t.id, TaskStatus::Failed, &[], &[], &[]);
                let _ = self.append_event(&t.id, "dependency_failed", &detail);
                continue;
            }
            if t.status == TaskStatus::Queued && !strict_unverified.is_empty() {
                let _ = self.append_event(
                    &t.id,
                    "dependency_unverified_wait",
                    &format!(
                        "strict deps require verified output: {}",
                        strict_unverified.join(", ")
                    ),
                );
                continue;
            }
            if t.status == TaskStatus::Queued && deps_ok {
                let detail = if unverified_deps.is_empty() {
                    "dependencies satisfied".to_string()
                } else {
                    format!(
                        "dependencies satisfied (unverified: {})",
                        unverified_deps.join(", ")
                    )
                };
                let _ = self.set_status(&t.id, TaskStatus::Ready, &detail);
                if !unverified_deps.is_empty() {
                    let _ = self.append_event(
                        &t.id,
                        "dependency_unverified",
                        &format!(
                            "continuing with unverified dependency output: {}",
                            unverified_deps.join(", ")
                        ),
                    );
                }
                let mut promoted = t.clone();
                promoted.status = TaskStatus::Ready;
                ready.push(promoted);
            } else if t.status == TaskStatus::Ready {
                ready.push(t.clone());
            }
        }
        ready.sort_by_key(|t| t.created_at);
        Ok(ready)
    }

    /// Does following `deps` from `start` revisit `start`? Used to fail cycles
    /// instead of leaving them queued forever. Missing deps terminate the walk.
    fn has_cycle(start: &str, by_id: &std::collections::HashMap<&str, &Task>) -> bool {
        fn walk(
            node: &str,
            start: &str,
            by_id: &std::collections::HashMap<&str, &Task>,
            seen: &mut std::collections::HashSet<String>,
        ) -> bool {
            if !seen.insert(node.to_string()) {
                return false;
            }
            let Some(t) = by_id.get(node) else {
                return false;
            };
            for d in &t.deps {
                if d == start {
                    return true;
                }
                if walk(d, start, by_id, seen) {
                    return true;
                }
            }
            false
        }
        let mut seen = std::collections::HashSet::new();
        walk(start, start, by_id, &mut seen)
    }

    pub fn set_status(&self, id: &str, next: TaskStatus, detail: &str) -> Result<Task> {
        let Some(current) = self.get(id)? else {
            return Err(StewardError::Invalid(format!("task not found: {id}")));
        };
        if !current.status.can_transition(next) {
            return Err(StewardError::Invalid(format!(
                "illegal task transition {:?} -> {:?}",
                current.status, next
            )));
        }
        let now = now_ms();
        {
            let conn = self.lock()?;
            let n = conn.execute(
                "UPDATE task SET status=?2, updated_at=?3 WHERE id=?1 AND status=?4",
                params![id, next.as_str(), now, current.status.as_str()],
            )?;
            if n == 0 {
                return Err(StewardError::Invalid(format!(
                    "task {id} changed status concurrently (expected {:?})",
                    current.status
                )));
            }
        }
        self.append_event(id, "status", detail)?;
        self.get(id)?
            .ok_or_else(|| StewardError::Invalid("task vanished".into()))
    }

    /// Upgrade a queued task's acceptance gates (used to add SourceCheck to
    /// tasks created before the gate existed). Only the scheduler calls this.
    pub fn update_accept(&self, id: &str, accept: &[GoalGate]) -> Result<()> {
        let conn = self.lock()?;
        conn.execute(
            "UPDATE task SET accept_json=?2, updated_at=?3 WHERE id=?1",
            params![id, serde_json::to_string(accept)?, now_ms()],
        )?;
        Ok(())
    }

    /// Atomically take a Ready task. Returns false if another scheduler/tick
    /// already claimed it. This is the only legal way to enter Running.
    pub fn claim_task(&self, id: &str) -> Result<bool> {
        let conn = self.lock()?;
        let n = conn.execute(
            "UPDATE task SET status='running', updated_at=?2
             WHERE id=?1 AND status='ready'",
            params![id, now_ms()],
        )?;
        Ok(n == 1)
    }

    /// Put a task back after a post-claim dispatch failure (owner/run/DB).
    pub fn release_claim(&self, id: &str, detail: &str) -> Result<()> {
        let conn = self.lock()?;
        conn.execute(
            "UPDATE task SET status='ready', owner_session=NULL, updated_at=?2
             WHERE id=?1 AND status='running'",
            params![id, now_ms()],
        )?;
        drop(conn);
        let _ = self.append_event(id, "claim_released", detail);
        Ok(())
    }

    pub fn set_owner(&self, id: &str, owner: Option<&str>) -> Result<()> {
        let conn = self.lock()?;
        conn.execute(
            "UPDATE task SET owner_session=?2, updated_at=?3 WHERE id=?1",
            params![id, owner, now_ms()],
        )?;
        Ok(())
    }

    pub fn bump_attempt(&self, id: &str) -> Result<u32> {
        let conn = self.lock()?;
        conn.execute(
            "UPDATE task SET attempt=attempt+1, updated_at=?2 WHERE id=?1",
            params![id, now_ms()],
        )?;
        let n: u32 = conn.query_row("SELECT attempt FROM task WHERE id=?1", params![id], |r| {
            r.get(0)
        })?;
        Ok(n)
    }

    pub fn finish(
        &self,
        id: &str,
        status: TaskStatus,
        evidence: &[GateResult],
        assumptions: &[String],
        skipped: &[String],
    ) -> Result<Task> {
        let conn = self.lock()?;
        // Notes recorded through `task_note` during the run must survive
        // finalisation: an empty argument list means "keep what is there".
        let n = if assumptions.is_empty() && skipped.is_empty() {
            conn.execute(
                "UPDATE task SET status=?2, evidence_json=?3, updated_at=?4
                 WHERE id=?1 AND status NOT IN ('verified','unverified','failed')",
                params![
                    id,
                    status.as_str(),
                    serde_json::to_string(evidence)?,
                    now_ms()
                ],
            )?
        } else {
            conn.execute(
                "UPDATE task SET status=?2, evidence_json=?3, assumptions_json=?4,
                                skipped_json=?5, updated_at=?6
                 WHERE id=?1 AND status NOT IN ('verified','unverified','failed')",
                params![
                    id,
                    status.as_str(),
                    serde_json::to_string(evidence)?,
                    serde_json::to_string(assumptions)?,
                    serde_json::to_string(skipped)?,
                    now_ms()
                ],
            )?
        };
        drop(conn);
        if n == 0 {
            return Err(StewardError::Invalid(format!(
                "task {id} is already terminal or vanished"
            )));
        }
        self.get(id)?
            .ok_or_else(|| StewardError::Invalid("task vanished".into()))
    }

    /// Record an assumption / skipped item / free note on a task.
    pub fn add_note(&self, id: &str, kind: &str, text: &str) -> Result<()> {
        let Some(mut task) = self.get(id)? else {
            return Err(StewardError::Invalid(format!("task not found: {id}")));
        };
        match kind {
            "assumption" => task.assumptions.push(text.to_string()),
            "skipped" => task.skipped.push(text.to_string()),
            _ => {}
        }
        if kind == "assumption" || kind == "skipped" {
            let conn = self.lock()?;
            conn.execute(
                "UPDATE task SET assumptions_json=?2, skipped_json=?3, updated_at=?4 WHERE id=?1",
                params![
                    id,
                    serde_json::to_string(&task.assumptions)?,
                    serde_json::to_string(&task.skipped)?,
                    now_ms()
                ],
            )?;
        }
        drop(self.lock());
        self.append_event(id, kind, text)
    }

    pub fn append_event(&self, task_id: &str, kind: &str, detail: &str) -> Result<()> {
        let conn = self.lock()?;
        conn.execute(
            "INSERT INTO task_event(id,task_id,at,kind,detail) VALUES (?1,?2,?3,?4,?5)",
            params![Uuid::new_v4().to_string(), task_id, now_ms(), kind, detail],
        )?;
        Ok(())
    }

    pub fn events(&self, task_id: &str, limit: usize) -> Result<Vec<TaskEvent>> {
        let conn = self.lock()?;
        let mut stmt = conn.prepare(
            "SELECT id,task_id,at,kind,detail FROM task_event
             WHERE task_id=?1 ORDER BY at DESC LIMIT ?2",
        )?;
        let rows = stmt.query_map(params![task_id, limit.clamp(1, 200) as i64], |r| {
            Ok(TaskEvent {
                id: r.get(0)?,
                task_id: r.get(1)?,
                at: r.get(2)?,
                kind: r.get(3)?,
                detail: r.get(4)?,
            })
        })?;
        let mut out = Vec::new();
        for r in rows {
            out.push(r?);
        }
        Ok(out)
    }

    pub fn counts(&self, project_id: Option<&str>) -> Result<TaskCounts> {
        let conn = self.lock()?;
        let (sql, param): (&str, Option<&str>) = match project_id {
            Some(p) => (
                "SELECT status, COUNT(*) FROM task WHERE project_id=?1 GROUP BY status",
                Some(p),
            ),
            None => ("SELECT status, COUNT(*) FROM task GROUP BY status", None),
        };
        let mut stmt = conn.prepare(sql)?;
        let mut c = TaskCounts {
            total: 0,
            verified: 0,
            unverified: 0,
            failed: 0,
            open: 0,
        };
        let mut rows = match param {
            Some(p) => stmt.query(params![p])?,
            None => stmt.query([])?,
        };
        while let Some(r) = rows.next()? {
            let status: String = r.get(0)?;
            let n: i64 = r.get(1)?;
            c.total += n as usize;
            match status.as_str() {
                "verified" => c.verified += n as usize,
                "unverified" => c.unverified += n as usize,
                "failed" => c.failed += n as usize,
                _ => c.open += n as usize,
            }
        }
        Ok(c)
    }

    pub fn all_terminal(&self, project_id: &str) -> Result<bool> {
        let conn = self.lock()?;
        let n: i64 = conn.query_row(
            "SELECT COUNT(*) FROM task WHERE project_id=?1
             AND status NOT IN ('verified','unverified','failed')",
            params![project_id],
            |r| r.get(0),
        )?;
        Ok(n == 0)
    }

    /// Startup repair: no worker can still be alive before the scheduler
    /// starts, so any persisted running/verifying task is orphaned. Put it
    /// back in the queue and close its run instead of leaving it in-flight
    /// forever (which also suppresses the project digest).
    pub fn recover_orphaned_tasks(&self) -> Result<usize> {
        let now = now_ms();
        let conn = self.lock()?;
        let ids: Vec<String> = {
            let mut stmt =
                conn.prepare("SELECT id FROM task WHERE status IN ('running','verifying')")?;
            let rows = stmt.query_map([], |r| r.get(0))?;
            rows.filter_map(|r| r.ok()).collect()
        };
        for id in &ids {
            conn.execute(
                "UPDATE task SET status='ready', owner_session=NULL, updated_at=?2 WHERE id=?1",
                params![id, now],
            )?;
            conn.execute(
                "UPDATE run SET status='failed', summary=?2, finished_at=?3
                 WHERE task_id=?1 AND status='running'",
                params![id, "orphaned by restart; task requeued", now],
            )?;
        }
        drop(conn);
        for id in &ids {
            let _ = self.append_event(id, "recovered", "orphaned by restart; requeued");
        }
        Ok(ids.len())
    }

    /// Drop finished run rows older than `days`; transcripts live in the
    /// agents sidecar and are intentionally not deleted here.
    pub fn prune_runs(&self, days: i64) -> Result<usize> {
        let cutoff = now_ms() - days.max(1) * 86_400_000;
        let conn = self.lock()?;
        let n = conn.execute(
            "DELETE FROM run WHERE finished_at IS NOT NULL AND finished_at < ?1",
            params![cutoff],
        )?;
        Ok(n)
    }

    /// Stop is a lifecycle verb: cancel every open task owned by this butler
    /// (or by this worker session) so nothing is left running with no owner.
    pub fn fail_tasks_for_butler(&self, butler_session: &str, reason: &str) -> Result<usize> {
        self.fail_tasks_where("butler_session", butler_session, reason)
    }

    pub fn fail_tasks_for_owner(&self, owner_session: &str, reason: &str) -> Result<usize> {
        self.fail_tasks_where("owner_session", owner_session, reason)
    }

    fn fail_tasks_where(&self, column: &str, value: &str, reason: &str) -> Result<usize> {
        let conn = self.lock()?;
        let sql = format!(
            "SELECT id FROM task WHERE {column}=?1 AND status NOT IN ('verified','unverified','failed')"
        );
        let ids: Vec<String> = {
            let mut stmt = conn.prepare(&sql)?;
            let rows = stmt.query_map(params![value], |r| r.get(0))?;
            rows.filter_map(|r| r.ok()).collect()
        };
        for id in &ids {
            conn.execute(
                "UPDATE task SET status='failed', owner_session=NULL, updated_at=?2 WHERE id=?1",
                params![id, now_ms()],
            )?;
            conn.execute(
                "UPDATE run SET status='failed', summary=?2, finished_at=?3
                 WHERE task_id=?1 AND status='running'",
                params![id, reason, now_ms()],
            )?;
        }
        drop(conn);
        for id in &ids {
            let _ = self.append_event(id, "cancelled", reason);
        }
        Ok(ids.len())
    }

    pub fn digest_sent(&self, project_id: &str) -> Result<bool> {
        let conn = self.lock()?;
        let sent: Option<Option<i64>> = conn
            .query_row(
                "SELECT sent_at FROM digest WHERE project_id=?1",
                params![project_id],
                |r| r.get(0),
            )
            .optional()?;
        Ok(matches!(sent, Some(Some(_))))
    }

    pub fn mark_digest_sent(&self, project_id: &str) -> Result<()> {
        let conn = self.lock()?;
        conn.execute(
            "INSERT INTO digest(project_id,sent_at,last_error) VALUES (?1,?2,'')
             ON CONFLICT(project_id) DO UPDATE SET sent_at=?2, last_error=''",
            params![project_id, now_ms()],
        )?;
        Ok(())
    }

    pub fn mark_digest_failed(&self, project_id: &str, error: &str) -> Result<()> {
        let conn = self.lock()?;
        conn.execute(
            "INSERT INTO digest(project_id,sent_at,last_error) VALUES (?1,NULL,?2)
             ON CONFLICT(project_id) DO UPDATE SET last_error=?2",
            params![project_id, error],
        )?;
        Ok(())
    }

    pub fn clear_digest(&self, project_id: &str) -> Result<()> {
        let conn = self.lock()?;
        conn.execute(
            "DELETE FROM digest WHERE project_id=?1",
            params![project_id],
        )?;
        Ok(())
    }

    // ── approvals ──────────────────────────────────────────────────────

    pub fn create_approval(&self, approval: &Approval) -> Result<()> {
        let conn = self.lock()?;
        conn.execute(
            "INSERT INTO approval(id,task_id,action,impact,reversibility,status,created_at,decided_at)
             VALUES (?1,?2,?3,?4,?5,?6,?7,?8)",
            params![
                approval.id,
                approval.task_id,
                approval.action,
                approval.impact,
                approval.reversibility,
                approval.status.as_str(),
                approval.created_at,
                approval.decided_at
            ],
        )?;
        Ok(())
    }

    pub fn approvals(&self, status: Option<ApprovalStatus>) -> Result<Vec<Approval>> {
        let conn = self.lock()?;
        let mut out = Vec::new();
        let mapper = |r: &rusqlite::Row<'_>| {
            Ok(Approval {
                id: r.get(0)?,
                task_id: r.get(1)?,
                action: r.get(2)?,
                impact: r.get(3)?,
                reversibility: r.get(4)?,
                status: ApprovalStatus::parse(&r.get::<_, String>(5)?).map_err(|_| {
                    rusqlite::Error::InvalidColumnType(
                        5,
                        "status".into(),
                        rusqlite::types::Type::Text,
                    )
                })?,
                created_at: r.get(6)?,
                decided_at: r.get(7)?,
            })
        };
        match status {
            Some(s) => {
                let mut stmt = conn.prepare(
                    "SELECT id,task_id,action,impact,reversibility,status,created_at,decided_at
                     FROM approval WHERE status=?1 ORDER BY created_at DESC LIMIT 100",
                )?;
                let rows = stmt.query_map(params![s.as_str()], mapper)?;
                for r in rows {
                    out.push(r?);
                }
            }
            None => {
                let mut stmt = conn.prepare(
                    "SELECT id,task_id,action,impact,reversibility,status,created_at,decided_at
                     FROM approval ORDER BY created_at DESC LIMIT 100",
                )?;
                let rows = stmt.query_map([], mapper)?;
                for r in rows {
                    out.push(r?);
                }
            }
        }
        Ok(out)
    }

    pub fn decide_approval(&self, id: &str, approved: bool) -> Result<Option<Approval>> {
        let status = if approved {
            ApprovalStatus::Approved
        } else {
            ApprovalStatus::Denied
        };
        {
            let conn = self.lock()?;
            conn.execute(
                "UPDATE approval SET status=?2, decided_at=?3 WHERE id=?1 AND status='pending'",
                params![id, status.as_str(), now_ms()],
            )?;
        }
        Ok(self.approvals(None)?.into_iter().find(|a| a.id == id))
    }

    pub fn pending_approval_for(&self, task_id: &str) -> Result<Option<Approval>> {
        Ok(self
            .approvals(Some(ApprovalStatus::Pending))?
            .into_iter()
            .find(|a| a.task_id == task_id))
    }

    /// Latest approval for a task, whatever its status. Dispatch consults
    /// this so an approved task is not queued again on the next tick.
    pub fn latest_approval_for(&self, task_id: &str) -> Result<Option<Approval>> {
        let conn = self.lock()?;
        let mut stmt = conn.prepare(
            "SELECT id,task_id,action,impact,reversibility,status,created_at,decided_at
             FROM approval WHERE task_id=?1 ORDER BY created_at DESC LIMIT 1",
        )?;
        let mut rows = stmt.query_map(params![task_id], |r| {
            Ok(Approval {
                id: r.get(0)?,
                task_id: r.get(1)?,
                action: r.get(2)?,
                impact: r.get(3)?,
                reversibility: r.get(4)?,
                status: ApprovalStatus::parse(&r.get::<_, String>(5)?).map_err(|_| {
                    rusqlite::Error::InvalidColumnType(
                        5,
                        "status".into(),
                        rusqlite::types::Type::Text,
                    )
                })?,
                created_at: r.get(6)?,
                decided_at: r.get(7)?,
            })
        })?;
        match rows.next() {
            Some(r) => Ok(Some(r?)),
            None => Ok(None),
        }
    }

    pub fn create_run(&self, run: &Run) -> Result<()> {
        let conn = self.lock()?;
        conn.execute(
            "INSERT INTO run(run_id,task_id,project_id,child_id,model,started_at,finished_at,status,summary)
             VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9)",
            params![
                run.run_id,
                run.task_id,
                run.project_id,
                run.child_id,
                run.model,
                run.started_at,
                run.finished_at,
                run.status,
                run.summary,
            ],
        )?;
        Ok(())
    }

    pub fn finish_run(&self, run_id: &str, status: &str, summary: &str) -> Result<()> {
        let conn = self.lock()?;
        conn.execute(
            "UPDATE run SET status=?2, summary=?3, finished_at=?4 WHERE run_id=?1",
            params![run_id, status, summary, now_ms()],
        )?;
        Ok(())
    }

    pub fn get_run(&self, run_id: &str) -> Result<Option<Run>> {
        let conn = self.lock()?;
        conn.query_row(
            "SELECT run_id,task_id,project_id,child_id,model,started_at,finished_at,status,summary
             FROM run WHERE run_id=?1",
            params![run_id],
            run_row,
        )
        .optional()
        .map_err(Into::into)
    }

    pub fn list_runs(&self, project_id: Option<&str>, limit: usize) -> Result<Vec<Run>> {
        let conn = self.lock()?;
        let limit = limit.clamp(1, 200) as i64;
        let (sql, param): (&str, Option<&str>) = match project_id {
            Some(p) => (
                "SELECT run_id,task_id,project_id,child_id,model,started_at,finished_at,status,summary
                 FROM run WHERE project_id=?1 ORDER BY started_at DESC LIMIT ?2",
                Some(p),
            ),
            None => (
                "SELECT run_id,task_id,project_id,child_id,model,started_at,finished_at,status,summary
                 FROM run ORDER BY started_at DESC LIMIT ?1",
                None,
            ),
        };
        let mut stmt = conn.prepare(sql)?;
        let rows = match param {
            Some(p) => stmt.query_map(params![p, limit], run_row)?,
            None => stmt.query_map(params![limit], run_row)?,
        };
        Ok(rows.filter_map(|r| r.ok()).collect())
    }

    pub fn expire_approvals(&self, older_than_ms: i64) -> Result<usize> {
        let cutoff = now_ms() - older_than_ms.max(0);
        let conn = self.lock()?;
        let n = conn.execute(
            "UPDATE approval SET status='expired', decided_at=?2
             WHERE status='pending' AND created_at < ?1",
            params![cutoff, now_ms()],
        )?;
        Ok(n)
    }
}

fn run_row(r: &rusqlite::Row<'_>) -> rusqlite::Result<Run> {
    Ok(Run {
        run_id: r.get(0)?,
        task_id: r.get(1)?,
        project_id: r.get(2)?,
        child_id: r.get(3)?,
        model: r.get(4)?,
        started_at: r.get(5)?,
        finished_at: r.get(6)?,
        status: r.get(7)?,
        summary: r.get(8)?,
    })
}

fn task_row(r: &rusqlite::Row<'_>) -> rusqlite::Result<Task> {
    let status: String = r.get(9)?;
    Ok(Task {
        id: r.get(0)?,
        project_id: r.get(1)?,
        project_name: r.get(2)?,
        butler_session: r.get(3)?,
        goal: r.get(4)?,
        detail: r.get(5)?,
        deps: serde_json::from_str(&r.get::<_, String>(6)?).unwrap_or_default(),
        require_verified_deps: r.get(7)?,
        accept: serde_json::from_str(&r.get::<_, String>(8)?).unwrap_or_default(),
        status: TaskStatus::parse(&status).unwrap_or(TaskStatus::Blocked),
        owner_session: r.get(10)?,
        attempt: r.get(11)?,
        max_attempts: r.get(12)?,
        budget: serde_json::from_str(&r.get::<_, String>(13)?).unwrap_or_default(),
        model: r.get(14)?,
        reversibility: crate::butler::spec::Reversibility::parse(&r.get::<_, String>(15)?)
            .unwrap_or(crate::butler::spec::Reversibility::WorkspaceWrite),
        evidence: serde_json::from_str(&r.get::<_, String>(16)?).unwrap_or_default(),
        assumptions: serde_json::from_str(&r.get::<_, String>(17)?).unwrap_or_default(),
        skipped: serde_json::from_str(&r.get::<_, String>(18)?).unwrap_or_default(),
        created_at: r.get(19)?,
        updated_at: r.get(20)?,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::goal::spec::GoalGate;

    fn task(store: &ButlerStore, id: &str, deps: &[&str]) -> Task {
        let mut t = Task::new("p", "butler", "do it");
        t.id = id.into();
        t.deps = deps.iter().map(|s| s.to_string()).collect();
        t.accept = vec![GoalGate::Command {
            command: "true".into(),
            timeout_secs: 5,
            retries: 0,
        }];
        store.create_task(&t).unwrap();
        t
    }

    #[test]
    fn runs_are_recorded_and_finished() {
        let store = ButlerStore::open_memory().unwrap();
        let run = Run {
            run_id: "r1".into(),
            task_id: "t1".into(),
            project_id: "/tmp/p".into(),
            child_id: "c1".into(),
            model: "m".into(),
            started_at: 1,
            finished_at: None,
            status: "running".into(),
            summary: String::new(),
        };
        store.create_run(&run).unwrap();
        store.finish_run("r1", "verified", "ok").unwrap();
        let runs = store.list_runs(Some("/tmp/p"), 10).unwrap();
        assert_eq!(runs.len(), 1);
        assert_eq!(runs[0].status, "verified");
        assert_eq!(runs[0].summary, "ok");
        assert!(runs[0].finished_at.is_some());
    }

    #[test]
    fn project_name_roundtrips() {
        let store = ButlerStore::open_memory().unwrap();
        let mut t = Task::new("/tmp/visualmusic", "butler", "make cover");
        t.project_name = Some("visualmusic".into());
        store.create_task(&t).unwrap();
        let got = store.get(&t.id).unwrap().unwrap();
        assert_eq!(got.project_name.as_deref(), Some("visualmusic"));
        assert_eq!(got.project_id, "/tmp/visualmusic");
    }

    #[test]
    fn stop_fails_open_tasks_and_closes_runs() {
        let store = ButlerStore::open_memory().unwrap();
        task(&store, "t1", &[]);
        store.set_status("t1", TaskStatus::Ready, "go").unwrap();
        store.set_status("t1", TaskStatus::Running, "run").unwrap();
        store
            .create_run(&Run {
                run_id: "r1".into(),
                task_id: "t1".into(),
                project_id: "p".into(),
                child_id: "c1".into(),
                model: "m".into(),
                started_at: 1,
                finished_at: None,
                status: "running".into(),
                summary: String::new(),
            })
            .unwrap();
        assert_eq!(store.fail_tasks_for_butler("butler", "stopped").unwrap(), 1);
        assert_eq!(store.get("t1").unwrap().unwrap().status, TaskStatus::Failed);
        assert_eq!(store.list_runs(None, 10).unwrap()[0].status, "failed");
    }

    #[test]
    fn claim_is_compare_and_swap() {
        let store = ButlerStore::open_memory().unwrap();
        task(&store, "t1", &[]);
        store.set_status("t1", TaskStatus::Ready, "go").unwrap();
        assert!(store.claim_task("t1").unwrap());
        assert!(!store.claim_task("t1").unwrap(), "second claim must fail");
        assert_eq!(
            store.get("t1").unwrap().unwrap().status,
            TaskStatus::Running
        );
    }

    #[test]
    fn orphaned_running_task_is_requeued_and_run_closed() {
        let store = ButlerStore::open_memory().unwrap();
        task(&store, "t1", &[]);
        store.set_status("t1", TaskStatus::Ready, "go").unwrap();
        store.set_status("t1", TaskStatus::Running, "run").unwrap();
        store
            .create_run(&Run {
                run_id: "r1".into(),
                task_id: "t1".into(),
                project_id: "p".into(),
                child_id: "c1".into(),
                model: "m".into(),
                started_at: 1,
                finished_at: None,
                status: "running".into(),
                summary: String::new(),
            })
            .unwrap();
        assert_eq!(store.recover_orphaned_tasks().unwrap(), 1);
        assert_eq!(store.get("t1").unwrap().unwrap().status, TaskStatus::Ready);
        assert_eq!(store.list_runs(None, 10).unwrap()[0].status, "failed");
    }

    #[test]
    fn digest_state_survives_a_send_failure() {
        let store = ButlerStore::open_memory().unwrap();
        assert!(!store.digest_sent("p").unwrap());
        store.mark_digest_failed("p", "offline").unwrap();
        assert!(!store.digest_sent("p").unwrap());
        store.mark_digest_sent("p").unwrap();
        assert!(store.digest_sent("p").unwrap());
        store.clear_digest("p").unwrap();
        assert!(!store.digest_sent("p").unwrap());
    }

    #[test]
    fn unverified_dependency_unblocks_downstream_with_a_note() {
        let store = ButlerStore::open_memory().unwrap();
        task(&store, "root", &[]);
        store
            .finish("root", TaskStatus::Unverified, &[], &[], &[])
            .unwrap();
        task(&store, "child", &["root"]);
        let ready = store.promote_and_list_ready("p").unwrap();
        assert_eq!(ready.len(), 1);
        assert_eq!(ready[0].id, "child");
        assert_eq!(ready[0].status, TaskStatus::Ready);
        let events = store.events("child", 20).unwrap();
        assert!(events.iter().any(|e| e.kind == "dependency_unverified"));
    }

    #[test]
    fn failed_dependency_fails_downstream_instead_of_queueing_forever() {
        let store = ButlerStore::open_memory().unwrap();
        task(&store, "root", &[]);
        store
            .finish("root", TaskStatus::Failed, &[], &[], &[])
            .unwrap();
        task(&store, "child", &["root"]);
        assert!(store.promote_and_list_ready("p").unwrap().is_empty());
        assert_eq!(
            store.get("child").unwrap().unwrap().status,
            TaskStatus::Failed
        );
    }

    #[test]
    fn missing_and_cyclic_deps_fail_instead_of_queueing_forever() {
        let store = ButlerStore::open_memory().unwrap();
        task(&store, "missing-child", &["does-not-exist"]);
        task(&store, "a", &["b"]);
        task(&store, "b", &["a"]);
        let ready = store.promote_and_list_ready("p").unwrap();
        assert!(ready.is_empty());
        for id in ["missing-child", "a", "b"] {
            assert_eq!(
                store.get(id).unwrap().unwrap().status,
                TaskStatus::Failed,
                "{id} must be terminal, not queued"
            );
        }
    }

    #[test]
    fn create_list_and_transition() {
        let store = ButlerStore::open_memory().unwrap();
        task(&store, "t1", &[]);
        assert_eq!(store.list(Some("p"), 10).unwrap().len(), 1);
        store.set_status("t1", TaskStatus::Ready, "go").unwrap();
        store
            .set_status("t1", TaskStatus::Running, "dispatch")
            .unwrap();
        assert!(store.set_status("t1", TaskStatus::Queued, "back").is_err());
    }

    #[test]
    fn deps_gate_promotion() {
        let store = ButlerStore::open_memory().unwrap();
        task(&store, "a", &[]);
        task(&store, "b", &["a"]);
        let ready = store.promote_and_list_ready("p").unwrap();
        assert_eq!(ready.len(), 1);
        assert_eq!(ready[0].id, "a");
        store
            .finish("a", TaskStatus::Verified, &[], &[], &[])
            .unwrap();
        let ready = store.promote_and_list_ready("p").unwrap();
        assert_eq!(ready.len(), 1);
        assert_eq!(ready[0].id, "b");
    }

    #[test]
    fn approvals_roundtrip_and_expiry() {
        let store = ButlerStore::open_memory().unwrap();
        task(&store, "t1", &[]);
        let mut a = Approval::new("t1", "publish");
        a.created_at = 1;
        store.create_approval(&a).unwrap();
        assert_eq!(store.pending_approval_for("t1").unwrap().unwrap().id, a.id);
        store.decide_approval(&a.id, false).unwrap();
        assert!(store.pending_approval_for("t1").unwrap().is_none());
        assert_eq!(store.expire_approvals(1000).unwrap(), 0);
    }

    #[test]
    fn latest_approval_reflects_the_decision() {
        let store = ButlerStore::open_memory().unwrap();
        task(&store, "t1", &[]);
        let mut a = Approval::new("t1", "write outside");
        a.reversibility = "outside_write".into();
        store.create_approval(&a).unwrap();
        assert_eq!(
            store.latest_approval_for("t1").unwrap().unwrap().status,
            ApprovalStatus::Pending
        );
        store.decide_approval(&a.id, true).unwrap();
        assert_eq!(
            store.latest_approval_for("t1").unwrap().unwrap().status,
            ApprovalStatus::Approved
        );
    }

    #[test]
    fn finishing_keeps_recorded_notes() {
        let store = ButlerStore::open_memory().unwrap();
        task(&store, "t1", &[]);
        store
            .add_note("t1", "assumption", "used last baseline")
            .unwrap();
        store
            .finish("t1", TaskStatus::Unverified, &[], &[], &[])
            .unwrap();
        let t = store.get("t1").unwrap().unwrap();
        assert_eq!(t.assumptions, vec!["used last baseline".to_string()]);
        assert_eq!(t.status, TaskStatus::Unverified);
    }

    #[test]
    fn corrupt_json_does_not_panic() {
        let store = ButlerStore::open_memory().unwrap();
        task(&store, "t1", &[]);
        {
            let conn = store.lock().unwrap();
            conn.execute("UPDATE task SET accept_json='{not json' WHERE id='t1'", [])
                .unwrap();
        }
        let t = store.get("t1").unwrap().unwrap();
        assert!(t.accept.is_empty());
    }

    #[test]
    fn blocked_dependency_waits_for_human_then_fails() {
        let store = ButlerStore::open_memory().unwrap();
        task(&store, "a", &[]);
        store
            .set_status("a", TaskStatus::Blocked, "approval pending")
            .unwrap();
        task(&store, "b", &["a"]);
        assert!(store.promote_and_list_ready("p").unwrap().is_empty());
        assert_eq!(store.get("b").unwrap().unwrap().status, TaskStatus::Queued);
        store
            .finish("a", TaskStatus::Failed, &[], &[], &[])
            .unwrap();
        assert!(store.promote_and_list_ready("p").unwrap().is_empty());
        assert_eq!(store.get("b").unwrap().unwrap().status, TaskStatus::Failed);
    }

    #[test]
    fn terminal_task_cannot_be_finished_twice() {
        let store = ButlerStore::open_memory().unwrap();
        task(&store, "t1", &[]);
        store
            .finish("t1", TaskStatus::Failed, &[], &[], &[])
            .unwrap();
        assert!(
            store
                .finish("t1", TaskStatus::Verified, &[], &[], &[])
                .is_err(),
            "a stopped task must not be resurrected by a late verifier"
        );
    }
}
