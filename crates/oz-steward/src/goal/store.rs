use std::path::Path;
use std::sync::{Mutex, MutexGuard};

use rusqlite::{params, Connection, OptionalExtension};
use uuid::Uuid;

use oz_core_types::supervision::{now_ms, TicketStatus};

use crate::error::{Result, StewardError};

#[cfg(test)]
use super::spec::GoalGate;
use super::spec::{GoalAttempt, GoalEvent, GoalRecord, GoalReport, GoalSpec, GoalStatus};

pub struct GoalStore {
    conn: Mutex<Connection>,
}

impl GoalStore {
    pub fn open(path: impl AsRef<Path>) -> Result<Self> {
        let conn = Connection::open(path)?;
        Self::init(conn)
    }

    pub fn open_memory() -> Result<Self> {
        Self::init(Connection::open_in_memory()?)
    }

    fn init(conn: Connection) -> Result<Self> {
        conn.pragma_update(None, "journal_mode", "WAL")?;
        conn.execute_batch(
            r#"
            CREATE TABLE IF NOT EXISTS goal (
                id TEXT PRIMARY KEY,
                status TEXT NOT NULL,
                current_version INTEGER NOT NULL,
                spec_json TEXT NOT NULL,
                attempts INTEGER NOT NULL DEFAULT 0,
                consecutive_failures INTEGER NOT NULL DEFAULT 0,
                no_progress_rounds INTEGER NOT NULL DEFAULT 0,
                tokens_used INTEGER NOT NULL DEFAULT 0,
                wall_started_at INTEGER,
                last_report_id TEXT,
                created_at INTEGER NOT NULL,
                updated_at INTEGER NOT NULL
            );
            CREATE TABLE IF NOT EXISTS goal_version (
                goal_id TEXT NOT NULL,
                version INTEGER NOT NULL,
                spec_json TEXT NOT NULL,
                reason TEXT NOT NULL,
                created_at INTEGER NOT NULL,
                PRIMARY KEY (goal_id, version)
            );
            CREATE TABLE IF NOT EXISTS goal_attempt (
                goal_id TEXT NOT NULL,
                attempt_no INTEGER NOT NULL,
                started_at INTEGER NOT NULL,
                ended_at INTEGER NOT NULL,
                summary TEXT NOT NULL,
                outcome TEXT NOT NULL,
                failure_class TEXT,
                checkpoint_id TEXT,
                tokens INTEGER NOT NULL,
                gates_json TEXT NOT NULL,
                evidence_json TEXT NOT NULL,
                PRIMARY KEY (goal_id, attempt_no)
            );
            CREATE TABLE IF NOT EXISTS goal_event (
                id TEXT PRIMARY KEY,
                goal_id TEXT NOT NULL,
                at INTEGER NOT NULL,
                kind TEXT NOT NULL,
                detail TEXT NOT NULL
            );
            CREATE TABLE IF NOT EXISTS goal_lease (
                goal_id TEXT PRIMARY KEY,
                runner_id TEXT NOT NULL,
                heartbeat_at INTEGER NOT NULL,
                expires_at INTEGER NOT NULL
            );
            CREATE TABLE IF NOT EXISTS goal_report (
                id TEXT PRIMARY KEY,
                goal_id TEXT NOT NULL,
                at INTEGER NOT NULL,
                status TEXT NOT NULL,
                body_json TEXT NOT NULL
            );
            CREATE INDEX IF NOT EXISTS goal_status_idx ON goal(status);
            CREATE INDEX IF NOT EXISTS goal_event_goal ON goal_event(goal_id, at);
            "#,
        )?;
        Ok(Self {
            conn: Mutex::new(conn),
        })
    }

