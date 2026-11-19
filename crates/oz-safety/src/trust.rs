//! Trust store — progressive trust entries with auto-escalation and decay.
//!
//! Keyed by `(tool_name, arg_pattern)`. Tracks approval/denial counts and
//! auto-escalates trust level based on user behavior.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock, RwLock};
use std::time::Duration;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

const DENIAL_AUTO_BLOCK_COUNT: u32 = 3;
const DENIAL_AUTO_BLOCK_WINDOW: Duration = Duration::from_secs(300);

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum TrustLevel {
    #[serde(rename = "blocked")]
    Blocked,
    #[serde(rename = "always_ask")]
    AlwaysAsk,
    #[serde(rename = "session_trust")]
    SessionTrust,
    #[serde(rename = "workspace_trust")]
    WorkspaceTrust,
    #[serde(rename = "global_trust")]
    GlobalTrust,
}

#[derive(Clone, Debug)]
pub enum TrustDecision {
    Allowed,
    Blocked(String),
    NeedsApproval(ApprovalInfo),
}

#[derive(Clone, Debug)]
pub struct ApprovalInfo {
    pub tool_name: String,
    pub pattern: String,
    pub arguments_summary: String,
    pub approved_count: u32,
    pub current_level: TrustLevel,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct TrustEntry {
    pub tool: String,
    pub pattern: String,
    pub level: TrustLevel,
    pub approved_count: u32,
    pub denied_count: u32,
    pub last_approved: Option<DateTime<Utc>>,
    pub last_denied: Option<DateTime<Utc>>,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Serialize, Deserialize)]
struct TrustFile {
    version: u32,
    entries: Vec<TrustEntry>,
}

pub struct TrustStore {
    inner: Arc<RwLock<TrustStoreInner>>,
    path: Option<PathBuf>,
}

struct TrustStoreInner {
    entries: HashMap<(String, String), TrustEntry>,
    session_trusts: HashSet<(String, String)>,
}

/// Serialize read-merge-write per trust file. Two runs in the same project
/// share the file; without this, whichever snapshot lands last erased the
/// other run's grants.
static SAVE_LOCKS: OnceLock<Mutex<HashMap<PathBuf, Arc<Mutex<()>>>>> = OnceLock::new();

fn save_lock(path: &Path) -> Arc<Mutex<()>> {
    let map = SAVE_LOCKS.get_or_init(|| Mutex::new(HashMap::new()));
    let mut map = map.lock().unwrap_or_else(|e| e.into_inner());
    Arc::clone(
        map.entry(path.to_path_buf())
            .or_insert_with(|| Arc::new(Mutex::new(()))),
    )
}

fn level_rank(level: &TrustLevel) -> u8 {
    match level {
        TrustLevel::Blocked => 4,
        TrustLevel::GlobalTrust => 3,
        TrustLevel::WorkspaceTrust => 2,
        TrustLevel::SessionTrust => 1,
        TrustLevel::AlwaysAsk => 0,
    }
}

impl TrustStore {
    pub fn new(path: Option<PathBuf>) -> Self {
        let entries = path
            .as_ref()
            .and_then(Self::load_from_disk)
            .unwrap_or_default();
        TrustStore {
            inner: Arc::new(RwLock::new(TrustStoreInner {
                entries,
                session_trusts: HashSet::new(),
            })),
            path,
        }
    }

    pub fn in_memory() -> Self {
        Self::new(None)
    }

    pub fn check(&self, tool: &str, pattern: &str) -> TrustDecision {
        let inner = self.inner.read().unwrap();
        let key = &(tool.to_string(), pattern.to_string());

        if inner.session_trusts.contains(key) {
            return TrustDecision::Allowed;
        }

        match inner.entries.get(key) {
            Some(entry) if entry.level == TrustLevel::Blocked => TrustDecision::Blocked(format!(
                "Operation `{tool}` with pattern `{pattern}` is permanently blocked",
            )),
            Some(entry)
                if entry.level == TrustLevel::GlobalTrust
                    || entry.level == TrustLevel::WorkspaceTrust =>
            {
                TrustDecision::Allowed
            }
            Some(entry) if entry.level == TrustLevel::SessionTrust => {
                if inner.session_trusts.contains(key) {
                    TrustDecision::Allowed
                } else {
                    TrustDecision::NeedsApproval(ApprovalInfo {
                        tool_name: tool.to_string(),
                        pattern: pattern.to_string(),
                        arguments_summary: String::new(),
                        approved_count: entry.approved_count,
                        current_level: entry.level.clone(),
                    })
                }
            }
            _ => TrustDecision::NeedsApproval(ApprovalInfo {
                tool_name: tool.to_string(),
                pattern: pattern.to_string(),
                arguments_summary: String::new(),
                approved_count: 0,
                current_level: TrustLevel::AlwaysAsk,
            }),
        }
    }

