//! Continual harness state (B2) — durable, reviewable, rollback-able
//! supplemental state updated in small evidence-backed steps.
//!
//! Mirrors Prime Agent's `/refine` design: the base system prompt stays
//! immutable; refinements only touch this ledger (`{data_dir}/harness/
//! harness_state.json`). Every change records a before/after snapshot so
//! any edit can be rolled back.
//!
//! Safety: model-initiated refinements MUST carry non-empty `evidence`
//! (host validates); `rollback` is host/user-only — never exposed to the
//! model, to prevent self-serving drift.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

/// Entry kind in the harness ledger.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HarnessKind {
    Memory,
    SkillNote,
    SubagentSpec,
}

/// One ledger entry.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HarnessEntry {
    pub id: String,
    pub kind: HarnessKind,
    pub content: String,
    /// Why this entry exists (session evidence, observed failure, …).
    pub evidence: String,
    pub created_at: String,
    pub updated_at: String,
}

/// A recorded refinement with before/after snapshots (for rollback).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RefinementRecord {
    pub id: String,
    pub kind: HarnessKind,
    pub entry_id: Option<String>,
    pub before: Option<String>,
    pub after: Option<String>,
    pub reason: String,
    pub ts: String,
}

/// The full harness ledger.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct HarnessState {
    #[serde(default)]
    pub entries: Vec<HarnessEntry>,
    #[serde(default)]
    pub refinements: Vec<RefinementRecord>,
}

impl HarnessState {
    /// Load from `<dir>/harness_state.json`; missing/unreadable → empty.
    pub fn load(dir: &Path) -> Self {
        std::fs::read_to_string(dir.join("harness_state.json"))
            .ok()
            .and_then(|data| serde_json::from_str(&data).ok())
            .unwrap_or_default()
    }