    fn lock(&self) -> Result<MutexGuard<'_, Connection>> {
        self.conn
            .lock()
            .map_err(|_| StewardError::Invalid("goal store lock poisoned".into()))
    }

    pub fn create(&self, mut spec: GoalSpec) -> Result<GoalRecord> {
        spec.validate()?;
        let now = now_ms();
        let conn = self.lock()?;
        conn.execute(
            "INSERT INTO goal(id,status,current_version,spec_json,attempts,consecutive_failures,no_progress_rounds,tokens_used,created_at,updated_at)
             VALUES (?1,'draft',?2,?3,0,0,0,0,?4,?4)",
            params![
                spec.id,
                spec.version as i64,
                serde_json::to_string(&spec)?,
                now
            ],
        )?;
        conn.execute(
            "INSERT INTO goal_version(goal_id,version,spec_json,reason,created_at)
             VALUES (?1,?2,?3,'created',?4)",
            params![
                spec.id,
                spec.version as i64,
                serde_json::to_string(&spec)?,
                now
            ],
        )?;
        drop(conn);
        self.get(&spec.id)?
            .ok_or_else(|| StewardError::Invalid("goal insert vanished".into()))
    }

    pub fn get(&self, id: &str) -> Result<Option<GoalRecord>> {
        let conn = self.lock()?;
        let row = conn
            .query_row(
                "SELECT id,status,current_version,spec_json,attempts,consecutive_failures,no_progress_rounds,tokens_used,wall_started_at,last_report_id,created_at,updated_at
                 FROM goal WHERE id=?1",
                params![id],
                |r| {
                    let status: String = r.get(1)?;
                    let spec_json: String = r.get(3)?;
                    Ok((
                        r.get::<_, String>(0)?,
                        status,
                        r.get::<_, i64>(2)?,
                        spec_json,
                        r.get::<_, i64>(4)?,
                        r.get::<_, i64>(5)?,
                        r.get::<_, i64>(6)?,
                        r.get::<_, i64>(7)?,
                        r.get::<_, Option<i64>>(8)?,
                        r.get::<_, Option<String>>(9)?,
                        r.get::<_, i64>(10)?,
                        r.get::<_, i64>(11)?,
                    ))
                },
            )
            .optional()?;
        row.map(|r| {
            let spec: GoalSpec = serde_json::from_str(&r.3)?;
            Ok(GoalRecord {
                id: r.0,
                status: parse_status(&r.1)?,
                current_version: r.2 as u32,
                spec,
                attempts: r.4 as u32,
                consecutive_failures: r.5 as u32,
                no_progress_rounds: r.6 as u32,
                tokens_used: r.7 as u64,
                wall_started_at: r.8,
                last_report_id: r.9,
                created_at: r.10,
                updated_at: r.11,
            })
        })
        .transpose()
    }

    pub fn list(&self, limit: usize) -> Result<Vec<GoalRecord>> {
        let ids: Vec<String> = {
            let conn = self.lock()?;
            let mut stmt = conn.prepare("SELECT id FROM goal ORDER BY created_at DESC LIMIT ?1")?;
            let rows = stmt.query_map(params![limit as i64], |r| r.get::<_, String>(0))?;
            rows.collect::<std::result::Result<Vec<_>, _>>()?
        };
        let mut out = Vec::new();
        for id in ids {
            if let Some(goal) = self.get(&id)? {
                out.push(goal);
            }
        }
        Ok(out)
    }

    pub fn set_status(&self, id: &str, next: GoalStatus, detail: &str) -> Result<GoalRecord> {
        let goal = self
            .get(id)?
            .ok_or_else(|| StewardError::Invalid(format!("goal not found: {id}")))?;
        if !goal.status.can_transition(next) {
            return Err(StewardError::Invalid(format!(
                "illegal goal transition {:?} -> {:?}",
                goal.status, next
            )));
        }
        let now = now_ms();
        {
            let conn = self.lock()?;
            conn.execute(
                "UPDATE goal SET status=?2, updated_at=?3 WHERE id=?1",
                params![id, status_str(next), now],
            )?;
        }
        self.append_event(id, "status", detail)?;
        self.get(id)?
            .ok_or_else(|| StewardError::Invalid("goal vanished".into()))
    }

    pub fn save_version(&self, mut spec: GoalSpec, reason: &str) -> Result<GoalRecord> {
        let goal = self
            .get(&spec.id)?
            .ok_or_else(|| StewardError::Invalid(format!("goal not found: {}", spec.id)))?;
        if matches!(
            goal.status,
            GoalStatus::Active | GoalStatus::Verifying | GoalStatus::Achieved
        ) {
            return Err(StewardError::Invalid(
                "active/verifying/achieved goal contracts are immutable".into(),
            ));
        }
        spec.version = goal.current_version + 1;
        spec.validate()?;
        let now = now_ms();
        let conn = self.lock()?;
        conn.execute(
            "INSERT INTO goal_version(goal_id,version,spec_json,reason,created_at)
             VALUES (?1,?2,?3,?4,?5)",
            params![
                spec.id,
                spec.version as i64,
                serde_json::to_string(&spec)?,
                reason,
                now
            ],
        )?;
        conn.execute(
            "UPDATE goal SET current_version=?2, spec_json=?3, updated_at=?4 WHERE id=?1",
            params![
                spec.id,
                spec.version as i64,
                serde_json::to_string(&spec)?,
                now
            ],
        )?;
        drop(conn);
        self.append_event(&spec.id, "version", reason)?;
        self.get(&spec.id)?
            .ok_or_else(|| StewardError::Invalid("goal vanished".into()))
    }

    pub fn append_event(&self, goal_id: &str, kind: &str, detail: &str) -> Result<()> {
        let conn = self.lock()?;
        conn.execute(
            "INSERT INTO goal_event(id,goal_id,at,kind,detail) VALUES (?1,?2,?3,?4,?5)",
            params![Uuid::new_v4().to_string(), goal_id, now_ms(), kind, detail],
        )?;
        Ok(())
    }

    pub fn events(&self, goal_id: &str, limit: usize) -> Result<Vec<GoalEvent>> {
        let conn = self.lock()?;
        let mut stmt = conn.prepare(
            "SELECT id,goal_id,at,kind,detail FROM goal_event WHERE goal_id=?1 ORDER BY at DESC LIMIT ?2",
        )?;
        let rows = stmt.query_map(params![goal_id, limit as i64], |r| {
            Ok(GoalEvent {
                id: r.get(0)?,
                goal_id: r.get(1)?,
                at: r.get(2)?,
                kind: r.get(3)?,
                detail: r.get(4)?,
            })
        })?;
        Ok(rows.collect::<std::result::Result<Vec<_>, _>>()?)
    }

    pub fn record_attempt(&self, attempt: &GoalAttempt) -> Result<()> {
        let conn = self.lock()?;
        conn.execute(
            "INSERT OR REPLACE INTO goal_attempt(goal_id,attempt_no,started_at,ended_at,summary,outcome,failure_class,checkpoint_id,tokens,gates_json,evidence_json)
             VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11)",
            params![
                attempt.goal_id,
                attempt.attempt_no as i64,
                attempt.started_at,
                attempt.ended_at,
                attempt.summary,
                attempt.outcome,
                attempt.failure_class,
                attempt.checkpoint_id,
                attempt.tokens as i64,
                serde_json::to_string(&attempt.gates)?,
                serde_json::to_string(&attempt.evidence_ids)?,
            ],
        )?;
        Ok(())
    }

    pub fn attempts(&self, goal_id: &str, limit: usize) -> Result<Vec<GoalAttempt>> {
        let conn = self.lock()?;
        let mut stmt = conn.prepare(
            "SELECT goal_id,attempt_no,started_at,ended_at,summary,outcome,failure_class,checkpoint_id,tokens,gates_json,evidence_json
             FROM goal_attempt WHERE goal_id=?1 ORDER BY attempt_no DESC LIMIT ?2",
        )?;
        let rows = stmt.query_map(params![goal_id, limit as i64], |r| {
            let gates_json: String = r.get(9)?;
            let evidence_json: String = r.get(10)?;
            Ok(GoalAttempt {
                goal_id: r.get(0)?,
                attempt_no: r.get::<_, i64>(1)? as u32,
                started_at: r.get(2)?,
                ended_at: r.get(3)?,
                summary: r.get(4)?,
                outcome: r.get(5)?,
                failure_class: r.get(6)?,
                checkpoint_id: r.get(7)?,
                tokens: r.get::<_, i64>(8)? as u64,
                gates: serde_json::from_str(&gates_json).unwrap_or_default(),
                evidence_ids: serde_json::from_str(&evidence_json).unwrap_or_default(),
            })
        })?;
        Ok(rows.collect::<std::result::Result<Vec<_>, _>>()?)
    }

    pub fn update_counters(
        &self,
        id: &str,
        attempts: u32,
        failures: u32,
        no_progress: u32,
        tokens_used: u64,
        wall_started_at: Option<i64>,
    ) -> Result<()> {
        let conn = self.lock()?;
        conn.execute(
            "UPDATE goal SET attempts=?2, consecutive_failures=?3, no_progress_rounds=?4, tokens_used=?5, wall_started_at=?6, updated_at=?7 WHERE id=?1",
            params![
                id,
                attempts as i64,
                failures as i64,
                no_progress as i64,
                tokens_used as i64,
                wall_started_at,
                now_ms()
            ],
        )?;
        Ok(())
    }

    pub fn acquire_lease(&self, goal_id: &str, runner_id: &str, ttl_ms: i64) -> Result<bool> {
        let now = now_ms();
        let conn = self.lock()?;
        let existing: Option<(String, i64)> = conn
            .query_row(
                "SELECT runner_id,expires_at FROM goal_lease WHERE goal_id=?1",
                params![goal_id],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .optional()?;
        if let Some((owner, expires)) = existing {
            if owner != runner_id && expires > now {
                return Ok(false);
            }
        }
        conn.execute(
            "INSERT OR REPLACE INTO goal_lease(goal_id,runner_id,heartbeat_at,expires_at) VALUES (?1,?2,?3,?4)",
            params![goal_id, runner_id, now, now + ttl_ms.max(1)],
        )?;
        Ok(true)
    }

    pub fn heartbeat_lease(&self, goal_id: &str, runner_id: &str, ttl_ms: i64) -> Result<bool> {
        let now = now_ms();
        let conn = self.lock()?;
        let n = conn.execute(
            "UPDATE goal_lease SET heartbeat_at=?3, expires_at=?4 WHERE goal_id=?1 AND runner_id=?2",
            params![goal_id, runner_id, now, now + ttl_ms.max(1)],
        )?;
        Ok(n > 0)
    }

    pub fn release_lease(&self, goal_id: &str, runner_id: &str) -> Result<()> {
        let conn = self.lock()?;
        conn.execute(
            "DELETE FROM goal_lease WHERE goal_id=?1 AND runner_id=?2",
            params![goal_id, runner_id],
        )?;
        Ok(())
    }

    /// Expired active/verifying goals become paused(interrupted), never
    /// silently active. Returns the affected ids.
    pub fn sweep_expired_leases(&self) -> Result<Vec<String>> {
        let now = now_ms();
        let ids: Vec<String> = {
            let conn = self.lock()?;
            let mut stmt = conn.prepare(
                "SELECT g.id FROM goal g JOIN goal_lease l ON l.goal_id=g.id
                 WHERE g.status IN ('active','verifying') AND l.expires_at < ?1",
            )?;
            let rows = stmt.query_map(params![now], |r| r.get::<_, String>(0))?;
            rows.collect::<std::result::Result<Vec<_>, _>>()?
        };
        for id in &ids {
            let _ = self.set_status(id, GoalStatus::Paused, "interrupted: lease expired");
            let _ = self.release_lease(id, "");
            if let Ok(conn) = self.lock() {
                let _ = conn.execute("DELETE FROM goal_lease WHERE goal_id=?1", params![id]);
            }
        }
        Ok(ids)
    }

    pub fn save_report(&self, report: &GoalReport) -> Result<()> {
        let conn = self.lock()?;
        conn.execute(
            "INSERT OR REPLACE INTO goal_report(id,goal_id,at,status,body_json) VALUES (?1,?2,?3,?4,?5)",
            params![
                report.id,
                report.goal_id,
                report.created_at,
                status_str(report.status),
                serde_json::to_string(report)?
            ],
        )?;
        conn.execute(
            "UPDATE goal SET last_report_id=?2 WHERE id=?1",
            params![report.goal_id, report.id],
        )?;
        Ok(())
    }

    pub fn latest_report(&self, goal_id: &str) -> Result<Option<GoalReport>> {
        let conn = self.lock()?;
        let body: Option<String> = conn
            .query_row(
                "SELECT body_json FROM goal_report WHERE goal_id=?1 ORDER BY at DESC LIMIT 1",
                params![goal_id],
                |r| r.get(0),
            )
            .optional()?;
        body.map(|b| serde_json::from_str(&b).map_err(StewardError::from))
            .transpose()
    }
}

