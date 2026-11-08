//! SQLite-backed flight recorder.
//!
//! One connection behind a mutex is enough for the desktop app: writes happen
//! at tool/turn boundaries, not per token. WAL keeps reads from blocking the
//! agent loop. The model never gets a handle to this store.

use std::path::Path;
use std::sync::Mutex;

use oz_core_types::supervision::{
    now_ms, InterventionRecord, JudgmentRecord, Phase, StepRecord, StepStatus, TicketSnapshot,
    TicketStatus, ToolRecord, Verdict,
};
use rusqlite::{params, Connection, OptionalExtension, Row};

use crate::contract::TaskContract;
use crate::error::{Result, StewardError};
use crate::report::Report;

const SCHEMA: &str = r#"
PRAGMA journal_mode = WAL;
PRAGMA synchronous = NORMAL;
PRAGMA foreign_keys = ON;

CREATE TABLE IF NOT EXISTS ticket (
    id TEXT PRIMARY KEY,
    session_id TEXT NOT NULL,
    title TEXT NOT NULL,
    status TEXT NOT NULL,
    phase TEXT NOT NULL,
    created_at INTEGER NOT NULL,
    updated_at INTEGER NOT NULL,
    heartbeat_at INTEGER,
    last_verdict TEXT,
    terminal_at INTEGER
);
CREATE INDEX IF NOT EXISTS ticket_session ON ticket(session_id, updated_at DESC);
CREATE INDEX IF NOT EXISTS ticket_status ON ticket(status, updated_at DESC);

CREATE TABLE IF NOT EXISTS step (
    id TEXT PRIMARY KEY,
    ticket_id TEXT NOT NULL,
    session_id TEXT NOT NULL,
    turn INTEGER NOT NULL,
    phase TEXT NOT NULL,
    at INTEGER NOT NULL,
    tool_json TEXT,
    status TEXT NOT NULL,
    artifacts_json TEXT NOT NULL,
    targets_json TEXT NOT NULL DEFAULT '[]',
    plan_hash TEXT,
    git_sha TEXT,
    tokens_in INTEGER NOT NULL DEFAULT 0,
    tokens_out INTEGER NOT NULL DEFAULT 0
);
CREATE INDEX IF NOT EXISTS step_ticket_at ON step(ticket_id, at);

CREATE TABLE IF NOT EXISTS judgment (
    id TEXT PRIMARY KEY,
    ticket_id TEXT NOT NULL,
    at INTEGER NOT NULL,
    provider TEXT NOT NULL,
    question TEXT NOT NULL,
    verdict TEXT NOT NULL,
    confidence REAL NOT NULL,
    evidence_json TEXT NOT NULL,
    action TEXT
);
CREATE INDEX IF NOT EXISTS judgment_ticket_at ON judgment(ticket_id, at);

CREATE TABLE IF NOT EXISTS intervention (
    id TEXT PRIMARY KEY,
    ticket_id TEXT NOT NULL,
    at INTEGER NOT NULL,
    level TEXT NOT NULL,
    reason TEXT NOT NULL,
    evidence_json TEXT NOT NULL,
    result TEXT NOT NULL
);
CREATE INDEX IF NOT EXISTS intervention_ticket_at ON intervention(ticket_id, at);

CREATE TABLE IF NOT EXISTS heartbeat (
    ticket_id TEXT NOT NULL,
    at INTEGER NOT NULL,
    source TEXT NOT NULL,
    PRIMARY KEY(ticket_id, at, source)
);

CREATE TABLE IF NOT EXISTS report (
    id TEXT PRIMARY KEY,
    ticket_id TEXT NOT NULL,
    at INTEGER NOT NULL,
    status TEXT NOT NULL,
    body TEXT NOT NULL,
    evidence_json TEXT NOT NULL
);

CREATE TABLE IF NOT EXISTS contract (
    ticket_id TEXT NOT NULL,
    version INTEGER NOT NULL,
    json TEXT NOT NULL,
    created_at INTEGER NOT NULL,
    confirmed INTEGER NOT NULL DEFAULT 0,
    PRIMARY KEY(ticket_id, version)
);
"#;