    /// Save atomically (tmp + rename). Returns the file path on success.
    pub fn save(&self, dir: &Path) -> Result<PathBuf, String> {
        let path = dir.join("harness_state.json");
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).map_err(|e| format!("mkdir: {e}"))?;
        }
        let json = serde_json::to_string_pretty(self).map_err(|e| format!("serialize: {e}"))?;
        let tmp = path.with_extension(format!("tmp.{}", std::process::id()));
        std::fs::write(&tmp, &json).map_err(|e| format!("write: {e}"))?;
        if let Err(e) = std::fs::rename(&tmp, &path) {
            if e.raw_os_error() == Some(18) {
                std::fs::copy(&tmp, &path).map_err(|e| format!("copy: {e}"))?;
                let _ = std::fs::remove_file(&tmp);
            } else {
                return Err(format!("rename: {e}"));
            }
        }
        Ok(path)
    }

    /// Apply a small evidence-backed edit. `mode`: "upsert" (create/update by
    /// content match) or "delete". Records a before/after snapshot.
    pub fn apply_refine(
        &mut self,
        kind: HarnessKind,
        content: &str,
        evidence: &str,
        reason: &str,
        mode: &str,
    ) -> Result<RefinementRecord, String> {
        let content = content.trim();
        let evidence = evidence.trim();
        if content.is_empty() {
            return Err("refine rejected: content is empty".into());
        }
        if evidence.is_empty() {
            return Err("refine rejected: evidence is empty (must cite observed behavior)".into());
        }
        let now = chrono::Utc::now().to_rfc3339();

        match mode {
            "delete" => {
                let pos = self.entries.iter().position(|e| e.content == content);
                let Some(pos) = pos else {
                    return Err("refine rejected: no matching entry to delete".into());
                };
                let removed = self.entries.remove(pos);
                let rec = RefinementRecord {
                    id: uuid::Uuid::new_v4().to_string(),
                    kind,
                    entry_id: Some(removed.id.clone()),
                    before: Some(serde_json::to_string(&removed).unwrap_or_default()),
                    after: None,
                    reason: reason.to_string(),
                    ts: now,
                };
                self.refinements.push(rec.clone());
                Ok(rec)
            }
            _ => {
                // Upsert: match by content (stable identity for re-refines).
                let existing = self.entries.iter_mut().find(|e| e.content == content);
                let rec = match existing {
                    Some(entry) => {
                        let before = serde_json::to_string(&*entry).unwrap_or_default();
                        entry.content = content.to_string();
                        entry.evidence = evidence.to_string();
                        entry.updated_at = now.clone();
                        let after = serde_json::to_string(&*entry).unwrap_or_default();
                        let rec = RefinementRecord {
                            id: uuid::Uuid::new_v4().to_string(),
                            kind,
                            entry_id: Some(entry.id.clone()),
                            before: Some(before),
                            after: Some(after),
                            reason: reason.to_string(),
                            ts: now,
                        };
                        self.refinements.push(rec.clone());
                        rec
                    }
                    None => {
                        let entry = HarnessEntry {
                            id: uuid::Uuid::new_v4().to_string(),
                            kind,
                            content: content.to_string(),
                            evidence: evidence.to_string(),
                            created_at: now.clone(),
                            updated_at: now.clone(),
                        };
                        let rec = RefinementRecord {
                            id: uuid::Uuid::new_v4().to_string(),
                            kind,
                            entry_id: Some(entry.id.clone()),
                            before: None,
                            after: Some(serde_json::to_string(&entry).unwrap_or_default()),
                            reason: reason.to_string(),
                            ts: now,
                        };
                        self.entries.push(entry);
                        self.refinements.push(rec.clone());
                        rec
                    }
                };
                Ok(rec)
            }
        }
    }

    /// Roll back a refinement by record id. Host/user only.
    pub fn rollback(&mut self, record_id: &str) -> Result<(), String> {
        let pos = self
            .refinements
            .iter()
            .position(|r| r.id == record_id)
            .ok_or_else(|| format!("no refinement record '{record_id}'"))?;
        let rec = self.refinements[pos].clone();
        match (&rec.before, &rec.after) {
            (Some(_), Some(_)) => {
                // Update: restore the before snapshot (full entry).
                let entry = self
                    .entries
                    .iter_mut()
                    .find(|e| Some(e.id.as_str()) == rec.entry_id.as_deref())
                    .ok_or_else(|| "rollback: entry no longer exists".to_string())?;
                let before: HarnessEntry =
                    serde_json::from_str(rec.before.as_deref().unwrap_or_default())
                        .map_err(|_| "rollback: corrupt before snapshot".to_string())?;
                *entry = before;
                self.refinements.remove(pos);
                Ok(())
            }
            (Some(_), None) => {
                // Delete: restore the removed entry.
                let before: HarnessEntry =
                    serde_json::from_str(rec.before.as_deref().unwrap_or_default())
                        .map_err(|_| "rollback: corrupt before snapshot".to_string())?;
                self.entries.push(before);
                self.refinements.remove(pos);
                Ok(())
            }
            (None, Some(_)) => {
                // Create: remove the created entry.
                let entry_id = rec.entry_id.as_deref().unwrap_or_default();
                let before_len = self.entries.len();
                self.entries.retain(|e| e.id != entry_id);
                if self.entries.len() == before_len {
                    return Err("rollback: created entry not found".to_string());
                }
                self.refinements.remove(pos);
                Ok(())
            }
            _ => Err("rollback: malformed record".to_string()),
        }
    }

    /// Entries of one kind, most recently updated first.
    pub fn entries_of(&self, kind: HarnessKind) -> Vec<&HarnessEntry> {
        let mut v: Vec<&HarnessEntry> = self.entries.iter().filter(|e| e.kind == kind).collect();
        v.sort_by(|a, b| b.updated_at.cmp(&a.updated_at));
        v
    }
}

/// Convenience: load → apply → save in one call.
pub fn refine(
    dir: &Path,
    kind: HarnessKind,
    content: &str,
    evidence: &str,
    reason: &str,
    mode: &str,
) -> Result<RefinementRecord, String> {
    let mut state = HarnessState::load(dir);
    let rec = state.apply_refine(kind, content, evidence, reason, mode)?;
    state.save(dir)?;
    Ok(rec)
}

/// Convenience: load → rollback → save in one call.
pub fn rollback(dir: &Path, record_id: &str) -> Result<(), String> {
    let mut state = HarnessState::load(dir);
    state.rollback(record_id)?;
    state.save(dir).map(|_| ())
}

/// Relevance tokens: lowercase latin words plus individual CJK
/// characters (cheap, dictionary-free).
fn relevance_tokens(s: &str) -> std::collections::HashSet<String> {
    let mut out = std::collections::HashSet::new();
    let mut latin = String::new();
    let flush = |latin: &mut String, out: &mut std::collections::HashSet<String>| {
        if latin.chars().count() >= 3 {
            out.insert(latin.to_lowercase());
        }
        latin.clear();
    };
    for ch in s.chars() {
        let is_cjk = matches!(ch as u32, 0x4E00..=0x9FFF | 0x3400..=0x4DBF);
        if is_cjk {
            flush(&mut latin, &mut out);
            out.insert(ch.to_string());
        } else if ch.is_alphanumeric() {
            latin.push(ch);
        } else {
            flush(&mut latin, &mut out);
        }
    }
    flush(&mut latin, &mut out);
    out
}

