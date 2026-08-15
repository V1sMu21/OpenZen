//! Staleness Checker — detects and manages outdated knowledge artifacts.
//!
//! Periodically scans the knowledge store for artifacts that have become
//! stale: unused for too long, low quality scores, or too many versions.

use std::path::Path;

use oz_core_types::SkillMcpMetadata;

use crate::meta::MetaStore;
use crate::SkillMcpError;

/// Configuration for staleness detection.
#[derive(Debug, Clone)]
pub struct StalenessConfig {
    /// Mark as stale if not used for this many days.
    pub max_age_days: u32,
    /// Mark as stale if quality score drops below this threshold.
    pub min_quality_score: f32,
    /// Keep at most this many versions per artifact.
    pub max_versions: u32,
    /// Minimum number of uses before age-based staleness applies.
    pub min_uses_for_age_check: u32,
}

impl Default for StalenessConfig {
    fn default() -> Self {
        StalenessConfig {
            max_age_days: 90,
            min_quality_score: 0.3,
            max_versions: 10,
            min_uses_for_age_check: 3,
        }
    }
}

/// A stale artifact found during scanning.
#[derive(Debug, Clone)]
pub struct StaleItem {
    /// Which knowledge category.
    pub category: String,
    /// Artifact name or ID.
    pub name: String,
    /// Reason it was flagged as stale.
    pub reason: String,
    /// Current quality score.
    pub quality_score: f32,
    /// Days since last use (None if never used).
    pub days_since_last_use: Option<f32>,
    /// Recommended action.
    pub recommendation: StaleAction,
}

#[derive(Debug, Clone)]
pub enum StaleAction {
    /// Artifact should be reviewed and possibly refined.
    Review,
    /// Artifact should be removed or archived.
    Remove,
    /// Artifact needs version cleanup.
    Cleanup,
}

/// Scans knowledge artifacts and flags stale or low-quality ones.
pub struct StalenessChecker {
    meta_store: MetaStore,
    config: StalenessConfig,
}

impl StalenessChecker {
    pub fn new(base_dir: &Path, config: Option<StalenessConfig>) -> Self {
        StalenessChecker {
            meta_store: MetaStore::new(base_dir),
            config: config.unwrap_or_default(),
        }
    }

    /// Scan all categories for stale artifacts.
    pub fn scan_all(&self) -> Result<Vec<StaleItem>, SkillMcpError> {
        let mut items = Vec::new();
        for category in &["skills", "sops"] {
            let metas = self.meta_store.list_category(category)?;
            for meta in &metas {
                if let Some(item) = self.check_single(category, meta) {
                    items.push(item);
                }
            }
        }
        Ok(items)
    }

    /// Check a single artifact for staleness.
    fn check_single(&self, category: &str, meta: &SkillMcpMetadata) -> Option<StaleItem> {
        let name = meta.id.clone();

        // Check quality score
        if meta.quality_score < self.config.min_quality_score && meta.total_uses() > 0 {
            return Some(StaleItem {
                category: category.to_string(),
                name,
                reason: format!(
                    "quality score {:.2} below threshold {:.2}",
                    meta.quality_score, self.config.min_quality_score
                ),
                quality_score: meta.quality_score,
                days_since_last_use: days_since(&meta.last_used_at),
                recommendation: StaleAction::Review,
            });
        }

        // Check age-based staleness
        if meta.total_uses() >= self.config.min_uses_for_age_check {
            if let Some(ref last_used) = meta.last_used_at {
                if let Some(days) = days_since(&Some(last_used.clone())) {
                    if days > self.config.max_age_days as f32 {
                        return Some(StaleItem {
                            category: category.to_string(),
                            name,
                            reason: format!(
                                "not used for {:.0} days (threshold: {} days)",
                                days, self.config.max_age_days
                            ),
                            quality_score: meta.quality_score,
                            days_since_last_use: Some(days),
                            recommendation: StaleAction::Remove,
                        });
                    }
                }
            }
        }

        // Check version count
        if meta.version > self.config.max_versions {
            return Some(StaleItem {
                category: category.to_string(),
                name,
                reason: format!(
                    "{} versions accumulated (threshold: {})",
                    meta.version, self.config.max_versions
                ),
                quality_score: meta.quality_score,
                days_since_last_use: days_since(&meta.last_used_at),
                recommendation: StaleAction::Cleanup,
            });
        }

        None
    }