fn parse_status(s: &str) -> Result<GoalStatus> {
    Ok(match s {
        "draft" => GoalStatus::Draft,
        "active" => GoalStatus::Active,
        "paused" => GoalStatus::Paused,
        "verifying" => GoalStatus::Verifying,
        "blocked" => GoalStatus::Blocked,
        "achieved" => GoalStatus::Achieved,
        "exhausted" => GoalStatus::Exhausted,
        "abandoned" => GoalStatus::Abandoned,
        _ => return Err(StewardError::Invalid(format!("bad goal status: {s}"))),
    })
}

fn status_str(s: GoalStatus) -> &'static str {
    match s {
        GoalStatus::Draft => "draft",
        GoalStatus::Active => "active",
        GoalStatus::Paused => "paused",
        GoalStatus::Verifying => "verifying",
        GoalStatus::Blocked => "blocked",
        GoalStatus::Achieved => "achieved",
        GoalStatus::Exhausted => "exhausted",
        GoalStatus::Abandoned => "abandoned",
    }
}

/// Unused import guard for ticket status; goals intentionally share the same
/// terminal vocabulary so the Steward UI can display both.
pub fn ticket_status_for(goal: GoalStatus) -> TicketStatus {
    match goal {
        GoalStatus::Draft | GoalStatus::Paused => TicketStatus::Blocked,
        GoalStatus::Active | GoalStatus::Verifying => TicketStatus::Active,
        GoalStatus::Blocked => TicketStatus::Blocked,
        GoalStatus::Achieved => TicketStatus::Achieved,
        GoalStatus::Exhausted => TicketStatus::Exhausted,
        GoalStatus::Abandoned => TicketStatus::Abandoned,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn spec(id: &str) -> GoalSpec {
        let mut s = GoalSpec::new(id, "title", "objective");
        s.gates = vec![GoalGate::Human {
            prompt: "ok?".into(),
        }];
        s
    }

    #[test]
    fn create_and_transition() {
        let store = GoalStore::open_memory().unwrap();
        let g = store.create(spec("g1")).unwrap();
        assert_eq!(g.status, GoalStatus::Draft);
        let g = store.set_status("g1", GoalStatus::Active, "start").unwrap();
        assert_eq!(g.status, GoalStatus::Active);
        assert!(store
            .set_status("g1", GoalStatus::Draft, "backwards")
            .is_err());
    }

    #[test]
    fn immutable_while_active() {
        let store = GoalStore::open_memory().unwrap();
        store.create(spec("g1")).unwrap();
        store.set_status("g1", GoalStatus::Active, "start").unwrap();
        let mut next = spec("g1");
        next.title = "changed".into();
        assert!(store.save_version(next, "edit").is_err());
    }

    #[test]
    fn duplicate_lease_rejected_then_expiry_recovers() {
        let store = GoalStore::open_memory().unwrap();
        store.create(spec("g1")).unwrap();
        store.set_status("g1", GoalStatus::Active, "start").unwrap();
        assert!(store.acquire_lease("g1", "a", 60_000).unwrap());
        assert!(!store.acquire_lease("g1", "b", 60_000).unwrap());
        let conn = store.lock().unwrap();
        conn.execute("UPDATE goal_lease SET expires_at=1 WHERE goal_id='g1'", [])
            .unwrap();
        drop(conn);
        let ids = store.sweep_expired_leases().unwrap();
        assert_eq!(ids, vec!["g1"]);
        assert_eq!(store.get("g1").unwrap().unwrap().status, GoalStatus::Paused);
    }

    #[test]
    fn attempt_and_report_roundtrip() {
        let store = GoalStore::open_memory().unwrap();
        store.create(spec("g1")).unwrap();
        let a = GoalAttempt {
            goal_id: "g1".into(),
            attempt_no: 1,
            started_at: 1,
            ended_at: 2,
            summary: "sum".into(),
            outcome: "failed".into(),
            failure_class: Some("gate".into()),
            checkpoint_id: None,
            tokens: 10,
            gates: vec![],
            evidence_ids: vec!["s1".into()],
        };
        store.record_attempt(&a).unwrap();
        assert_eq!(store.attempts("g1", 10).unwrap().len(), 1);
        let r = GoalReport {
            id: "r1".into(),
            goal_id: "g1".into(),
            status: GoalStatus::Blocked,
            attempts: 1,
            gates: vec![],
            evidence_ids: vec!["s1".into()],
            summary: "sum".into(),
            next_actions: vec!["act".into()],
            created_at: 3,
        };
        store.save_report(&r).unwrap();
        assert_eq!(store.latest_report("g1").unwrap().unwrap().id, "r1");
    }
}