    pub fn record_approval(&self, tool: &str, pattern: &str, _info: &ApprovalInfo) {
        let mut inner = self.inner.write().unwrap();
        let key = (tool.to_string(), pattern.to_string());
        if inner.session_trusts.contains(&key) {
            return;
        }

        let entry = inner
            .entries
            .entry(key.clone())
            .or_insert_with(|| TrustEntry {
                tool: tool.to_string(),
                pattern: pattern.to_string(),
                level: TrustLevel::AlwaysAsk,
                approved_count: 0,
                denied_count: 0,
                last_approved: None,
                last_denied: None,
                created_at: Utc::now(),
            });

        entry.approved_count += 1;
        entry.last_approved = Some(Utc::now());

        // Escalate trust level
        // Interpreters / network clients never escalate: one approval for
        // `python3 x.py` must not become a standing approval for
        // `python3 -c "<anything>"` (round5 P1-8a).
        if crate::patterns::is_auto_trustable(tool, pattern) {
            // Three ordinary Allow clicks on the same concrete pattern is
            // enough to stop asking for it in this project. The old rule
            // (10 approvals AND >=1 day) almost never fired; the old 3-click
            // rule only set an in-memory SessionTrust that was lost with the
            // run's TrustStore.
            if entry.approved_count >= 3 {
                entry.level = TrustLevel::WorkspaceTrust;
            }
        }

        drop(inner);
        self.maybe_save();
    }

    /// "Trust this session": allow immediately, not persisted.
    pub fn trust_session(&self, tool: &str, pattern: &str) {
        // A wildcard/tool-name pattern would trust every invocation of a
        // whole tool. Explicit trust is still per concrete action.
        if pattern == "*" || pattern == tool {
            return;
        }
        let mut inner = self.inner.write().unwrap();
        let key = (tool.to_string(), pattern.to_string());
        if inner
            .entries
            .get(&key)
            .is_some_and(|e| e.level == TrustLevel::Blocked)
        {
            return;
        }
        let entry = inner
            .entries
            .entry(key.clone())
            .or_insert_with(|| TrustEntry {
                tool: tool.to_string(),
                pattern: pattern.to_string(),
                level: TrustLevel::SessionTrust,
                approved_count: 0,
                denied_count: 0,
                last_approved: None,
                last_denied: None,
                created_at: Utc::now(),
            });
        entry.approved_count += 1;
        entry.last_approved = Some(Utc::now());
        if entry.level != TrustLevel::WorkspaceTrust && entry.level != TrustLevel::Blocked {
            entry.level = TrustLevel::SessionTrust;
        }
        inner.session_trusts.insert(key);
        drop(inner);
        self.maybe_save();
    }

    /// "Trust this project": allow immediately and persist.
    pub fn trust_workspace(&self, tool: &str, pattern: &str) {
        // Interpreters/network/MCP wildcards never get a durable grant; fall
        // back to this-run trust instead of silently broadening the scope.
        if !crate::patterns::is_auto_trustable(tool, pattern) || pattern == "*" || pattern == tool {
            self.trust_session(tool, pattern);
            return;
        }
        let mut inner = self.inner.write().unwrap();
        let key = (tool.to_string(), pattern.to_string());
        if inner
            .entries
            .get(&key)
            .is_some_and(|e| e.level == TrustLevel::Blocked)
        {
            return;
        }
        let entry = inner
            .entries
            .entry(key.clone())
            .or_insert_with(|| TrustEntry {
                tool: tool.to_string(),
                pattern: pattern.to_string(),
                level: TrustLevel::WorkspaceTrust,
                approved_count: 0,
                denied_count: 0,
                last_approved: None,
                last_denied: None,
                created_at: Utc::now(),
            });
        entry.approved_count += 1;
        entry.last_approved = Some(Utc::now());
        entry.level = TrustLevel::WorkspaceTrust;
        inner.session_trusts.insert(key);
        drop(inner);
        self.maybe_save();
    }

