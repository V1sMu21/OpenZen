//! Skill / MCP registry type definitions — shared across ga-skill-mcp, ga-tools, oz-core.
//!
//! Defines the unified artifact model for skills, SOPs, facts, and insights.

use serde::{Deserialize, Serialize};

/// Category of a skill/MCP artifact.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Hash)]
#[serde(rename_all = "lowercase")]
pub enum SkillMcpType {
    /// A reusable capability definition (SKILL.md format).
    Skill,
    /// A standard operating procedure (structured steps).
    Sop,
    /// A persistent global fact (L2 memory).
    Fact,
    /// A distilled insight (L1 memory).
    Insight,
}

impl SkillMcpType {
    pub fn as_str(&self) -> &'static str {
        match self {
            SkillMcpType::Skill => "skill",
            SkillMcpType::Sop => "sop",
            SkillMcpType::Fact => "fact",
            SkillMcpType::Insight => "insight",
        }
    }

    pub fn as_dir(&self) -> &'static str {
        match self {
            SkillMcpType::Skill => "skills",
            SkillMcpType::Sop => "sops",
            SkillMcpType::Fact => "facts",
            SkillMcpType::Insight => "insights",
        }
    }
}

impl std::fmt::Display for SkillMcpType {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.as_str())
    }
}

/// Metadata tracking usage and quality of a knowledge artifact.
/// Stored in `.skill_mcp/{category}/{name}/meta.toml`.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct SkillMcpMetadata {
    /// Unique identifier (skill name, SOP name, fact UUID, etc.).
    pub id: String,
    /// ISO 8601 creation timestamp.
    pub created_at: String,
    /// ISO 8601 last-updated timestamp.
    pub updated_at: String,
    /// Session ID that created this artifact (if applicable).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source_session: Option<String>,
    /// Monotonic version counter.
    pub version: u32,
    /// Human-readable description.
    #[serde(default)]
    pub description: String,

    // ── Usage metrics ──
    /// Number of times this knowledge was used successfully.
    pub success_count: u32,
    /// Number of times usage resulted in failure.
    pub failure_count: u32,
    /// ISO 8601 timestamp of last usage.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_used_at: Option<String>,
    /// Average number of agent turns when this knowledge is used.
    #[serde(default)]
    pub avg_completion_turns: f32,
    /// Cached quality score (0.0–1.0). Recomputed on refinement.
    #[serde(default)]
    pub quality_score: f32,
    /// Whether a human has reviewed and approved this artifact.
    #[serde(default)]
    pub user_approved: bool,
    /// Flagged for review/retirement.
    #[serde(default)]
    pub stale_flag: bool,

    // ── Tags for matching ──
    /// Searchable keywords.
    #[serde(default)]
    pub tags: Vec<String>,
}

impl SkillMcpMetadata {
    /// Create a fresh metadata entry for a new knowledge artifact.
    pub fn new(id: &str, description: &str, tags: Vec<String>) -> Self {
        let now = chrono::Utc::now().to_rfc3339();
        SkillMcpMetadata {
            id: id.to_string(),
            created_at: now.clone(),
            updated_at: now,
            source_session: None,
            version: 1,
            description: description.to_string(),
            success_count: 0,
            failure_count: 0,
            last_used_at: None,
            avg_completion_turns: 0.0,
            quality_score: 0.5, // neutral initial score
            user_approved: false,
            stale_flag: false,
            tags,
        }
    }

    /// Record a successful usage, updating metrics.
    pub fn record_success(&mut self, turns: u32) {
        self.success_count += 1;
        self.last_used_at = Some(chrono::Utc::now().to_rfc3339());
        // Exponential moving average for turns
        if self.avg_completion_turns == 0.0 {
            self.avg_completion_turns = turns as f32;
        } else {
            self.avg_completion_turns = self.avg_completion_turns * 0.7 + turns as f32 * 0.3;
        }
        // Bump quality on success
        self.quality_score = (self.quality_score + 0.15).min(1.0);
    }

    /// Record a failed usage, decreasing quality score.
    pub fn record_failure(&mut self) {
        self.failure_count += 1;
        self.last_used_at = Some(chrono::Utc::now().to_rfc3339());
        self.quality_score = (self.quality_score - 0.1).max(0.0);
    }

    /// Bump version and update timestamp.
    pub fn bump_version(&mut self) {
        self.version += 1;
        self.updated_at = chrono::Utc::now().to_rfc3339();
    }

    /// Total usage count.
    pub fn total_uses(&self) -> u32 {
        self.success_count + self.failure_count
    }

    /// Whether this artifact should be considered in matching.
    pub fn is_active(&self) -> bool {
        !self.stale_flag && self.quality_score >= 0.3
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_knowledge_type_as_str() {
        assert_eq!(SkillMcpType::Skill.as_str(), "skill");
        assert_eq!(SkillMcpType::Sop.as_str(), "sop");
        assert_eq!(SkillMcpType::Fact.as_str(), "fact");
        assert_eq!(SkillMcpType::Insight.as_str(), "insight");
    }

    #[test]
    fn test_knowledge_type_as_dir() {
        assert_eq!(SkillMcpType::Skill.as_dir(), "skills");
        assert_eq!(SkillMcpType::Sop.as_dir(), "sops");
    }

    #[test]
    fn test_metadata_new() {
        let meta = SkillMcpMetadata::new("test_skill", "A test", vec!["test".into()]);
        assert_eq!(meta.id, "test_skill");
        assert_eq!(meta.version, 1);
        assert_eq!(meta.quality_score, 0.5);
        assert_eq!(meta.success_count, 0);
        assert!(meta.is_active());
    }

    #[test]
    fn test_metadata_record_success() {
        let mut meta = SkillMcpMetadata::new("t", "", vec![]);
        meta.record_success(5);
        assert_eq!(meta.success_count, 1);
        assert_eq!(meta.avg_completion_turns, 5.0);
        assert!(meta.quality_score > 0.5);
        assert!(meta.last_used_at.is_some());
    }

    #[test]
    fn test_metadata_record_failure() {
        let mut meta = SkillMcpMetadata::new("t", "", vec![]);
        meta.record_failure();
        assert_eq!(meta.failure_count, 1);
        assert!(meta.quality_score < 0.5);
    }

    #[test]
    fn test_metadata_ema() {
        let mut meta = SkillMcpMetadata::new("t", "", vec![]);
        meta.record_success(10);
        meta.record_success(2); // EMA: 10*0.7 + 2*0.3 = 7.6
        assert!((meta.avg_completion_turns - 7.6).abs() < 0.01);
    }

    #[test]
    fn test_metadata_stale_not_active() {
        let mut meta = SkillMcpMetadata::new("t", "", vec![]);
        meta.quality_score = 0.2;
        assert!(!meta.is_active());
        meta.stale_flag = true;
        assert!(!meta.is_active());
    }

    #[test]
    fn test_knowledge_type_serialization() {
        let kt = SkillMcpType::Skill;
        let json = serde_json::to_string(&kt).unwrap();
        assert_eq!(json, "\"skill\"");
        let deser: SkillMcpType = serde_json::from_str(&json).unwrap();
        assert_eq!(deser, SkillMcpType::Skill);
    }

    #[test]
    fn test_metadata_serialization_roundtrip() {
        let meta = SkillMcpMetadata::new("my_skill", "desc", vec!["a".into(), "b".into()]);
        let json = serde_json::to_string_pretty(&meta).unwrap();
        let deser: SkillMcpMetadata = serde_json::from_str(&json).unwrap();
        assert_eq!(deser.id, "my_skill");
        assert_eq!(deser.tags, vec!["a", "b"]);
    }
}
