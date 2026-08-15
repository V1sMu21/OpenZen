//! Side Panel state — ArtifactInfo and SidePanelState.

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

/// Runtime state of the Side Panel, protected by a Mutex in AppState.
#[derive(Debug, Clone)]
pub struct SidePanelState {
    pub visible: bool,
    /// Pixel width, clamped to [280, 800]
    pub width: u32,
    /// All currently open artifacts (tabs)
    pub artifacts: Vec<ArtifactInfo>,
    /// ID of the active (visible) artifact
    pub active_id: Option<String>,
}

impl SidePanelState {
    pub fn new() -> Self {
        Self {
            visible: false,
            width: 380,
            artifacts: Vec::new(),
            active_id: None,
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
}

impl Default for SidePanelState {
    fn default() -> Self {
        Self::new()
    }
}