    /// Mark an artifact as stale and optionally remove it.
    pub fn mark_stale(&self, category: &str, name: &str) -> Result<(), SkillMcpError> {
        if let Some(mut meta) = self.meta_store.load(category, name)? {
            meta.stale_flag = true;
            self.meta_store.save(category, name, &meta)?;
        }
        Ok(())
    }

    /// Remove a stale artifact (delete meta.toml and content).
    pub fn remove_artifact(&self, category: &str, name: &str) -> Result<(), SkillMcpError> {
        // Delete meta.toml
        self.meta_store.delete(category, name)?;

        // Try to delete content directory/files
        let dir = self.meta_store.meta_path(category, name);
        if let Some(parent) = dir.parent() {
            // For skills: remove the whole directory
            if category == "skills" {
                let _ = std::fs::remove_dir_all(parent);
            }
            // For sops: remove the .md file
            if category == "sops" {
                for entry in std::fs::read_dir(parent)
                    .unwrap_or_else(|_| std::fs::read_dir(".").unwrap())
                    .flatten()
                {
                    let path = entry.path();
                    if path.extension().and_then(|e| e.to_str()) == Some("md") {
                        let _ = std::fs::remove_file(&path);
                    }
                }
            }
        }
        Ok(())
    }

    /// Run full staleness scan and auto-remove artifacts recommended for removal.
    pub fn cleanup(&self, auto_remove: bool) -> Result<(Vec<StaleItem>, usize), SkillMcpError> {
        let items = self.scan_all()?;
        let mut removed = 0;

        if auto_remove {
            for item in &items {
                if matches!(item.recommendation, StaleAction::Remove)
                    && self.remove_artifact(&item.category, &item.name).is_ok()
                {
                    removed += 1;
                }
            }
        }

        Ok((items, removed))
    }
}

/// Calculate days since a given ISO 8601 timestamp.
fn days_since(timestamp: &Option<String>) -> Option<f32> {
    let ts = timestamp.as_ref()?;
    let parsed = chrono::DateTime::parse_from_rfc3339(ts).ok()?;
    let now = chrono::Utc::now();
    let duration = now.signed_duration_since(parsed);
    Some(duration.num_hours() as f32 / 24.0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use oz_core_types::SkillMcpMetadata;

    #[test]
    fn test_staleness_config_defaults() {
        let config = StalenessConfig::default();
        assert_eq!(config.max_age_days, 90);
        assert_eq!(config.min_quality_score, 0.3);
    }

    #[test]
    fn test_check_low_quality() {
        let dir = tempfile::tempdir().unwrap();
        let checker = StalenessChecker::new(dir.path(), None);

        let mut meta = SkillMcpMetadata::new("bad_skill", "desc", vec![]);
        meta.record_success(1); // bump quality to 0.65
        meta.quality_score = 0.2; // then drop below threshold

        let item = checker.check_single("skills", &meta).unwrap();
        assert!(item.reason.contains("quality"));
    }

    #[test]
    fn test_check_ok_quality_passes() {
        let dir = tempfile::tempdir().unwrap();
        let checker = StalenessChecker::new(dir.path(), None);

        let meta = SkillMcpMetadata::new("good_skill", "desc", vec![]);
        // quality 0.5 > 0.3, no usage yet → no age check

        assert!(checker.check_single("skills", &meta).is_none());
    }

    #[test]
    fn test_scan_empty_directory() {
        let dir = tempfile::tempdir().unwrap();
        let checker = StalenessChecker::new(dir.path(), None);
        let items = checker.scan_all().unwrap();
        assert!(items.is_empty());
    }

    #[test]
    fn test_days_since_none() {
        assert!(days_since(&None).is_none());
    }

    #[test]
    fn test_stale_action_enum() {
        let review = StaleAction::Review;
        let remove = StaleAction::Remove;
        let cleanup = StaleAction::Cleanup;
        assert!(matches!(review, StaleAction::Review));
        assert!(matches!(remove, StaleAction::Remove));
        assert!(matches!(cleanup, StaleAction::Cleanup));
    }
}
