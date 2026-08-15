//! FTS5 full-text index over distilled memories (P2-7).
//!
//! Indexes `memories(session_id, category, content, created_at)` in SQLite
//! with an FTS5 virtual table using the `trigram` tokenizer — CJK-safe:
//! Chinese text matches on any 3-character window, English on 3-letter
//! substrings. Queries shorter than 3 chars fall back to a LIKE scan.

use std::path::Path;
use std::sync::Mutex;

use rusqlite::{params, Connection};

/// One search hit.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FtsHit {
    pub session_id: String,
    pub category: String,
    pub content: String,
}

/// SQLite-backed FTS5 memory index. `Connection` is Send but not Sync, so
/// the index wraps it in a `Mutex` to stay usable from async tool contexts.
pub struct MemoryFts {
    conn: Mutex<Connection>,
}

/// Shared DDL for both on-disk and in-memory indexes.
const SCHEMA: &str = "
    CREATE TABLE memories (
        id INTEGER PRIMARY KEY AUTOINCREMENT,
        session_id TEXT NOT NULL,
        category TEXT NOT NULL,
        content TEXT NOT NULL,
        created_at TEXT NOT NULL DEFAULT (datetime('now'))
    );
    CREATE VIRTUAL TABLE memories_fts USING fts5(
        content, category, session_id,
        tokenize = 'trigram'
    );
";