pub struct Store {
    conn: Mutex<Connection>,
}

impl Store {
    pub fn open(path: impl AsRef<Path>) -> Result<Self> {
        let path = path.as_ref();
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let conn = Connection::open(path)?;
        Self::from_conn(conn)
    }

    pub fn open_memory() -> Result<Self> {
        Self::from_conn(Connection::open_in_memory()?)
    }

    fn from_conn(conn: Connection) -> Result<Self> {
        conn.busy_timeout(std::time::Duration::from_millis(2_000))?;
        conn.execute_batch(SCHEMA)?;
        // Forward migration for databases created before target tracking.
        // A duplicate-column error is expected on fresh/current schemas.
        let _ = conn.execute(
            "ALTER TABLE step ADD COLUMN targets_json TEXT NOT NULL DEFAULT '[]'",
            [],
        );
        Ok(Self {
            conn: Mutex::new(conn),
        })
    }

    fn lock(&self) -> Result<std::sync::MutexGuard<'_, Connection>> {
        self.conn.lock().map_err(|_| StewardError::Lock)
    }

    #[cfg(test)]
    pub(crate) fn lock_for_test(&self) -> std::sync::MutexGuard<'_, Connection> {
        self.conn.lock().expect("store lock")
    }

    /// Test-only hook for the fault-injection suite (integration tests cannot
    /// reach the private connection). Not part of the product API.
    #[doc(hidden)]
    pub fn set_times_for_test(
        &self,
        ticket_id: &str,
        heartbeat_at: i64,
        updated_at: i64,
    ) -> Result<()> {
        let conn = self.lock()?;
        conn.execute(
            "UPDATE ticket SET heartbeat_at=?2, updated_at=?3 WHERE id=?1",
            params![ticket_id, heartbeat_at, updated_at],
        )?;
        Ok(())
    }

    /// Create the ticket if it does not exist; never reopen a terminal one.
    pub fn ensure_ticket(&self, id: &str, session_id: &str, title: &str) -> Result<()> {
        let now = now_ms();
        let conn = self.lock()?;
        conn.execute(
            "INSERT INTO ticket
             (id, session_id, title, status, phase, created_at, updated_at, heartbeat_at)
             VALUES (?1, ?2, ?3, 'active', 'planning', ?4, ?4, ?4)
             ON CONFLICT(id) DO UPDATE SET
               title = CASE WHEN ticket.title = '' THEN excluded.title ELSE ticket.title END",
            params![id, session_id, title, now],
        )?;
        Ok(())
    }

    pub fn set_status(
        &self,
        ticket_id: &str,
        status: TicketStatus,
        phase: Phase,
        verdict: Option<Verdict>,
    ) -> Result<()> {
        let now = now_ms();
        let terminal_at = if status.is_terminal() {
            Some(now)
        } else {
            None
        };
        let conn = self.lock()?;
        conn.execute(
            "UPDATE ticket SET status=?2, phase=?3, updated_at=?4,
                terminal_at=COALESCE(?5, terminal_at),
                last_verdict=COALESCE(?6, last_verdict)
             WHERE id=?1",
            params![
                ticket_id,
                status_str(status),
                phase_str(phase),
                now,
                terminal_at,
                verdict.map(verdict_str)
            ],
        )?;
        Ok(())
    }

    pub fn set_phase(&self, ticket_id: &str, phase: Phase) -> Result<()> {
        let conn = self.lock()?;
        conn.execute(
            "UPDATE ticket SET phase=?2, updated_at=?3 WHERE id=?1",
            params![ticket_id, phase_str(phase), now_ms()],
        )?;
        Ok(())
    }

    pub fn heartbeat(&self, ticket_id: &str, source: &str) -> Result<()> {
        let now = now_ms();
        let conn = self.lock()?;
        conn.execute(
            "UPDATE ticket SET heartbeat_at=?2, updated_at=?2 WHERE id=?1",
            params![ticket_id, now],
        )?;
        conn.execute(
            "INSERT OR IGNORE INTO heartbeat(ticket_id, at, source) VALUES (?1, ?2, ?3)",
            params![ticket_id, now, source],
        )?;
        Ok(())
    }

    /// Append an immutable step and advance the ticket's phase/heartbeat.
    pub fn record_step(&self, step: &StepRecord) -> Result<()> {
        let tool_json = match &step.tool {
            Some(t) => Some(serde_json::to_string(t)?),
            None => None,
        };
        let artifacts_json = serde_json::to_string(&step.artifacts)?;
        let targets_json = serde_json::to_string(&step.targets)?;
        let conn = self.lock()?;
        conn.execute(
            "INSERT OR IGNORE INTO step
             (id, ticket_id, session_id, turn, phase, at, tool_json, status,
              artifacts_json, targets_json, plan_hash, git_sha, tokens_in, tokens_out)
             VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14)",
            params![
                step.id,
                step.ticket_id,
                step.session_id,
                step.turn,
                phase_str(step.phase),
                step.at,
                tool_json,
                step_status_str(step.status),
                artifacts_json,
                targets_json,
                step.plan_hash,
                step.git_sha,
                step.tokens_in as i64,
                step.tokens_out as i64,
            ],
        )?;
        conn.execute(
            "UPDATE ticket SET updated_at=?2, heartbeat_at=?2,
                phase=CASE WHEN status IN ('completed','achieved','abandoned','failed') THEN phase ELSE ?3 END
             WHERE id=?1",
            params![step.ticket_id, step.at, phase_str(step.phase)],
        )?;
        Ok(())
    }

    pub fn record_judgment(&self, j: &JudgmentRecord) -> Result<()> {
        let conn = self.lock()?;
        conn.execute(
            "INSERT OR REPLACE INTO judgment
             (id,ticket_id,at,provider,question,verdict,confidence,evidence_json,action)
             VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9)",
            params![
                j.id,
                j.ticket_id,
                j.at,
                j.provider,
                j.question,
                verdict_str(j.verdict),
                j.confidence,
                serde_json::to_string(&j.evidence_ids)?,
                j.action,
            ],
        )?;
        conn.execute(
            "UPDATE ticket SET updated_at=?2, last_verdict=?3 WHERE id=?1",
            params![j.ticket_id, j.at, verdict_str(j.verdict)],
        )?;
        Ok(())
    }

    pub fn record_intervention(&self, i: &InterventionRecord) -> Result<()> {
        let conn = self.lock()?;
        conn.execute(
            "INSERT OR REPLACE INTO intervention
             (id,ticket_id,at,level,reason,evidence_json,result)
             VALUES (?1,?2,?3,?4,?5,?6,?7)",
            params![
                i.id,
                i.ticket_id,
                i.at,
                i.level,
                i.reason,
                serde_json::to_string(&i.evidence_ids)?,
                i.result,
            ],
        )?;
        Ok(())
    }

    pub fn snapshot(&self, ticket_id: &str) -> Result<Option<TicketSnapshot>> {
        let conn = self.lock()?;
        snapshot_one(&conn, ticket_id)
    }

    pub fn snapshots(&self, limit: usize) -> Result<Vec<TicketSnapshot>> {
        let conn = self.lock()?;
        let mut stmt = conn.prepare(
            "SELECT t.id,t.session_id,t.title,t.status,t.phase,t.created_at,t.updated_at,
                    t.heartbeat_at,t.last_verdict,
                    (SELECT COUNT(*) FROM step s WHERE s.ticket_id=t.id),
                    (SELECT MAX(at) FROM step s WHERE s.ticket_id=t.id)
             FROM ticket t ORDER BY t.updated_at DESC LIMIT ?1",
        )?;
        let rows = stmt.query_map(params![limit as i64], snapshot_row)?;
        rows.collect::<rusqlite::Result<Vec<_>>>()
            .map_err(StewardError::from)
    }

    pub fn inbox(&self) -> Result<Vec<TicketSnapshot>> {
        let conn = self.lock()?;
        let mut stmt = conn.prepare(
            "SELECT t.id,t.session_id,t.title,t.status,t.phase,t.created_at,t.updated_at,
                    t.heartbeat_at,t.last_verdict,
                    (SELECT COUNT(*) FROM step s WHERE s.ticket_id=t.id),
                    (SELECT MAX(at) FROM step s WHERE s.ticket_id=t.id)
             FROM ticket t
             WHERE t.status IN ('blocked','stalled','exhausted')
             ORDER BY t.updated_at DESC LIMIT 50",
        )?;
        let rows = stmt.query_map([], snapshot_row)?;
        rows.collect::<rusqlite::Result<Vec<_>>>()
            .map_err(StewardError::from)
    }

    pub fn steps(&self, ticket_id: &str, limit: usize) -> Result<Vec<StepRecord>> {
        let conn = self.lock()?;
        let mut stmt = conn.prepare(
            "SELECT id,ticket_id,session_id,turn,phase,at,tool_json,status,
                    artifacts_json,targets_json,plan_hash,git_sha,tokens_in,tokens_out
             FROM step WHERE ticket_id=?1 ORDER BY at DESC LIMIT ?2",
        )?;
        let rows = stmt.query_map(params![ticket_id, limit as i64], step_row)?;
        rows.collect::<rusqlite::Result<Vec<_>>>()
            .map_err(StewardError::from)
    }

    /// Mark active tickets whose last heartbeat is older than `stale_ms`.
    /// Returns the tickets that were transitioned, so the caller can report
    /// or notify without re-querying.
    pub fn sweep_stalled(&self, now: i64, stale_ms: i64) -> Result<Vec<TicketSnapshot>> {
        let cutoff = now.saturating_sub(stale_ms.max(1));
        let conn = self.lock()?;
        let mut stmt = conn.prepare(
            "SELECT id FROM ticket
             WHERE status IN ('active','verifying')
               AND COALESCE(heartbeat_at, updated_at) < ?1",
        )?;
        let ids: Vec<String> = stmt
            .query_map(params![cutoff], |r| r.get(0))?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        drop(stmt);
        let mut out = Vec::new();
        for id in ids {
            conn.execute(
                "UPDATE ticket SET status='stalled', updated_at=?2, last_verdict='stalled'
                 WHERE id=?1 AND status IN ('active','verifying')",
                params![id, now],
            )?;
            if let Some(s) = snapshot_one(&conn, &id)? {
                out.push(s);
            }
        }
        Ok(out)
    }

    /// Count tickets with no terminal state and no recent heartbeat. The
    /// Watchdog uses this as the invariant check.
    pub fn unended_count(&self, now: i64, stale_ms: i64) -> Result<u64> {
        let cutoff = now.saturating_sub(stale_ms.max(1));
        let conn = self.lock()?;
        let n: i64 = conn.query_row(
            "SELECT COUNT(*) FROM ticket
             WHERE status IN ('active','verifying')
               AND COALESCE(heartbeat_at, updated_at) >= ?1",
            params![cutoff],
            |r| r.get(0),
        )?;
        Ok(n.max(0) as u64)
    }

    /// Insert a contract version. Same version + same JSON is idempotent;
    /// same version + different JSON is rejected (contracts are immutable).
    pub fn save_contract(&self, contract: &TaskContract) -> Result<()> {
        contract.validate()?;
        let json = serde_json::to_string(contract)?;
        let conn = self.lock()?;
        let existing: Option<String> = conn
            .query_row(
                "SELECT json FROM contract WHERE ticket_id=?1 AND version=?2",
                params![contract.ticket_id, contract.version],
                |r| r.get(0),
            )
            .optional()?;
        if let Some(existing) = existing {
            if existing != json {
                return Err(StewardError::Invalid(
                    "contract version already exists with different content".into(),
                ));
            }
            return Ok(());
        }
        conn.execute(
            "INSERT INTO contract(ticket_id,version,json,created_at,confirmed)
             VALUES (?1,?2,?3,?4,?5)",
            params![
                contract.ticket_id,
                contract.version,
                json,
                contract.created_at,
                contract.confirmed as i64
            ],
        )?;
        Ok(())
    }

    pub fn save_report(&self, report: &Report) -> Result<()> {
        let body = serde_json::to_string(report)?;
        let evidence = serde_json::to_string(&report.evidence_ids)?;
        let conn = self.lock()?;
        conn.execute(
            "INSERT OR REPLACE INTO report(id,ticket_id,at,status,body,evidence_json)
             VALUES (?1,?2,?3,?4,?5,?6)",
            params![
                report.id,
                report.ticket_id,
                report.created_at,
                format!("{:?}", report.status),
                body,
                evidence
            ],
        )?;
        Ok(())
    }

    pub fn latest_report(&self, ticket_id: &str) -> Result<Option<Report>> {
        let conn = self.lock()?;
        let body: Option<String> = conn
            .query_row(
                "SELECT body FROM report WHERE ticket_id=?1 ORDER BY at DESC LIMIT 1",
                params![ticket_id],
                |r| r.get(0),
            )
            .optional()?;
        body.map(|b| serde_json::from_str::<Report>(&b).map_err(StewardError::from))
            .transpose()
    }

    pub fn latest_contract(&self, ticket_id: &str) -> Result<Option<TaskContract>> {
        let conn = self.lock()?;
        let row: Option<(String, i64)> = conn
            .query_row(
                "SELECT json,confirmed FROM contract WHERE ticket_id=?1
                 ORDER BY version DESC LIMIT 1",
                params![ticket_id],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .optional()?;
        row.map(|(json, confirmed)| {
            let mut contract: TaskContract = serde_json::from_str(&json)?;
            contract.confirmed = confirmed != 0;
            Ok(contract)
        })
        .transpose()
    }

    pub fn last_steps(&self, ticket_id: &str, limit: usize) -> Result<Vec<StepRecord>> {
        self.steps(ticket_id, limit)
    }
}

