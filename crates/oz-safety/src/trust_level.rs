//! Project trust levels (B7a) — coarse per-project capability gating on top
//! of the fine-grained permission policy (P1-4).
//!
//! Levels, least to most restrictive:
//! - `full`      — no extra restrictions (current behavior; default)
//! - `restricted`— blocks execution tools (code_run, web_execute_js)
//! - `readonly`  — blocks all write/execution tools (write, edit, patch, …)
//!
//! Stored in `{data_dir}/trust.json` as `{ "projects": [{root_path, level}] }`.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

/// Coarse trust level for a project root.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ProjectTrustLevel {
    #[default]
    Full,
    Restricted,
    Readonly,
}

impl ProjectTrustLevel {
    /// Tools denied at this level. Empty for `Full`.
    pub fn denied_tools(&self) -> &'static [&'static str] {
        match self {
            ProjectTrustLevel::Full => &[],
            ProjectTrustLevel::Restricted => &["code_run", "web_execute_js"],
            ProjectTrustLevel::Readonly => &[
                "code_run",
                "web_execute_js",
                "write",
                "edit",
                "patch",
            ],
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TrustEntry {
    pub root_path: String,
    pub level: ProjectTrustLevel,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct TrustFile {
    #[serde(default)]
    pub projects: Vec<TrustEntry>,
}

/// Load trust levels from `<dir>/trust.json`; missing/unreadable → empty.
pub fn load_trust(dir: &Path) -> TrustFile {
    std::fs::read_to_string(dir.join("trust.json"))
        .ok()
        .and_then(|data| serde_json::from_str(&data).ok())
        .unwrap_or_default()
}

/// Persist trust levels atomically (tmp + rename, mirroring projects.json).
pub fn save_trust(dir: &Path, file: &TrustFile) -> Result<(), String> {
    let path = dir.join("trust.json");
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| format!("Failed to create dir: {e}"))?;
    }
    let json = serde_json::to_string_pretty(file).map_err(|e| format!("Serialize: {e}"))?;
    let tmp = path.with_extension(format!("tmp.{}", std::process::id()));
    std::fs::write(&tmp, &json).map_err(|e| format!("Write: {e}"))?;
    if let Err(e) = std::fs::rename(&tmp, &path) {
        if e.raw_os_error() == Some(18) {
            std::fs::copy(&tmp, &path).map_err(|e| format!("Cross-device copy: {e}"))?;
            let _ = std::fs::remove_file(&tmp);
        } else {
            return Err(format!("Atomic rename: {e}"));
        }
    }
    Ok(())
}

/// Set the trust level for a project root (upsert).
pub fn set_project_trust(dir: &Path, root_path: &str, level: ProjectTrustLevel) -> Result<(), String> {
    let mut file = load_trust(dir);
    let root = normalize(root_path);
    match file.projects.iter_mut().find(|e| normalize(&e.root_path) == root) {
        Some(entry) => entry.level = level,
        None => file.projects.push(TrustEntry { root_path: root, level }),
    }
    save_trust(dir, &file)
}

/// Look up the trust level for a project root; default `Full` when unset.
pub fn project_trust(dir: &Path, root_path: &str) -> ProjectTrustLevel {
    let file = load_trust(dir);
    let root = normalize(root_path);
    file.projects
        .iter()
        .find(|e| normalize(&e.root_path) == root)
        .map(|e| e.level)
        .unwrap_or_default()
}

fn normalize(p: &str) -> String {
    PathBuf::from(p)
        .canonicalize()
        .unwrap_or_else(|_| PathBuf::from(p))
        .to_string_lossy()
        .trim_end_matches('/')
        .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp_dir(tag: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("oz-trust-{}-{tag}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn test_default_is_full() {
        let dir = tmp_dir("default");
        assert_eq!(project_trust(&dir, "/tmp/proj"), ProjectTrustLevel::Full);
        assert!(ProjectTrustLevel::Full.denied_tools().is_empty());
    }

    #[test]
    fn test_set_and_read_roundtrip() {
        let dir = tmp_dir("roundtrip");
        set_project_trust(&dir, "/tmp/proj", ProjectTrustLevel::Restricted).unwrap();
        assert_eq!(project_trust(&dir, "/tmp/proj"), ProjectTrustLevel::Restricted);
        // Reload from disk.
        let file = load_trust(&dir);
        assert_eq!(file.projects.len(), 1);
        assert_eq!(file.projects[0].level, ProjectTrustLevel::Restricted);
    }

    #[test]
    fn test_set_overwrites_existing() {
        let dir = tmp_dir("overwrite");
        set_project_trust(&dir, "/tmp/proj", ProjectTrustLevel::Restricted).unwrap();
        set_project_trust(&dir, "/tmp/proj", ProjectTrustLevel::Readonly).unwrap();
        assert_eq!(project_trust(&dir, "/tmp/proj"), ProjectTrustLevel::Readonly);
        assert_eq!(load_trust(&dir).projects.len(), 1);
    }

    #[test]
    fn test_denied_tools_by_level() {
        assert_eq!(ProjectTrustLevel::Restricted.denied_tools(), &["code_run", "web_execute_js"]);
        let ro = ProjectTrustLevel::Readonly.denied_tools();
        assert!(ro.contains(&"write"));
        assert!(ro.contains(&"code_run"));
        assert!(!ro.contains(&"read"));
        assert!(!ro.contains(&"grep"));
    }

    #[test]
    fn test_bad_json_degrades_to_empty() {
        let dir = tmp_dir("bad");
        std::fs::write(dir.join("trust.json"), "not json [[[").unwrap();
        assert!(load_trust(&dir).projects.is_empty());
        assert_eq!(project_trust(&dir, "/tmp/proj"), ProjectTrustLevel::Full);
    }
}
