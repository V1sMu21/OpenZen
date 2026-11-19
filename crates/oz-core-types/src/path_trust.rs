//! Path-boundary approvals for mutating file tools.
//!
//! `write` / `edit` / `patch` are on the safety guard's safe list, so the
//! approval pipeline never sees them; their workspace boundary is enforced
//! inside the tools instead. That meant an out-of-workspace edit was a hard
//! failure the model could only react to by asking the user in prose — it
//! could not be *approved*.
//!
//! This module supplies the two halves of the fix:
//! * [`write_target_path`] + [`resolve_boundary_path`] — what the agent loop
//!   uses to decide whether a call needs the user's consent;
//! * [`approve_path`] / [`is_path_approved`] — the session-scoped record of
//!   paths the user approved, which the file tools consult as an extra allow
//!   condition.
//!
//! Only an approval decision ever writes to the registry: the model cannot
//! reach it, and a denial records nothing, so the boundary still holds by
//! default. The registry is bounded and keyed by session.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};

/// Cap per session so a long run cannot grow the registry without limit.
const MAX_APPROVED_PER_SESSION: usize = 64;
/// Cap on tracked sessions (oldest-first eviction).
const MAX_SESSIONS: usize = 32;

fn registry() -> &'static Mutex<HashMap<String, SessionPaths>> {
    static REGISTRY: OnceLock<Mutex<HashMap<String, SessionPaths>>> = OnceLock::new();
    REGISTRY.get_or_init(|| Mutex::new(HashMap::new()))
}

/// What the user has decided about paths in one session/run.
#[derive(Default)]
struct SessionPaths {
    approved: HashSet<PathBuf>,
    /// Paths the user refused. Recorded so a model that keeps retrying gets a
    /// deterministic "no" instead of re-opening the approval dialog for every
    /// attempt (the guard has the same idea for tool patterns).
    denied: HashSet<PathBuf>,
}

impl SessionPaths {
    fn insert_bounded(set: &mut HashSet<PathBuf>, path: PathBuf) {
        if set.len() >= MAX_APPROVED_PER_SESSION {
            // Bounded: drop an arbitrary entry; the user is asked again later.
            if let Some(first) = set.iter().next().cloned() {
                set.remove(&first);
            }
        }
        set.insert(path);
    }
}

/// Canonical form of `path` for boundary comparisons: the deepest EXISTING
/// ancestor is canonicalized (resolving `..` and symlinks) and the
/// not-yet-existing tail is re-attached verbatim.
///
/// A non-existent component cannot be a symlink, so re-attaching the tail adds
/// no traversal surface, and a path whose parent does not exist yet no longer
/// collapses to "unresolvable" — which used to be reported to the model as a
/// permission problem (2026-10-23 isolated run: three retries on
/// `work/e2e/hello.py` before a manual `mkdir -p`).
pub fn resolve_boundary_path(path: &Path) -> PathBuf {
    resolve_boundary_path_depth(path, 0)
}

fn resolve_boundary_path_depth(path: &Path, depth: u8) -> PathBuf {
    // Canonicalize the ORIGINAL path first. Pre-collapsing `..` before
    // canonicalize is wrong when a component is a symlink: `link/../x`
    // resolves relative to the link target, not to the lexical parent.
    if let Ok(real) = std::fs::canonicalize(path) {
        return real;
    }
    // A dangling symlink still exists as a link; `canonicalize` fails only
    // because its target does not. Re-attaching the leaf lexically treated it
    // as an in-workspace file while the eventual write followed the link out
    // of the workspace. Resolve the target (bounded) before falling back.
    if depth <= 8 {
        if let Ok(md) = std::fs::symlink_metadata(path) {
            if md.file_type().is_symlink() {
                if let Ok(target) = std::fs::read_link(path) {
                    let abs = if target.is_absolute() {
                        target
                    } else {
                        path.parent().unwrap_or(Path::new("/")).join(target)
                    };
                    return normalize_lexical(&resolve_boundary_path_depth(&abs, depth + 1));
                }
            }
        }
    }
    // Deepest existing ancestor, preserving every trailing component
    // (including `..`): `file_name()` returns None for `..`, which is how the
    // old fallback silently dropped a traversal segment.
    let mut cursor = path.to_path_buf();
    let mut tail: Vec<std::ffi::OsString> = Vec::new();
    loop {
        if let Ok(real) = std::fs::canonicalize(&cursor) {
            let mut rebuilt = real;
            for part in tail.iter().rev() {
                if part == ".." {
                    rebuilt.pop();
                } else if part != "." {
                    rebuilt.push(part);
                }
            }
            return normalize_lexical(&rebuilt);
        }
        if depth <= 8 {
            if let Ok(md) = std::fs::symlink_metadata(&cursor) {
                if md.file_type().is_symlink() {
                    if let Ok(target) = std::fs::read_link(&cursor) {
                        let abs = if target.is_absolute() {
                            target
                        } else {
                            cursor.parent().unwrap_or(Path::new("/")).join(target)
                        };
                        let mut rebuilt = resolve_boundary_path_depth(&abs, depth + 1);
                        for part in tail.iter().rev() {
                            if part == ".." {
                                rebuilt.pop();
                            } else if part != "." {
                                rebuilt.push(part);
                            }
                        }
                        return normalize_lexical(&rebuilt);
                    }
                }
            }
        }
        let Some(parent) = cursor.parent() else {
            break;
        };
        if let Some(name) = cursor.file_name() {
            tail.push(name.to_os_string());
        } else if let Some(last) = cursor.components().next_back() {
            // `.` / `..` / root component.
            tail.push(last.as_os_str().to_os_string());
        }
        if parent == cursor {
            break;
        }
        cursor = parent.to_path_buf();
    }
    normalize_lexical(path)
}