    pub fn record_denial(&self, tool: &str, pattern: &str) {
        let mut inner = self.inner.write().unwrap();
        let key = (tool.to_string(), pattern.to_string());

        let entry = inner.entries.entry(key).or_insert_with(|| TrustEntry {
            tool: tool.to_string(),
            pattern: pattern.to_string(),
            level: TrustLevel::AlwaysAsk,
            approved_count: 0,
            denied_count: 0,
            last_approved: None,
            last_denied: None,
            created_at: Utc::now(),
        });

        entry.denied_count += 1;
        entry.last_denied = Some(Utc::now());

        // Rejection fatigue: auto-block after N denials within the window
        if entry.denied_count >= DENIAL_AUTO_BLOCK_COUNT {
            if let Some(last) = entry.last_denied {
                let elapsed = Utc::now().signed_duration_since(last);
                if elapsed.num_seconds().unsigned_abs() <= DENIAL_AUTO_BLOCK_WINDOW.as_secs() {
                    entry.level = TrustLevel::Blocked;
                    tracing::warn!(
                        "[safety] auto-blocked `{tool}/{pattern}` after {count} denials",
                        tool = tool,
                        pattern = pattern,
                        count = DENIAL_AUTO_BLOCK_COUNT
                    );
                }
            }
        }

        drop(inner);
        self.maybe_save();
    }

    pub fn block(&self, tool: &str, pattern: &str) {
        let mut inner = self.inner.write().unwrap();
        let key = (tool.to_string(), pattern.to_string());
        inner.session_trusts.remove(&key);
        let entry = inner.entries.entry(key).or_insert_with(|| TrustEntry {
            tool: tool.to_string(),
            pattern: pattern.to_string(),
            level: TrustLevel::AlwaysAsk,
            approved_count: 0,
            denied_count: 0,
            last_approved: None,
            last_denied: None,
            created_at: Utc::now(),
        });
        entry.level = TrustLevel::Blocked;
        drop(inner);
        self.maybe_save();
    }

    pub fn unblock(&self, tool: &str, pattern: &str) {
        let mut inner = self.inner.write().unwrap();
        if let Some(entry) = inner
            .entries
            .get_mut(&(tool.to_string(), pattern.to_string()))
        {
            entry.level = TrustLevel::AlwaysAsk;
            entry.denied_count = 0;
        }
        drop(inner);
        self.maybe_save();
    }

    pub fn decay_expired(&self, max_days_inactive: i64) {
        let mut inner = self.inner.write().unwrap();
        let now = Utc::now();
        for entry in inner.entries.values_mut() {
            if entry.level == TrustLevel::WorkspaceTrust {
                if let Some(last) = entry.last_approved {
                    if (now - last).num_days() >= max_days_inactive {
                        entry.level = TrustLevel::SessionTrust;
                        tracing::info!(
                            "[safety] decayed `{}/{}` to SessionTrust (inactive)",
                            entry.tool,
                            entry.pattern
                        );
                    }
                }
            }
        }
        drop(inner);
        self.maybe_save();
    }

    pub fn list_trusted(&self) -> Vec<(String, String, TrustLevel)> {
        let inner = self.inner.read().unwrap();
        inner
            .entries
            .iter()
            .filter(|(_, e)| e.level != TrustLevel::Blocked && e.level != TrustLevel::AlwaysAsk)
            .map(|((t, p), e)| (t.clone(), p.clone(), e.level.clone()))
            .collect()
    }

    pub fn list_blocked(&self) -> Vec<(String, String)> {
        let inner = self.inner.read().unwrap();
        inner
            .entries
            .iter()
            .filter(|(_, e)| e.level == TrustLevel::Blocked)
            .map(|((t, p), _)| (t.clone(), p.clone()))
            .collect()
    }