fn snapshot_one(conn: &Connection, ticket_id: &str) -> Result<Option<TicketSnapshot>> {
    conn.query_row(
        "SELECT t.id,t.session_id,t.title,t.status,t.phase,t.created_at,t.updated_at,
                t.heartbeat_at,t.last_verdict,
                (SELECT COUNT(*) FROM step s WHERE s.ticket_id=t.id),
                (SELECT MAX(at) FROM step s WHERE s.ticket_id=t.id)
         FROM ticket t WHERE t.id=?1",
        params![ticket_id],
        snapshot_row,
    )
    .optional()
    .map_err(StewardError::from)
}

fn snapshot_row(row: &Row<'_>) -> rusqlite::Result<TicketSnapshot> {
    let status: String = row.get(3)?;
    let phase: String = row.get(4)?;
    let verdict: Option<String> = row.get(8)?;
    Ok(TicketSnapshot {
        id: row.get(0)?,
        session_id: row.get(1)?,
        title: row.get(2)?,
        status: parse_status(&status),
        phase: parse_phase(&phase),
        created_at: row.get(5)?,
        updated_at: row.get(6)?,
        heartbeat_at: row.get(7)?,
        last_verdict: verdict.as_deref().map(parse_verdict),
        step_count: row.get::<_, i64>(9)?.max(0) as u64,
        last_step_at: row.get(10)?,
    })
}