/// Fold `.`/`..` WITHOUT touching the filesystem. This must run before the
/// deepest-existing-ancestor fallback: `file_name()` on a path ending in `..`
/// is `None`, and the old fallback silently dropped that segment, turning a
/// traversal into an in-workspace path.
pub fn normalize_lexical(path: &Path) -> PathBuf {
    use std::path::Component;
    let mut out = PathBuf::new();
    for c in path.components() {
        match c {
            Component::CurDir => {}
            Component::ParentDir => {
                if !out.pop() {
                    out.push("..");
                }
            }
            Component::Normal(p) => out.push(p),
            Component::RootDir | Component::Prefix(_) => out.push(c.as_os_str()),
        }
    }
    out
}

/// The file a mutating tool is about to write, if it names one.
///
/// Only the tools whose workspace boundary can now be approved are listed;
/// read-only tools keep their own fence untouched.
pub fn write_target_path<'a>(tool: &str, args: &'a serde_json::Value) -> Option<&'a str> {
    if !matches!(
        tool,
        "write" | "edit" | "patch" | "file_write" | "file_edit"
    ) {
        return None;
    }
    args.get("file_path").and_then(|v| v.as_str())
}

/// Resolved target of a mutating call when it lands OUTSIDE `working_dir`.
///
/// `None` means "no approval needed": either the call writes nothing, or the
/// path is inside the workspace.
pub fn outside_workspace_target(
    tool: &str,
    args: &serde_json::Value,
    working_dir: &str,
) -> Option<PathBuf> {
    let target = write_target_path(tool, args)?;
    let resolved = resolve_boundary_path(&absolute(target, working_dir));
    let root = resolve_boundary_path(Path::new(working_dir));
    if resolved.starts_with(&root) {
        None
    } else {
        Some(resolved)
    }
}

/// Make `path` relative to `working_dir` when it is not absolute.
fn absolute(path: &str, working_dir: &str) -> PathBuf {
    let p = Path::new(path);
    if p.is_absolute() {
        p.to_path_buf()
    } else {
        Path::new(working_dir).join(p)
    }
}

/// Remember that the user approved writing to `path` in this session.
pub fn approve_path(session_id: &str, path: &Path) {
    let resolved = resolve_boundary_path(path);
    let mut map = registry().lock().unwrap_or_else(|p| p.into_inner());
    if map.len() >= MAX_SESSIONS && !map.contains_key(session_id) {
        if let Some(oldest) = map.keys().next().cloned() {
            map.remove(&oldest);
        }
    }
    let entry = map.entry(session_id.to_string()).or_default();
    entry.denied.remove(&resolved);
    SessionPaths::insert_bounded(&mut entry.approved, resolved);
}

/// Remember that the user REFUSED writing to `path` in this session.
pub fn deny_path(session_id: &str, path: &Path) {
    if session_id.is_empty() {
        return;
    }
    let resolved = resolve_boundary_path(path);
    let mut map = registry().lock().unwrap_or_else(|p| p.into_inner());
    let entry = map.entry(session_id.to_string()).or_default();
    entry.approved.remove(&resolved);
    SessionPaths::insert_bounded(&mut entry.denied, resolved);
}

/// Did the user already refuse writing to `path` in this session?
pub fn is_path_denied(session_id: &str, path: &Path) -> bool {
    if session_id.is_empty() {
        return false;
    }
    let resolved = resolve_boundary_path(path);
    let map = registry().lock().unwrap_or_else(|p| p.into_inner());
    map.get(session_id)
        .map(|s| s.denied.contains(&resolved))
        .unwrap_or(false)
}

/// Has the user approved writing to `path` in this session?
pub fn is_path_approved(session_id: &str, path: &Path) -> bool {
    if session_id.is_empty() {
        return false;
    }
    let resolved = resolve_boundary_path(path);
    let map = registry().lock().unwrap_or_else(|p| p.into_inner());
    map.get(session_id)
        .map(|s| s.approved.contains(&resolved))
        .unwrap_or(false)
}

