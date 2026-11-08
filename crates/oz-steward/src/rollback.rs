//! File journal for L4 rollback / fork.
//!
//! The journal snapshots only the files named by the caller (normally the
//! `targets` of write/edit/patch steps) and restores exactly those files.
//! It is deliberately not a workspace-wide backup: unrelated user edits must
//! survive a rollback.

use std::collections::BTreeMap;
use std::path::{Component, Path, PathBuf};

use serde::Serialize;
use uuid::Uuid;

use crate::error::{Result, StewardError};

#[derive(Debug, Clone)]
struct Entry {
    ticket_id: String,
    relative: PathBuf,
    existed: bool,
    backup: Option<PathBuf>,
}

#[derive(Debug, Clone, Serialize, Default, PartialEq, Eq)]
pub struct RollbackReport {
    pub restored: Vec<String>,
    pub removed: Vec<String>,
    pub skipped: Vec<String>,
}

pub struct FileJournal {
    root: PathBuf,
    backup_dir: PathBuf,
    entries: BTreeMap<(String, String), Entry>,
}

impl FileJournal {
    pub fn new(root: impl Into<PathBuf>, backup_dir: impl Into<PathBuf>) -> Result<Self> {
        let root = root.into();
        let backup_dir = backup_dir.into();
        std::fs::create_dir_all(&root)?;
        std::fs::create_dir_all(&backup_dir)?;
        Ok(Self {
            root,
            backup_dir,
            entries: BTreeMap::new(),
        })
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    /// Snapshot files before they are modified. Duplicate paths are cheap
    /// no-ops so the recorder can call this on every write.
    pub fn snapshot(&mut self, paths: &[String]) -> Result<()> {
        self.snapshot_for("__global__", paths)
    }

    /// Snapshot only for one ticket so a rollback cannot touch another
    /// session's files.
    pub fn snapshot_for(&mut self, ticket_id: &str, paths: &[String]) -> Result<()> {
        for raw in paths {
            let relative = safe_relative(raw)?;
            let key = (
                ticket_id.to_string(),
                relative.to_string_lossy().to_string(),
            );
            if self.entries.contains_key(&key) {
                continue;
            }
            let source = self.root.join(&relative);
            if source.exists() {
                let backup = self.backup_dir.join(format!("{}.bak", Uuid::new_v4()));
                std::fs::copy(&source, &backup)?;
                self.entries.insert(
                    key,
                    Entry {
                        ticket_id: ticket_id.to_string(),
                        relative,
                        existed: true,
                        backup: Some(backup),
                    },
                );
            } else {
                self.entries.insert(
                    key,
                    Entry {
                        ticket_id: ticket_id.to_string(),
                        relative,
                        existed: false,
                        backup: None,
                    },
                );
            }
        }
        Ok(())
    }

    /// Restore entries in reverse snapshot order. A file that did not exist
    /// before the first snapshot is removed; an existing file is restored
    /// from its backup.
    pub fn rollback(&self) -> Result<RollbackReport> {
        self.rollback_where(|_| true)
    }

    /// Roll back only the files journaled for one ticket.
    pub fn rollback_ticket(&self, ticket_id: &str) -> Result<RollbackReport> {
        self.rollback_where(|entry| entry.ticket_id == ticket_id)
    }

    fn rollback_where<F>(&self, keep: F) -> Result<RollbackReport>
    where
        F: Fn(&Entry) -> bool,
    {
        let mut report = RollbackReport::default();
        for entry in self.entries.values().rev().filter(|e| keep(e)) {
            let target = self.root.join(&entry.relative);
            let name = entry.relative.to_string_lossy().to_string();
            if entry.existed {
                let Some(backup) = &entry.backup else {
                    report.skipped.push(name);
                    continue;
                };
                if let Some(parent) = target.parent() {
                    std::fs::create_dir_all(parent)?;
                }
                std::fs::copy(backup, &target)?;
                report.restored.push(name);
            } else if target.exists() {
                std::fs::remove_file(&target)?;
                report.removed.push(name);
            } else {
                report.skipped.push(name);
            }
        }
        Ok(report)
    }

    /// Copy the current versions of journaled files into `dest` for a fork.
    pub fn fork_to(&self, dest: impl AsRef<Path>) -> Result<Vec<String>> {
        self.fork_where(dest, |_| true)
    }

    pub fn fork_ticket_to(&self, ticket_id: &str, dest: impl AsRef<Path>) -> Result<Vec<String>> {
        self.fork_where(dest, |e| e.ticket_id == ticket_id)
    }

    fn fork_where<F>(&self, dest: impl AsRef<Path>, keep: F) -> Result<Vec<String>>
    where
        F: Fn(&Entry) -> bool,
    {
        let dest = dest.as_ref();
        std::fs::create_dir_all(dest)?;
        let mut copied = Vec::new();
        for entry in self.entries.values().filter(|e| keep(e)) {
            let source = self.root.join(&entry.relative);
            if !source.exists() {
                continue;
            }
            let target = dest.join(&entry.relative);
            if let Some(parent) = target.parent() {
                std::fs::create_dir_all(parent)?;
            }
            std::fs::copy(&source, &target)?;
            copied.push(entry.relative.to_string_lossy().to_string());
        }
        Ok(copied)
    }
}

fn safe_relative(raw: &str) -> Result<PathBuf> {
    let path = Path::new(raw);
    if path.is_absolute() {
        return Err(StewardError::Invalid(format!(
            "journal path must be relative to the workspace: {raw}"
        )));
    }
    let mut out = PathBuf::new();
    for component in path.components() {
        match component {
            Component::Normal(part) => out.push(part),
            Component::CurDir => {}
            Component::ParentDir | Component::RootDir | Component::Prefix(_) => {
                return Err(StewardError::Invalid(format!(
                    "journal path escapes the workspace: {raw}"
                )))
            }
        }
    }
    if out.as_os_str().is_empty() {
        return Err(StewardError::Invalid("empty journal path".into()));
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rollback_restores_modified_and_removes_created() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("work");
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(root.join("a.txt"), "before").unwrap();
        let mut journal = FileJournal::new(&root, dir.path().join("backup")).unwrap();
        journal
            .snapshot(&["a.txt".into(), "new.txt".into()])
            .unwrap();

        std::fs::write(root.join("a.txt"), "after").unwrap();
        std::fs::write(root.join("new.txt"), "created").unwrap();
        let report = journal.rollback().unwrap();
        assert_eq!(
            std::fs::read_to_string(root.join("a.txt")).unwrap(),
            "before"
        );
        assert!(!root.join("new.txt").exists());
        assert_eq!(report.restored, vec!["a.txt"]);
        assert_eq!(report.removed, vec!["new.txt"]);
    }

    #[test]
    fn fork_copies_only_journaled_files() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("work");
        std::fs::create_dir_all(root.join("src")).unwrap();
        std::fs::write(root.join("src/a.rs"), "a").unwrap();
        std::fs::write(root.join("src/b.rs"), "b").unwrap();
        let mut journal = FileJournal::new(&root, dir.path().join("backup")).unwrap();
        journal.snapshot(&["src/a.rs".into()]).unwrap();
        let dest = dir.path().join("fork");
        let copied = journal.fork_to(&dest).unwrap();
        assert_eq!(copied, vec!["src/a.rs"]);
        assert!(dest.join("src/a.rs").exists());
        assert!(!dest.join("src/b.rs").exists());
    }

    #[test]
    fn rollback_is_scoped_to_one_ticket() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("work");
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(root.join("a.txt"), "a0").unwrap();
        std::fs::write(root.join("b.txt"), "b0").unwrap();
        let mut journal = FileJournal::new(&root, dir.path().join("backup")).unwrap();
        journal.snapshot_for("t1", &["a.txt".into()]).unwrap();
        journal.snapshot_for("t2", &["b.txt".into()]).unwrap();
        std::fs::write(root.join("a.txt"), "a1").unwrap();
        std::fs::write(root.join("b.txt"), "b1").unwrap();
        journal.rollback_ticket("t1").unwrap();
        assert_eq!(std::fs::read_to_string(root.join("a.txt")).unwrap(), "a0");
        assert_eq!(std::fs::read_to_string(root.join("b.txt")).unwrap(), "b1");
    }

    #[test]
    fn parent_escape_is_rejected() {
        let dir = tempfile::tempdir().unwrap();
        let mut journal =
            FileJournal::new(dir.path().join("work"), dir.path().join("backup")).unwrap();
        assert!(journal.snapshot(&["../secret".into()]).is_err());
        assert!(journal.snapshot(&["/etc/passwd".into()]).is_err());
    }
}
