//! Trust store — progressive trust entries with auto-escalation and decay.
//!
//! Keyed by `(tool_name, arg_pattern)`. Tracks approval/denial counts and
//! auto-escalates trust level based on user behavior.

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::{Arc, RwLock};
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
        let now = Utc::now();
        let span = entry.created_at;
        let days_span = (now - span).num_days();

        if entry.approved_count >= 10 && days_span >= 1 {
            entry.level = TrustLevel::WorkspaceTrust;
        } else if entry.approved_count >= 3 {
            entry.level = TrustLevel::SessionTrust;
            inner.session_trusts.insert(key.clone());
        }

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
        if let Some(ref path) = self.path {
            let inner = self.inner.read().unwrap();
            let entries: Vec<TrustEntry> = inner
                .entries
                .values()
                .filter(|e| {
                    e.level == TrustLevel::WorkspaceTrust
                        || e.level == TrustLevel::GlobalTrust
                        || e.level == TrustLevel::Blocked
                })
                .cloned()
                .collect();
            drop(inner);
            Self::save_to_disk(path, &entries);
        }
    }

    fn load_from_disk(path: &PathBuf) -> Option<HashMap<(String, String), TrustEntry>> {
        let data = std::fs::read_to_string(path).ok()?;
        let file: TrustFile = serde_json::from_str(&data).ok()?;
        let entries: HashMap<_, _> = file
            .entries
            .into_iter()
            .map(|e| ((e.tool.clone(), e.pattern.clone()), e))
            .collect();
        Some(entries)
    }

    fn save_to_disk(path: &PathBuf, entries: &[TrustEntry]) {
        if let Some(parent) = path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        let tmp = path.with_extension("tmp");
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
}