    pub(crate) fn maybe_save(&self) {
        let Some(ref path) = self.path else { return };
        let lock = save_lock(path);
        let _guard = lock.lock().unwrap_or_else(|e| e.into_inner());
        let inner = self.inner.read().unwrap();
        let mut merged: HashMap<(String, String), TrustEntry> = inner
            .entries
            .values()
            .filter(|e| {
                e.level == TrustLevel::WorkspaceTrust
                    || e.level == TrustLevel::GlobalTrust
                    || e.level == TrustLevel::Blocked
            })
            .map(|e| ((e.tool.clone(), e.pattern.clone()), e.clone()))
            .collect();
        drop(inner);
        // Merge the current disk state so a concurrent run's grants are not
        // overwritten by this run's older snapshot.
        if let Ok(raw) = std::fs::read_to_string(path) {
            if let Ok(file) = serde_json::from_str::<TrustFile>(&raw) {
                for e in file.entries {
                    let key = (e.tool.clone(), e.pattern.clone());
                    match merged.get(&key) {
                        Some(existing)
                            if level_rank(&existing.level) > level_rank(&e.level)
                                || (existing.level == e.level
                                    && existing.approved_count >= e.approved_count) => {}
                        _ => {
                            merged.insert(key, e);
                        }
                    }
                }
            }
        }
        let entries: Vec<TrustEntry> = merged.into_values().collect();
        Self::save_to_disk(path, &entries);
    }

    fn load_from_disk(path: &PathBuf) -> Option<HashMap<(String, String), TrustEntry>> {
        let data = std::fs::read_to_string(path).ok()?;
        let file: TrustFile = serde_json::from_str(&data).ok()?;
        let entries: HashMap<_, _> = file
            .entries
            .into_iter()
            // Drop remembered trust for interpreters/network clients: entries
            // written before this rule existed (or planted by anything that
            // could write the file) must not stay authoritative. Blocked
            // entries are kept — they only ever deny.
            .filter(|e| {
                e.level == TrustLevel::Blocked
                    || crate::patterns::is_auto_trustable(&e.tool, &e.pattern)
            })
            .map(|e| ((e.tool.clone(), e.pattern.clone()), e))
            .collect();
        Some(entries)
    }

    fn save_to_disk(path: &PathBuf, entries: &[TrustEntry]) {
        if let Some(parent) = path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        let tmp = path.with_extension(format!(
            "tmp-{}-{}",
            std::process::id(),
            chrono::Utc::now().timestamp_nanos_opt().unwrap_or_default()
        ));
        let file = TrustFile {
            version: 1,
            entries: entries.to_vec(),
        };
        if let Ok(json) = serde_json::to_string_pretty(&file) {
            if std::fs::write(&tmp, &json).is_ok() {
                let _ = std::fs::rename(&tmp, path);
                // Set restrictive permissions
                #[cfg(unix)]
                {
                    use std::os::unix::fs::PermissionsExt;
                    let _ = std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600));
                }
            }
        }
    }
}