fn step_row(row: &Row<'_>) -> rusqlite::Result<StepRecord> {
    let phase: String = row.get(4)?;
    let status: String = row.get(7)?;
    let tool_json: Option<String> = row.get(6)?;
    let artifacts_json: String = row.get(8)?;
    let targets_json: String = row.get(9)?;
    Ok(StepRecord {
        id: row.get(0)?,
        ticket_id: row.get(1)?,
        session_id: row.get(2)?,
        turn: row.get::<_, i64>(3)?.max(0) as u32,
        phase: parse_phase(&phase),
        at: row.get(5)?,
        tool: tool_json
            .as_deref()
            .and_then(|s| serde_json::from_str::<ToolRecord>(s).ok()),
        status: parse_step_status(&status),
        artifacts: serde_json::from_str(&artifacts_json).unwrap_or_default(),
        targets: serde_json::from_str(&targets_json).unwrap_or_default(),
        plan_hash: row.get(10)?,
        git_sha: row.get(11)?,
        tokens_in: row.get::<_, i64>(12)?.max(0) as u64,
        tokens_out: row.get::<_, i64>(13)?.max(0) as u64,
    })
}

fn status_str(s: TicketStatus) -> &'static str {
    match s {
        TicketStatus::Draft => "draft",
        TicketStatus::Active => "active",
        TicketStatus::Verifying => "verifying",
        TicketStatus::Blocked => "blocked",
        TicketStatus::Stalled => "stalled",
        TicketStatus::Completed => "completed",
        TicketStatus::Achieved => "achieved",
        TicketStatus::Exhausted => "exhausted",
        TicketStatus::Abandoned => "abandoned",
        TicketStatus::Failed => "failed",
    }
}