fn jaccard(a: &std::collections::HashSet<String>, b: &std::collections::HashSet<String>) -> f32 {
    if a.is_empty() || b.is_empty() {
        return 0.0;
    }
    let inter = a.intersection(b).count() as f32;
    let union = (a.len() + b.len()) as f32 - inter;
    if union <= 0.0 {
        0.0
    } else {
        inter / union
    }
}

/// Like `render_context`, but ranks entries by token relevance to `query`
/// (Jaccard) instead of pure recency. Empty or non-matching queries fall
/// back to the recency order — lessons about the CURRENT task surface
/// first instead of whatever was updated last.
pub fn render_context_relevant(dir: &Path, kind: HarnessKind, limit: usize, query: &str) -> String {
    let state = HarnessState::load(dir);
    render_entries(&state.entries_of(kind), kind, limit, query)
}

/// Round3 P2-p: same rendering with a process-wide 5-minute TTL cache over
/// the ledger file — the runner calls this once per run per session, and
/// re-reading + re-parsing the JSONL every time was pure IO waste for a
/// ledger that changes at most a few times an hour.
pub fn render_context_relevant_cached(
    dir: &Path,
    kind: HarnessKind,
    limit: usize,
    query: &str,
) -> String {
    use std::collections::HashMap;
    use std::sync::{LazyLock, Mutex};
    type LedgerCache = HashMap<PathBuf, (std::time::Instant, Vec<HarnessEntry>)>;
    static CACHE: LazyLock<Mutex<LedgerCache>> = LazyLock::new(|| Mutex::new(HashMap::new()));
    let mut cache = CACHE.lock().unwrap_or_else(|e| e.into_inner());
    let fresh = match cache.get(dir) {
        Some((at, _)) => at.elapsed() < std::time::Duration::from_secs(300),
        None => false,
    };
    if !fresh {
        let state = HarnessState::load(dir);
        let owned: Vec<HarnessEntry> = state.entries_of(kind).into_iter().cloned().collect();
        cache.insert(dir.to_path_buf(), (std::time::Instant::now(), owned));
    }
    // Clone only the small entry vec out so the lock is not held while
    // rendering (rendering is cheap too, but borrowing through the guard
    // across format! calls invites lock-order bugs later).
    let owned = cache.get(dir).map(|(_, e)| e.clone()).unwrap_or_default();
    drop(cache);
    let refs: Vec<&HarnessEntry> = owned.iter().collect();
    render_entries(&refs, kind, limit, query)
}

fn render_entries(
    entries: &[&HarnessEntry],
    kind: HarnessKind,
    limit: usize,
    query: &str,
) -> String {
    let mut entries = entries.to_vec();
    if entries.is_empty() {
        return String::new();
    }
    let q_tokens = relevance_tokens(query);
    if !q_tokens.is_empty() {
        entries.sort_by(|a, b| {
            let sa = jaccard(&relevance_tokens(&a.content), &q_tokens);
            let sb = jaccard(&relevance_tokens(&b.content), &q_tokens);
            sb.partial_cmp(&sa)
                .unwrap_or(std::cmp::Ordering::Equal)
                .then(b.updated_at.cmp(&a.updated_at))
        });
    }
    let kind_name = match kind {
        HarnessKind::Memory => "memory",
        HarnessKind::SkillNote => "skill_note",
        HarnessKind::SubagentSpec => "subagent_spec",
    };
    let mut out = format!("<harness kind=\"{kind_name}\">\n");
    for e in entries.iter().take(limit) {
        out.push_str(&format!("- {}\n", e.content));
    }
    out.push_str("</harness>");
    out
}

