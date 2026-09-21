//! Side Panel state — ArtifactInfo and SidePanelState.

use std::collections::HashMap;

use serde::{Deserialize, Serialize};

/// Metadata for a single artifact (file) opened in the Side Panel.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ArtifactInfo {
    pub id: String,
    /// "html", "pdf", "code", "spreadsheet", "markdown", "image", "office", "terminal"
    #[serde(rename = "type")]
    pub artifact_type: String,
    /// Absolute path to the file on disk
    pub path: String,
    /// Display label (file name or user-provided)
    pub label: String,
}

/// One session's side-panel tabs, parked while another session is active.
/// Only the tabs travel with the session: visibility and width are window
/// layout and stay put across a switch.
#[derive(Debug, Clone, Default)]
pub struct ParkedTabs {
    pub artifacts: Vec<ArtifactInfo>,
    pub active_id: Option<String>,
}

/// Runtime state of the Side Panel, protected by a Mutex in AppState.
#[derive(Debug, Clone)]
pub struct SidePanelState {
    pub visible: bool,
    /// Pixel width, clamped to [280, 960]
    pub width: u32,
    /// All currently open artifacts (tabs)
    pub artifacts: Vec<ArtifactInfo>,
    /// ID of the active (visible) artifact
    pub active_id: Option<String>,
    /// Session the current tab set belongs to (None until the frontend
    /// binds one). Tabs are per-session: switching conversations parks the
    /// outgoing set in AppState.sidepanel_sessions and restores the
    /// incoming one, so A's artifacts never show up over B — and coming
    /// back to A finds its tabs still open.
    pub session_id: Option<String>,
}

impl SidePanelState {
    pub fn new() -> Self {
        Self {
            visible: false,
            // 380 was unreadable for html/pdf previews — users resized every
            // time. 560 fits a typical article/pdf page; drag range below.
            width: 560,
            artifacts: Vec::new(),
            active_id: None,
            session_id: None,
        }
    }

    /// Find the index of the active artifact in the artifacts vec.
    pub fn active_index(&self) -> Option<usize> {
        self.active_id
            .as_ref()
            .and_then(|id| self.artifacts.iter().position(|a| &a.id == id))
    }

    /// Move to the previous tab (wraps around).
    pub fn prev_tab(&mut self) {
        if self.artifacts.is_empty() {
            return;
        }
        let idx = self.active_index().unwrap_or(0);
        let new_idx = if idx == 0 {
            self.artifacts.len() - 1
        } else {
            idx - 1
        };
        self.active_id = Some(self.artifacts[new_idx].id.clone());
    }

    /// Move to the next tab (wraps around).
    pub fn next_tab(&mut self) {
        if self.artifacts.is_empty() {
            return;
        }
        let idx = self.active_index().unwrap_or(0);
        let new_idx = (idx + 1) % self.artifacts.len();
        self.active_id = Some(self.artifacts[new_idx].id.clone());
    }

    /// Remove an artifact by tab index. Returns the removed artifact or None.
    pub fn remove_tab(&mut self, index: usize) -> Option<ArtifactInfo> {
        if index >= self.artifacts.len() {
            return None;
        }
        let removed = self.artifacts.remove(index);
        // Adjust active_id if needed
        if self.active_id.as_ref() == Some(&removed.id) {
            if self.artifacts.is_empty() {
                self.active_id = None;
            } else {
                let new_idx = index.min(self.artifacts.len() - 1);
                self.active_id = Some(self.artifacts[new_idx].id.clone());
            }
        }
        Some(removed)
    }

    /// Clear all artifacts (e.g., on session switch).
    pub fn clear(&mut self) {
        self.artifacts.clear();
        self.active_id = None;
    }

    /// Point the panel at `session_id`: park the outgoing session's tabs in
    /// `parked` and adopt the incoming session's set (empty when that
    /// session never opened any). Visibility and width are left alone — the
    /// panel stays open across a switch, and returning to a session brings
    /// its tabs back. Re-binding the same session is a no-op.
    pub fn bind_session(&mut self, session_id: &str, parked: &mut HashMap<String, ParkedTabs>) {
        if self.session_id.as_deref() == Some(session_id) {
            return;
        }
        if let Some(prev) = self.session_id.replace(session_id.to_string()) {
            if self.artifacts.is_empty() {
                // Nothing to restore later; drop a stale spot instead of
                // keeping an empty entry per visited session.
                parked.remove(&prev);
            } else {
                let tabs = ParkedTabs {
                    artifacts: std::mem::take(&mut self.artifacts),
                    active_id: self.active_id.take(),
                };
                parked.insert(prev, tabs);
            }
        }
        let tabs = parked.remove(session_id).unwrap_or_default();
        self.artifacts = tabs.artifacts;
        self.active_id = tabs.active_id;
    }
}

impl Default for SidePanelState {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn artifact(id: &str) -> ArtifactInfo {
        ArtifactInfo {
            id: id.to_string(),
            artifact_type: "html".into(),
            path: format!("/tmp/{id}.html"),
            label: id.to_string(),
        }
    }

    fn state_with(session: &str, ids: &[&str]) -> SidePanelState {
        let mut sp = SidePanelState::new();
        sp.session_id = Some(session.to_string());
        sp.artifacts = ids.iter().map(|i| artifact(i)).collect();
        sp.active_id = ids.last().map(|i| i.to_string());
        sp
    }

    #[test]
    fn tabs_come_back_when_returning_to_a_session() {
        let mut parked = HashMap::new();
        let mut sp = state_with("A", &["a1", "a2"]);
        sp.visible = true;

        sp.bind_session("B", &mut parked);
        assert!(sp.artifacts.is_empty(), "B must not inherit A's artifacts");
        assert!(sp.visible, "the panel itself stays open");
        assert_eq!(parked.len(), 1, "A's tabs are parked, not dropped");

        sp.artifacts = vec![artifact("b1")];
        sp.active_id = Some("b1".into());
        sp.bind_session("A", &mut parked);
        assert_eq!(sp.artifacts.len(), 2, "A's tabs are restored");
        assert_eq!(sp.active_id.as_deref(), Some("a2"));
        assert_eq!(parked.len(), 1, "B's tab is parked in exchange");
        assert_eq!(parked["B"].artifacts[0].id, "b1");
    }

    #[test]
    fn rebinding_the_same_session_keeps_the_tabs() {
        let mut parked = HashMap::new();
        let mut sp = state_with("A", &["a1"]);

        // ⌘[ wrap-around and re-selecting the current row both land here.
        sp.bind_session("A", &mut parked);
        assert_eq!(sp.artifacts.len(), 1);
        assert!(parked.is_empty());
    }

    #[test]
    fn empty_sessions_leave_no_parked_entry() {
        let mut parked = HashMap::new();
        let mut sp = state_with("A", &[]);

        sp.bind_session("B", &mut parked);
        sp.bind_session("A", &mut parked);
        assert!(parked.is_empty());
        assert!(sp.artifacts.is_empty());
    }

    #[test]
    fn unbound_panel_adopts_the_session_without_leaking_tabs() {
        let mut parked = HashMap::new();
        // No `bind_session` yet (e.g. an artifact opened during startup
        // restore): the tabs have no owner to park under and must not
        // follow the panel into B.
        let mut sp = SidePanelState::new();
        sp.artifacts = vec![artifact("orphan")];

        sp.bind_session("B", &mut parked);
        assert!(sp.artifacts.is_empty());
        assert!(parked.is_empty());
        assert_eq!(sp.session_id.as_deref(), Some("B"));
    }
}