fn parse_status(s: &str) -> TicketStatus {
    match s {
        "draft" => TicketStatus::Draft,
        "verifying" => TicketStatus::Verifying,
        "blocked" => TicketStatus::Blocked,
        "stalled" => TicketStatus::Stalled,
        "completed" => TicketStatus::Completed,
        "achieved" => TicketStatus::Achieved,
        "exhausted" => TicketStatus::Exhausted,
        "abandoned" => TicketStatus::Abandoned,
        "failed" => TicketStatus::Failed,
        _ => TicketStatus::Active,
    }
}

fn phase_str(p: Phase) -> &'static str {
    match p {
        Phase::Planning => "planning",
        Phase::Acting => "acting",
        Phase::Verifying => "verifying",
        Phase::Reporting => "reporting",
        Phase::Done => "done",
    }
}

fn parse_phase(s: &str) -> Phase {
    match s {
        "acting" => Phase::Acting,
        "verifying" => Phase::Verifying,
        "reporting" => Phase::Reporting,
        "done" => Phase::Done,
        _ => Phase::Planning,
    }
}

fn verdict_str(v: Verdict) -> &'static str {
    match v {
        Verdict::OnTrack => "on_track",
        Verdict::AtRisk => "at_risk",
        Verdict::OffTrack => "off_track",
        Verdict::Stalled => "stalled",
        Verdict::Blocked => "blocked",
    }
}