/// Render entries as a compact context block (for reminder injection).
pub fn render_context(dir: &Path, kind: HarnessKind, limit: usize) -> String {
    let state = HarnessState::load(dir);
    let entries = state.entries_of(kind);
    if entries.is_empty() {
        return String::new();
    }
    let kind_name = match kind {
        HarnessKind::Memory => "memory",
        HarnessKind::SkillNote => "skill_note",
        HarnessKind::SubagentSpec => "subagent_spec",
    };
    let mut out = format!("<harness kind=\"{kind_name}\">\n");
    for e in entries.iter().take(limit) {
        out.push_str(&format!("- {}\n", e.content));
    }
    out.push_str("</harness>");
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp_dir(tag: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("oz-harness-{}-{tag}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn test_refine_rejects_missing_evidence() {
        let dir = tmp_dir("no-evidence");
        let r = refine(&dir, HarnessKind::Memory, "do X", "", "test", "upsert");
        assert!(r.is_err());
    }

    #[test]
    fn test_refine_create_and_persist() {
        let dir = tmp_dir("create");
        let rec = refine(
            &dir,
            HarnessKind::Memory,
            "retry flaky tests twice",
            "seen 3 flakes this session",
            "observed pattern",
            "upsert",
        )
        .unwrap();
        assert!(rec.before.is_none());
        assert!(rec.after.is_some());
        let state = HarnessState::load(&dir);
        assert_eq!(state.entries.len(), 1);
        assert_eq!(state.refinements.len(), 1);
        assert_eq!(state.entries[0].content, "retry flaky tests twice");
        assert_eq!(state.entries[0].evidence, "seen 3 flakes this session");
    }

    #[test]
    fn test_refine_update_records_before() {
        let dir = tmp_dir("update");
        refine(
            &dir,
            HarnessKind::SkillNote,
            "use flag -X",
            "worked once",
            "test",
            "upsert",
        )
        .unwrap();
        let rec = refine(
            &dir,
            HarnessKind::SkillNote,
            "use flag -X",
            "worked 5 times",
            "updated",
            "upsert",
        )
        .unwrap();
        assert!(
            rec.before
                .as_deref()
                .unwrap_or_default()
                .contains("use flag -X"),
            "before snapshot must capture the prior entry"
        );
        let state = HarnessState::load(&dir);
        assert_eq!(state.entries.len(), 1, "upsert must not duplicate");
        assert_eq!(state.entries[0].evidence, "worked 5 times");
        assert_eq!(state.refinements.len(), 2);
    }

    #[test]
    fn test_rollback_create_removes_entry() {
        let dir = tmp_dir("rb-create");
        let rec = refine(
            &dir,
            HarnessKind::Memory,
            "temp note",
            "evidence here",
            "test",
            "upsert",
        )
        .unwrap();
        assert_eq!(HarnessState::load(&dir).entries.len(), 1);
        rollback(&dir, &rec.id).unwrap();
        let state = HarnessState::load(&dir);
        assert!(state.entries.is_empty());
        assert!(
            state.refinements.is_empty(),
            "rolled-back record must be removed"
        );
    }

    #[test]
    fn test_rollback_update_restores_before() {
        let dir = tmp_dir("rb-update");
        refine(
            &dir,
            HarnessKind::SkillNote,
            "v1 content",
            "e1",
            "t",
            "upsert",
        )
        .unwrap();
        let rec = refine(
            &dir,
            HarnessKind::SkillNote,
            "v1 content",
            "e2",
            "t",
            "upsert",
        )
        .unwrap();
        rollback(&dir, &rec.id).unwrap();
        let state = HarnessState::load(&dir);
        assert_eq!(state.entries.len(), 1);
        assert_eq!(state.entries[0].evidence, "e1");
    }

    #[test]
    fn test_delete_and_rollback_restores() {
        let dir = tmp_dir("delete");
        refine(
            &dir,
            HarnessKind::Memory,
            "obsolete note",
            "e1",
            "t",
            "upsert",
        )
        .unwrap();
        let rec = refine(
            &dir,
            HarnessKind::Memory,
            "obsolete note",
            "e1",
            "cleanup",
            "delete",
        )
        .unwrap();
        assert!(HarnessState::load(&dir).entries.is_empty());
        rollback(&dir, &rec.id).unwrap();
        let state = HarnessState::load(&dir);
        assert_eq!(state.entries.len(), 1);
        assert_eq!(state.entries[0].content, "obsolete note");
    }

    #[test]
    fn test_entries_of_filters_and_sorts() {
        let dir = tmp_dir("filter");
        refine(&dir, HarnessKind::Memory, "first", "e", "t", "upsert").unwrap();
        std::thread::sleep(std::time::Duration::from_millis(10));
        refine(&dir, HarnessKind::Memory, "second", "e", "t", "upsert").unwrap();
        refine(&dir, HarnessKind::SkillNote, "skill", "e", "t", "upsert").unwrap();
        let state = HarnessState::load(&dir);
        let mem = state.entries_of(HarnessKind::Memory);
        assert_eq!(mem.len(), 2);
        assert_eq!(mem[0].content, "second", "most recent first");
    }

    #[test]
    fn test_render_context() {
        let dir = tmp_dir("render");
        refine(&dir, HarnessKind::Memory, "remember me", "e", "t", "upsert").unwrap();
        let ctx = render_context(&dir, HarnessKind::Memory, 5);
        assert!(ctx.contains("remember me"));
        assert!(ctx.starts_with("<harness"));
        assert!(render_context(&dir, HarnessKind::SkillNote, 5).is_empty());
    }

    #[test]
    fn test_bad_json_degrades_empty() {
        let dir = tmp_dir("bad");
        std::fs::write(dir.join("harness_state.json"), "not json").unwrap();
        assert!(HarnessState::load(&dir).entries.is_empty());
    }
}