impl MemoryFts {
    /// Open (or create) the index at `path`.
    pub fn open(path: &Path) -> Result<Self, rusqlite::Error> {
        let conn = Connection::open(path)?;
        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS memories (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                session_id TEXT NOT NULL,
                category TEXT NOT NULL,
                content TEXT NOT NULL,
                created_at TEXT NOT NULL DEFAULT (datetime('now'))
            );
            CREATE VIRTUAL TABLE IF NOT EXISTS memories_fts USING fts5(
                content, category, session_id,
                tokenize = 'trigram'
            );",
        )?;
        Ok(MemoryFts {
            conn: Mutex::new(conn),
        })
    }

    /// In-memory index (used by tests and ephemeral sessions).
    pub fn open_in_memory() -> Result<Self, rusqlite::Error> {
        let conn = Connection::open_in_memory()?;
        conn.execute_batch(SCHEMA)?;
        Ok(MemoryFts {
            conn: Mutex::new(conn),
        })
    }

    /// Index one memory item. `category` is searchable too.
    pub fn insert(
        &self,
        session_id: &str,
        category: &str,
        content: &str,
    ) -> Result<(), rusqlite::Error> {
        let guard = self.conn.lock().unwrap();
        let tx = guard.unchecked_transaction()?;
        tx.execute(
            "INSERT INTO memories (session_id, category, content) VALUES (?1, ?2, ?3)",
            params![session_id, category, content],
        )?;
        tx.execute(
            "INSERT INTO memories_fts (rowid, content, category, session_id)
             VALUES (last_insert_rowid(), ?1, ?2, ?3)",
            params![content, category, session_id],
        )?;
        tx.commit()
    }

    /// Full-text search. `query` is tokenized into 3-char+ phrases joined
    /// with AND (FTS5 implicit-AND semantics); short queries (< 3 chars)
    /// degrade to a LIKE substring scan.
    pub fn search(&self, query: &str, limit: usize) -> Result<Vec<FtsHit>, rusqlite::Error> {
        let q = query.trim();
        let tokens: Vec<String> = q
            .split_whitespace()
            .filter(|t| t.chars().count() >= 3)
            .map(|t| format!("\"{}\"", t.replace('"', "\"\"")))
            .collect();
        if tokens.is_empty() {
            return self.search_like(q, limit);
        }
        let match_query = tokens.join(" AND ");
        let guard = self.conn.lock().unwrap();
        let mut stmt = guard.prepare(
            "SELECT m.session_id, m.category, m.content
             FROM memories_fts f JOIN memories m ON m.id = f.rowid
             WHERE memories_fts MATCH ?1
             ORDER BY rank
             LIMIT ?2",
        )?;
        let rows = stmt.query_map(params![match_query, limit as i64], |row| {
            Ok(FtsHit {
                session_id: row.get(0)?,
                category: row.get(1)?,
                content: row.get(2)?,
            })
        })?;
        rows.collect()
    }

    fn search_like(&self, q: &str, limit: usize) -> Result<Vec<FtsHit>, rusqlite::Error> {
        if q.is_empty() {
            return Ok(Vec::new());
        }
        let pattern = format!("%{q}%");
        let guard = self.conn.lock().unwrap();
        let mut stmt = guard.prepare(
            "SELECT session_id, category, content
             FROM memories
             WHERE content LIKE ?1 OR category LIKE ?1
             LIMIT ?2",
        )?;
        let rows = stmt.query_map(params![pattern, limit as i64], |row| {
            Ok(FtsHit {
                session_id: row.get(0)?,
                category: row.get(1)?,
                content: row.get(2)?,
            })
        })?;
        rows.collect()
    }

    /// Total indexed items.
    pub fn count(&self) -> Result<usize, rusqlite::Error> {
        let guard = self.conn.lock().unwrap();
        let n: i64 = guard.query_row("SELECT count(*) FROM memories", [], |row| row.get(0))?;
        Ok(n as usize)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_insert_and_search_english() {
        let fts = MemoryFts::open_in_memory().unwrap();
        fts.insert("s1", "fact", "the quick brown fox").unwrap();
        let hits = fts.search("quick", 10).unwrap();
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].content, "the quick brown fox");
        assert_eq!(hits[0].category, "fact");
    }

    #[test]
    fn test_cjk_trigram_search() {
        let fts = MemoryFts::open_in_memory().unwrap();
        fts.insert("s1", "insight", "用户偏好天青色主题，背景使用暖白")
            .unwrap();
        let hits = fts.search("天青色", 10).unwrap();
        assert_eq!(hits.len(), 1, "3-char CJK query must hit via trigram");
    }

    #[test]
    fn test_short_query_falls_back_to_like() {
        let fts = MemoryFts::open_in_memory().unwrap();
        fts.insert("s1", "fact", "alpha beta gamma").unwrap();
        let hits = fts.search("ga", 10).unwrap();
        assert_eq!(hits.len(), 1, "2-char query must hit via LIKE");
        assert_eq!(hits[0].content, "alpha beta gamma");
    }

    #[test]
    fn test_empty_query_returns_nothing() {
        let fts = MemoryFts::open_in_memory().unwrap();
        fts.insert("s1", "fact", "alpha").unwrap();
        assert!(fts.search("", 10).unwrap().is_empty());
        assert!(fts.search("   ", 10).unwrap().is_empty());
    }

    #[test]
    fn test_ranking_prefers_exact_content() {
        let fts = MemoryFts::open_in_memory().unwrap();
        fts.insert("s1", "fact", "rust is a systems language")
            .unwrap();
        fts.insert("s2", "fact", "rust cargo build is slow")
            .unwrap();
        let hits = fts.search("rust systems", 10).unwrap();
        assert!(!hits.is_empty());
        assert_eq!(hits[0].content, "rust is a systems language");
    }

    #[test]
    fn test_count() {
        let fts = MemoryFts::open_in_memory().unwrap();
        assert_eq!(fts.count().unwrap(), 0);
        fts.insert("s1", "fact", "one").unwrap();
        fts.insert("s2", "insight", "two").unwrap();
        assert_eq!(fts.count().unwrap(), 2);
    }

    #[test]
    fn test_persisted_index_reopens() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("memory_fts.sqlite");
        {
            let fts = MemoryFts::open(&path).unwrap();
            fts.insert("s1", "fact", "persisted memory").unwrap();
        }
        let fts = MemoryFts::open(&path).unwrap();
        let hits = fts.search("persisted", 10).unwrap();
        assert_eq!(hits.len(), 1);
    }
}