/// Drop a session's approvals (called when a run ends).
pub fn forget_session(session_id: &str) {
    let mut map = registry().lock().unwrap_or_else(|p| p.into_inner());
    map.remove(session_id);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmpdir(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("oz-pathtrust-{}-{tag}", std::process::id()));
        let _ = std::fs::create_dir_all(&dir);
        dir
    }

    #[test]
    fn only_mutating_tools_name_a_write_target() {
        let args = serde_json::json!({"file_path": "/etc/hosts"});
        assert_eq!(write_target_path("write", &args), Some("/etc/hosts"));
        assert_eq!(write_target_path("patch", &args), Some("/etc/hosts"));
        assert_eq!(write_target_path("edit", &args), Some("/etc/hosts"));
        assert_eq!(write_target_path("read", &args), None);
        assert_eq!(write_target_path("code_run", &args), None);
        assert_eq!(
            write_target_path("write", &serde_json::json!({"content": "x"})),
            None
        );
    }

    #[test]
    fn inside_the_workspace_needs_no_approval() {
        let wd = tmpdir("inside");
        let inside = serde_json::json!({"file_path": wd.join("a/b/c.txt").to_string_lossy()});
        assert!(outside_workspace_target("write", &inside, &wd.to_string_lossy()).is_none());

        // A relative path resolves against the workspace, not the process cwd.
        let rel = serde_json::json!({"file_path": "sub/dir/new.txt"});
        assert!(outside_workspace_target("write", &rel, &wd.to_string_lossy()).is_none());

        // ...and a relative escape is caught.
        let escape = serde_json::json!({"file_path": "../outside.txt"});
        let target = outside_workspace_target("write", &escape, &wd.to_string_lossy());
        assert!(target.is_some(), "`../` must leave the workspace");
        let _ = std::fs::remove_dir_all(&wd);
    }

    #[test]
    fn approvals_are_per_session_and_only_for_the_named_path() {
        let dir = tmpdir("approve");
        let outside = dir.join("elsewhere/cordis.patch.yml");
        assert!(!is_path_approved("s1", &outside), "unapproved by default");

        approve_path("s1", &outside);
        assert!(is_path_approved("s1", &outside));
        assert!(
            !is_path_approved("s2", &outside),
            "another session must not inherit the approval"
        );
        assert!(
            !is_path_approved("s1", &dir.join("elsewhere/other.yml")),
            "the approval covers exactly the approved path"
        );

        // An empty session id (no session) never grants anything.
        approve_path("", &dir.join("x"));
        assert!(!is_path_approved("", &dir.join("x")));

        forget_session("s1");
        assert!(!is_path_approved("s1", &outside));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn denials_are_recorded_so_the_question_is_not_re_asked() {
        // A session id of its own: these tests share the process-wide registry.
        let dir = tmpdir("deny");
        let sid = "sess-denial-test";
        let outside = dir.join("refused/x.txt");
        assert!(!is_path_denied(sid, &outside));

        deny_path(sid, &outside);
        assert!(is_path_denied(sid, &outside));
        assert!(
            !is_path_approved(sid, &outside),
            "a denial must never look like an approval"
        );
        assert!(
            !is_path_denied("sess-other-denial", &outside),
            "per session"
        );

        // A later explicit approval clears the denial.
        approve_path(sid, &outside);
        assert!(is_path_approved(sid, &outside));
        assert!(!is_path_denied(sid, &outside));

        forget_session(sid);
        assert!(!is_path_approved(sid, &outside) && !is_path_denied(sid, &outside));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn missing_parent_directories_still_resolve() {
        let dir = tmpdir("missing-parent");
        let deep = dir.join("a/b/c/deep.txt");
        let resolved = resolve_boundary_path(&deep);
        // The existing part is canonicalized (so /tmp symlinks normalise) and
        // the missing tail survives.
        assert!(
            resolved.ends_with("a/b/c/deep.txt"),
            "tail must be preserved: {}",
            resolved.display()
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn missing_parent_traversal_is_resolved_lexically() {
        let root = tmpdir("lexical-root");
        let raw = root.join("missing/../../../../etc/passwd");
        let resolved = normalize_lexical(&raw);
        assert!(
            !resolved.starts_with(&root),
            "traversal must not stay inside root: {}",
            resolved.display()
        );
        let target = outside_workspace_target(
            "write",
            &serde_json::json!({"file_path": raw.to_string_lossy()}),
            root.to_str().unwrap(),
        );
        assert!(
            target.is_some(),
            "missing-parent traversal must require approval"
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn symlink_parent_traversal_is_not_treated_as_inside() {
        let root = tmpdir("symlink-root");
        let outside = tmpdir("symlink-outside");
        std::fs::write(outside.join("victim.txt"), "x").unwrap();
        let link = root.join("link");
        #[cfg(unix)]
        std::os::unix::fs::symlink(&outside, &link).unwrap();
        let raw = link.join("../victim.txt");
        let target = outside_workspace_target(
            "write",
            &serde_json::json!({"file_path": raw.to_string_lossy()}),
            root.to_str().unwrap(),
        );
        assert!(
            target.is_some(),
            "symlink/.. must resolve outside: {target:?}"
        );
        let _ = std::fs::remove_dir_all(&root);
        let _ = std::fs::remove_dir_all(&outside);
    }
}