impl Clone for TrustStore {
    fn clone(&self) -> Self {
        TrustStore {
            inner: self.inner.clone(),
            path: self.path.clone(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_store() -> TrustStore {
        TrustStore::in_memory()
    }

    #[test]
    fn test_new_entry_always_ask() {
        let store = make_store();
        let decision = store.check("code_run", "echo");
        match decision {
            TrustDecision::NeedsApproval(info) => {
                assert_eq!(info.approved_count, 0);
            }
            _ => panic!("expected NeedsApproval"),
        }
    }

    #[test]
    fn test_approval_escalation() {
        let store = make_store();
        let info = ApprovalInfo {
            tool_name: "code_run".into(),
            pattern: "echo".into(),
            arguments_summary: String::new(),
            approved_count: 0,
            current_level: TrustLevel::AlwaysAsk,
        };
        store.record_approval("code_run", "echo", &info);
        store.record_approval("code_run", "echo", &info);
        store.record_approval("code_run", "echo", &info);

        let decision = store.check("code_run", "echo");
        assert!(matches!(decision, TrustDecision::Allowed));
    }

    #[test]
    fn test_block_and_unblock() {
        let store = make_store();
        store.block("web_scan", "evil.com");
        let decision = store.check("web_scan", "evil.com");
        assert!(matches!(decision, TrustDecision::Blocked(_)));

        store.unblock("web_scan", "evil.com");
        let decision = store.check("web_scan", "evil.com");
        assert!(matches!(decision, TrustDecision::NeedsApproval(_)));
    }

    #[test]
    fn test_rejection_fatigue() {
        let store = make_store();
        store.record_denial("code_run", "rm");
        store.record_denial("code_run", "rm");
        // After 3 denials, should auto-block
        store.record_denial("code_run", "rm");

        let decision = store.check("code_run", "rm");
        assert!(matches!(decision, TrustDecision::Blocked(_)));
    }

    #[test]
    fn test_persistence_roundtrip() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("trust.json");

        let store = TrustStore::new(Some(path.clone()));
        let info = ApprovalInfo {
            tool_name: "code_run".into(),
            pattern: "echo".into(),
            arguments_summary: String::new(),
            approved_count: 0,
            current_level: TrustLevel::AlwaysAsk,
        };
        for _ in 0..10 {
            store.record_approval("code_run", "echo", &info);
        }
        // Force WorkspaceTrust for deterministic persistence test
        {
            let mut inner = store.inner.write().unwrap();
            if let Some(entry) = inner.entries.get_mut(&("code_run".into(), "echo".into())) {
                entry.level = TrustLevel::WorkspaceTrust;
            }
        }
        store.maybe_save();

        assert!(path.exists());

        let store2 = TrustStore::new(Some(path));
        let decision = store2.check("code_run", "echo");
        assert!(matches!(decision, TrustDecision::Allowed));
    }

    #[test]
    fn repeated_allow_persists_workspace_trust_across_runs() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("trust.json");
        let info = ApprovalInfo {
            tool_name: "code_run".into(),
            pattern: "git status".into(),
            arguments_summary: String::new(),
            approved_count: 0,
            current_level: TrustLevel::AlwaysAsk,
        };
        {
            let store = TrustStore::new(Some(path.clone()));
            for _ in 0..3 {
                store.record_approval("code_run", "git status", &info);
            }
            assert!(matches!(
                store.check("code_run", "git status"),
                TrustDecision::Allowed
            ));
        }
        // A new run creates a new TrustStore; the grant must survive.
        let reloaded = TrustStore::new(Some(path));
        assert!(
            matches!(
                reloaded.check("code_run", "git status"),
                TrustDecision::Allowed
            ),
            "three Allow clicks must persist, not reset with the run"
        );
    }

    #[test]
    fn explicit_session_trust_is_immediate_but_not_persisted() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("trust.json");
        {
            let store = TrustStore::new(Some(path.clone()));
            store.trust_session("code_run", "npm test");
            assert!(matches!(
                store.check("code_run", "npm test"),
                TrustDecision::Allowed
            ));
        }
        let reloaded = TrustStore::new(Some(path));
        assert!(
            matches!(
                reloaded.check("code_run", "npm test"),
                TrustDecision::NeedsApproval(_)
            ),
            "session trust must not leak into the next run"
        );
    }

    #[test]
    fn explicit_workspace_trust_is_immediate_and_persisted() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("trust.json");
        {
            let store = TrustStore::new(Some(path.clone()));
            store.trust_workspace("code_run", "cargo test");
        }
        let reloaded = TrustStore::new(Some(path));
        assert!(matches!(
            reloaded.check("code_run", "cargo test"),
            TrustDecision::Allowed
        ));
    }

    #[test]
    fn concurrent_trust_stores_merge_instead_of_losing_grants() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("trust.json");
        let a = TrustStore::new(Some(path.clone()));
        let b = TrustStore::new(Some(path.clone()));
        let info = |tool: &str, pattern: &str| ApprovalInfo {
            tool_name: tool.into(),
            pattern: pattern.into(),
            arguments_summary: String::new(),
            approved_count: 0,
            current_level: TrustLevel::AlwaysAsk,
        };
        for _ in 0..3 {
            a.record_approval("code_run", "git status", &info("code_run", "git status"));
            b.record_approval("code_run", "npm test", &info("code_run", "npm test"));
        }
        a.maybe_save();
        b.maybe_save();
        let reloaded = TrustStore::new(Some(path));
        assert!(matches!(
            reloaded.check("code_run", "git status"),
            TrustDecision::Allowed
        ));
        assert!(matches!(
            reloaded.check("code_run", "npm test"),
            TrustDecision::Allowed
        ));
    }
}