fn parse_verdict(s: &str) -> Verdict {
    match s {
        "at_risk" => Verdict::AtRisk,
        "off_track" => Verdict::OffTrack,
        "stalled" => Verdict::Stalled,
        "blocked" => Verdict::Blocked,
        _ => Verdict::OnTrack,
    }
}

// `StepStatus` is only used for the step's status column; parse_status above
// returns a TicketStatus, so keep the step status conversion separate.
fn step_status_str(s: StepStatus) -> &'static str {
    match s {
        StepStatus::Started => "started",
        StepStatus::Succeeded => "succeeded",
        StepStatus::Failed => "failed",
        StepStatus::Skipped => "skipped",
        StepStatus::Blocked => "blocked",
    }
}

fn parse_step_status(s: &str) -> StepStatus {
    match s {
        "succeeded" => StepStatus::Succeeded,
        "failed" => StepStatus::Failed,
        "skipped" => StepStatus::Skipped,
        "blocked" => StepStatus::Blocked,
        _ => StepStatus::Started,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::contract::{Acceptance, RiskLevel, TaskContract};
    use oz_core_types::supervision::{StepRecord, TicketStatus};

    fn step(id: &str, ticket: &str, at: i64) -> StepRecord {
        StepRecord {
            id: id.into(),
            ticket_id: ticket.into(),
            session_id: "sess".into(),
            turn: 1,
            phase: Phase::Acting,
            at,
            tool: None,
            status: StepStatus::Succeeded,
            artifacts: vec![],
            targets: vec![],
            plan_hash: None,
            git_sha: None,
            tokens_in: 1,
            tokens_out: 1,
        }
    }

    #[test]
    fn ticket_step_and_snapshot_roundtrip() {
        let store = Store::open_memory().unwrap();
        store.ensure_ticket("t1", "sess", "title").unwrap();
        store.record_step(&step("s1", "t1", 10)).unwrap();
        let snap = store.snapshot("t1").unwrap().unwrap();
        assert_eq!(snap.id, "t1");
        assert_eq!(snap.step_count, 1);
        assert_eq!(snap.last_step_at, Some(10));
        assert_eq!(snap.status, TicketStatus::Active);
    }

    #[test]
    fn sweep_marks_stale_active_ticket_stalled() {
        let store = Store::open_memory().unwrap();
        store.ensure_ticket("t1", "sess", "title").unwrap();
        // Ensure the ticket's heartbeat is old: set it explicitly.
        {
            let conn = store.lock().unwrap();
            conn.execute(
                "UPDATE ticket SET heartbeat_at=1, updated_at=1 WHERE id='t1'",
                [],
            )
            .unwrap();
        }
        let swept = store.sweep_stalled(1_000_000, 90_000).unwrap();
        assert_eq!(swept.len(), 1);
        assert_eq!(swept[0].status, TicketStatus::Stalled);
        assert_eq!(
            store.snapshot("t1").unwrap().unwrap().status,
            TicketStatus::Stalled
        );
    }

    #[test]
    fn terminal_ticket_is_not_reopened_by_ensure() {
        let store = Store::open_memory().unwrap();
        store.ensure_ticket("t1", "sess", "title").unwrap();
        store
            .set_status("t1", TicketStatus::Achieved, Phase::Done, None)
            .unwrap();
        store.ensure_ticket("t1", "sess", "title").unwrap();
        assert_eq!(
            store.snapshot("t1").unwrap().unwrap().status,
            TicketStatus::Achieved
        );
    }

    #[test]
    fn contract_versions_are_immutable_and_latest_wins() {
        let store = Store::open_memory().unwrap();
        store.ensure_ticket("t1", "sess", "title").unwrap();
        let c1 = TaskContract::new(
            "t1",
            "goal",
            vec![Acceptance {
                id: "a".into(),
                description: "tests pass".into(),
            }],
            RiskLevel::Low,
            true,
        )
        .unwrap();
        store.save_contract(&c1).unwrap();
        assert_eq!(store.latest_contract("t1").unwrap().unwrap().version, 1);

        let mut changed = c1.clone();
        changed.goal = "different".into();
        assert!(store.save_contract(&changed).is_err());

        let c2 = c1.next_version();
        store.save_contract(&c2).unwrap();
        assert_eq!(store.latest_contract("t1").unwrap().unwrap().version, 2);
    }

    #[test]
    fn judgments_and_interventions_are_persisted() {
        let store = Store::open_memory().unwrap();
        store.ensure_ticket("t1", "sess", "title").unwrap();
        store
            .record_judgment(&JudgmentRecord {
                id: "j1".into(),
                ticket_id: "t1".into(),
                at: 5,
                provider: "rules".into(),
                question: "on_track".into(),
                verdict: Verdict::AtRisk,
                confidence: 0.8,
                evidence_ids: vec!["s1".into()],
                action: None,
            })
            .unwrap();
        store
            .record_intervention(&InterventionRecord {
                id: "i1".into(),
                ticket_id: "t1".into(),
                at: 6,
                level: "L1".into(),
                reason: "test".into(),
                evidence_ids: vec!["s1".into()],
                result: "nudged".into(),
            })
            .unwrap();
        assert_eq!(
            store.snapshot("t1").unwrap().unwrap().last_verdict,
            Some(Verdict::AtRisk)
        );
        let inbox = store.inbox().unwrap();
        assert!(inbox.is_empty()); // at_risk is not an attention status
    }
}
