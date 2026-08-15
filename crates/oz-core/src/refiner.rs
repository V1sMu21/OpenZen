//! Knowledge Refiner — LLM-driven improvement of existing knowledge artifacts.
//!
//! Periodically evaluates and improves skills and SOPs based on usage data.
//! Triggers when success_count exceeds a threshold or quality drops below minimum.

use oz_core_types::{LlmClient, LlmError, Message, MockResponse};
use oz_skill_mcp::SkillMcpStore;

/// Trigger conditions for refinement.
#[derive(Debug, Clone)]
pub struct RefineTrigger {
    /// Refine every N successful uses.
    pub every_n_uses: u32,
    /// Refine when quality score drops below this threshold.
    pub min_quality: f32,
    /// Maximum number of refinements per artifact.
    pub max_refinements: u32,
}

impl Default for RefineTrigger {
    fn default() -> Self {
        RefineTrigger {
            every_n_uses: 5,
            min_quality: 0.4,
            max_refinements: 10,
        }
    }
}

/// Result of a refinement attempt.
#[derive(Debug, Clone)]
pub enum RefineResult {
    Refined { name: String, old_version: u32, new_version: u32 },
    Skipped { name: String, reason: String },
}

/// Analyzes existing knowledge and proposes improvements via LLM.
pub struct Refiner;

impl Refiner {
    /// Check if a skill should be refined based on trigger conditions.
    pub fn should_refine(success_count: u32, quality: f32, version: u32, trigger: &RefineTrigger) -> bool {
        version < trigger.max_refinements
            && (success_count > 0 && success_count.is_multiple_of(trigger.every_n_uses)
                || quality < trigger.min_quality)
    }

    /// Refine a skill by asking the LLM to improve its SKILL.md content.
    pub async fn refine_skill<C: LlmClient>(
        client: &mut C,
        store: &mut SkillMcpStore,
        skill_name: &str,
    ) -> Result<RefineResult, LlmError> {
        let skill = match store.skills.get(skill_name).cloned() {
            Some(s) => s,
            None => return Ok(RefineResult::Skipped {
                name: skill_name.to_string(),
                reason: "skill not found".into(),
            }),
        };

        let trigger = RefineTrigger::default();
        if !Self::should_refine(
            skill.metadata.success_count,
            skill.metadata.quality_score,
            skill.metadata.version,
            &trigger,
        ) {
            return Ok(RefineResult::Skipped {
                name: skill_name.to_string(),
                reason: "trigger conditions not met".into(),
            });
        }

        let prompt = Self::build_refine_prompt(&skill);

        let messages = vec![
            Message::system("You are a knowledge refinement expert. Improve the given skill definition."),
            Message::user(&prompt),
        ];

        let response: MockResponse = client.chat(&messages, &[]).await?;
        let refined_content = response.content.trim().to_string();

        if refined_content.is_empty() || refined_content == skill.content {
            return Ok(RefineResult::Skipped {
                name: skill_name.to_string(),
                reason: "no improvements suggested".into(),
            });
        }

        // Update the skill
        let old_version = skill.metadata.version;
        let mut updated = skill;
        updated.content = refined_content;
        updated.metadata.bump_version();

        let new_version = updated.metadata.version;
        store.skills.register(updated).map_err(|e| {
            LlmError::Custom(format!("Failed to save refined skill: {}", e))
        })?;

        Ok(RefineResult::Refined {
            name: skill_name.to_string(),
            old_version,
            new_version,
        })
    }

    /// Build the refinement prompt for the LLM.
    fn build_refine_prompt(skill: &oz_skill_mcp::skill::Skill) -> String {
        let mut prompt = String::new();
        prompt.push_str("Improve the following skill definition based on its usage history.\n\n");

        prompt.push_str(&format!("**Current Version:** v{}\n", skill.metadata.version));
        prompt.push_str(&format!("**Success Count:** {}\n", skill.metadata.success_count));
        prompt.push_str(&format!("**Failure Count:** {}\n", skill.metadata.failure_count));
        prompt.push_str(&format!("**Current Quality Score:** {:.2}\n", skill.metadata.quality_score));
        prompt.push_str(&format!("**Average Completion Turns:** {:.1}\n\n", skill.metadata.avg_completion_turns));

        prompt.push_str("**Current SKILL.md:**\n");
        prompt.push_str("```markdown\n");
        prompt.push_str(&skill.content);
        prompt.push_str("\n```\n\n");

        prompt.push_str("**Instructions:**\n");
        prompt.push_str("1. Keep the same general structure (# name — description, Tags, ## sections).\n");
        prompt.push_str("2. Add missing steps, clarify ambiguous instructions.\n");
        prompt.push_str("3. Remove unnecessary or outdated steps.\n");
        prompt.push_str("4. If the procedure is already optimal, return it unchanged.\n");
        prompt.push_str("5. Output ONLY the refined markdown, no explanations.\n");

        prompt
    }

    /// Run refinement checks on all skills in a knowledge store.
    pub async fn refine_all_skills<C: LlmClient>(
        client: &mut C,
        store: &mut SkillMcpStore,
    ) -> Result<Vec<RefineResult>, LlmError> {
        let names: Vec<String> = store.skills.list().iter().map(|s| s.name.clone()).collect();
        let mut results = Vec::new();

        for name in names {
            match Self::refine_skill(client, store, &name).await {
                Ok(result) => results.push(result),
                Err(e) => {
                    tracing::warn!("Refinement failed for skill '{}': {}", name, e);
                }
            }
        }

        Ok(results)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_should_refine_every_5_uses() {
        let trigger = RefineTrigger::default();
        assert!(Refiner::should_refine(5, 0.8, 1, &trigger));
        assert!(!Refiner::should_refine(3, 0.8, 1, &trigger));
        assert!(Refiner::should_refine(10, 0.8, 1, &trigger));
    }

    #[test]
    fn test_should_refine_low_quality() {
        let trigger = RefineTrigger::default();
        assert!(Refiner::should_refine(1, 0.3, 1, &trigger));
        assert!(!Refiner::should_refine(1, 0.5, 1, &trigger));
    }

    #[test]
    fn test_should_refine_max_refinements() {
        let trigger = RefineTrigger::default();
        assert!(!Refiner::should_refine(5, 0.8, 10, &trigger));
        assert!(Refiner::should_refine(5, 0.8, 9, &trigger));
    }

    #[test]
    fn test_trigger_defaults() {
        let trigger = RefineTrigger::default();
        assert_eq!(trigger.every_n_uses, 5);
        assert_eq!(trigger.min_quality, 0.4);
        assert_eq!(trigger.max_refinements, 10);
    }

    #[test]
    fn test_build_refine_prompt_contains_skill_info() {
        let mut skill = oz_skill_mcp::skill::Skill {
            name: "test_skill".into(),
            description: "A test".into(),
            tags: vec![],
            required_tools: vec![],
            content: "# test_skill\n\nDo something.\n".into(),
            source_path: std::path::PathBuf::new(),
            metadata: oz_core_types::SkillMcpMetadata::new("test_skill", "", vec![]),
            quality: 0.7,
        };
        skill.metadata.success_count = 5;

        let prompt = Refiner::build_refine_prompt(&skill);
        assert!(prompt.contains("test_skill"));
        assert!(prompt.contains("5"));
        assert!(prompt.contains("Do something"));
    }
}
