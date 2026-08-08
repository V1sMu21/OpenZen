//! Knowledge Crystallizer — LLM-driven knowledge extraction from agent sessions.
//!
//! After a successful agent run, the Crystallizer analyzes the conversation
//! and tool call sequence to extract reusable knowledge: Skills, SOPs, or Facts.
//!
//! Unlike the simple `tool_sequence` recording in `sop.rs`, this uses the LLM
//! to understand intent and surface reusable patterns.

use oz_core_types::{LlmClient, LlmError, Message, MockResponse};
use oz_skill_mcp::SkillMcpStore;
use oz_skill_mcp::skill::Skill;

/// Result of a crystallization attempt.
#[derive(Debug, Clone)]
pub enum CrystallizeResult {
    /// A new skill was created.
    SkillCreated { name: String },
    /// A new SOP was created.
    SopCreated { name: String },
    /// A fact was added to memory.
    FactAdded { content: String },
    /// Nothing worth crystallizing.
    Nothing,
}

/// Analyzes agent sessions and crystallizes reusable knowledge.
///
/// The Crystallizer sends the conversation history to an LLM with a
/// specialized prompt asking it to identify reusable patterns.
pub struct Crystallizer;

impl Crystallizer {
    /// Analyze a completed agent session and crystallize any reusable knowledge.
    ///
    /// Requires:
    /// - `client` — the LLM client to use for analysis
    /// - `store` — the knowledge store to write results into
    /// - `user_input` — the original user request
    /// - `messages` — full conversation history
    /// - `tool_sequence` — ordered list of tool calls made
    /// - `session_id` — optional session identifier
    pub async fn crystallize<C: LlmClient>(
        client: &mut C,
        store: &mut SkillMcpStore,
        user_input: &str,
        _messages: &[Message],
        tool_sequence: &[(String, serde_json::Value)],
        session_id: Option<String>,
    ) -> Result<Vec<CrystallizeResult>, LlmError> {
        if tool_sequence.len() < 3 {
            return Ok(vec![CrystallizeResult::Nothing]);
        }

        let prompt = Self::build_crystallize_prompt(user_input, tool_sequence);

        let analysis_messages = vec![
            Message::system("You are a knowledge extraction expert. Analyze the conversation and identify reusable patterns. Respond in JSON."),
            Message::user(&prompt),
        ];

        let response: MockResponse = client.chat(&analysis_messages, &[]).await?;

        let results = Self::parse_crystallize_response(&response.content, store, tool_sequence, session_id);

        Ok(results)
    }

    /// Build the LLM prompt for crystallization analysis.
    fn build_crystallize_prompt(user_input: &str, tool_sequence: &[(String, serde_json::Value)]) -> String {
        let mut prompt = String::new();
        prompt.push_str("Analyze this agent session and identify reusable knowledge.\n\n");

        prompt.push_str(&format!("**User Request:** {}\n\n", user_input));

        prompt.push_str("**Tool Call Sequence:**\n");
        for (i, (tool, args)) in tool_sequence.iter().enumerate() {
            let args_summary = simplify_args_for_prompt(args);
            prompt.push_str(&format!("  {}. `{}` — {}\n", i + 1, tool, args_summary));
        }

        prompt.push_str("\n**Instructions:**\n");
        prompt.push_str("1. If this session represents a **reusable capability**, write a SKILL definition.\n");
        prompt.push_str("2. If it's a **specific procedure**, write a SOP.\n");
        prompt.push_str("3. If there's a **notable fact**, extract it.\n");
        prompt.push_str("4. If nothing is reusable, return empty.\n\n");

        prompt.push_str("Respond in this JSON format:\n");
        prompt.push_str("```json\n");
        prompt.push_str(r#"{"skills": [{"name": "...", "description": "...", "tags": [...], "procedure": "..."}], "sops": [{"name": "...", "description": "...", "tags": [...], "steps": "..."}], "facts": ["fact 1", "fact 2"]}"#);
        prompt.push_str("\n```\n");

        prompt
    }

    /// Parse the LLM's crystallization response and write to the knowledge store.
    fn parse_crystallize_response(
        content: &str,
        store: &mut SkillMcpStore,
        tool_sequence: &[(String, serde_json::Value)],
        session_id: Option<String>,
    ) -> Vec<CrystallizeResult> {
        let mut results = Vec::new();

        // Try to extract JSON from the response
        let json_str = extract_json(content).unwrap_or(content.to_string());

        if let Ok(parsed) = serde_json::from_str::<serde_json::Value>(&json_str) {
            // Process skills
            if let Some(skills) = parsed.get("skills").and_then(|v| v.as_array()) {
                for skill_json in skills {
                    let name = skill_json.get("name").and_then(|v| v.as_str()).unwrap_or("unnamed");
                    let description = skill_json.get("description").and_then(|v| v.as_str()).unwrap_or("");
                    let tags: Vec<String> = skill_json.get("tags")
                        .and_then(|v| v.as_array())
                        .map(|arr| arr.iter().filter_map(|t| t.as_str().map(String::from)).collect())
                        .unwrap_or_default();
                    let procedure = skill_json.get("procedure").and_then(|v| v.as_str()).unwrap_or("");

                    let content = format!("# {} — {}\n\n{}\n", name, description, procedure);

                    let skill = Skill {
                        name: name.to_string(),
                        description: description.to_string(),
                        tags,
                        required_tools: Vec::new(),
                        content,
                        source_path: std::path::PathBuf::new(),
                        metadata: oz_core_types::SkillMcpMetadata::new(name, description, vec![]),
                        quality: 0.6,
                    };

                    if let Err(e) = store.skills.register(skill) {
                        tracing::warn!("Failed to register crystallized skill '{}': {}", name, e);
                    } else {
                        results.push(CrystallizeResult::SkillCreated { name: name.to_string() });
                    }
                }
            }

            // Process SOPs
            if let Some(sops) = parsed.get("sops").and_then(|v| v.as_array()) {
                for sop_json in sops {
                    let name = sop_json.get("name").and_then(|v| v.as_str()).unwrap_or("unnamed");
                    let description = sop_json.get("description").and_then(|v| v.as_str()).unwrap_or("");

                    let _ = store.crystallise_sop(name, description, tool_sequence, session_id.clone());
                    results.push(CrystallizeResult::SopCreated { name: name.to_string() });
                }
            }

            // Process facts
            if let Some(facts) = parsed.get("facts").and_then(|v| v.as_array()) {
                for fact in facts {
                    if let Some(fact_str) = fact.as_str() {
                        results.push(CrystallizeResult::FactAdded { content: fact_str.to_string() });
                    }
                }
            }
        }

        if results.is_empty() {
            results.push(CrystallizeResult::Nothing);
        }

        results
    }
}

/// Simplify tool args into a brief summary for the LLM prompt.
fn simplify_args_for_prompt(args: &serde_json::Value) -> String {
    match args {
        serde_json::Value::Object(map) => {
            let keys: Vec<&str> = map.keys().map(|k| k.as_str()).collect();
            if keys.len() <= 3 {
                keys.join(", ")
            } else {
                format!("{} fields", keys.len())
            }
        }
        serde_json::Value::String(s) => {
            if s.len() > 60 {
                format!("{}...", &s[..57])
            } else {
                s.clone()
            }
        }
        _ => String::new(),
    }
}

/// Extract JSON content from an LLM response that may be wrapped in markdown.
fn extract_json(content: &str) -> Option<String> {
    // Look for ```json ... ``` block
    if let Some(start) = content.find("```json") {
        let rest = &content[start + 7..];
        if let Some(end) = rest.find("```") {
            return Some(rest[..end].trim().to_string());
        }
    }
    // Look for bare JSON object
    let trimmed = content.trim();
    if trimmed.starts_with('{') && trimmed.ends_with('}') {
        return Some(trimmed.to_string());
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_simplify_args_small_object() {
        let args = serde_json::json!({"path": "/tmp/test", "pattern": "hello"});
        let summary = simplify_args_for_prompt(&args);
        assert!(summary.contains("path"));
        assert!(summary.contains("pattern"));
    }

    #[test]
    fn test_simplify_args_long_string() {
        let long = "a".repeat(100);
        let args = serde_json::json!({"code": long});
        let summary = simplify_args_for_prompt(&args);
        assert!(summary.len() <= 63);
    }

    #[test]
    fn test_extract_json_from_block() {
        let content = "Here's the result:\n```json\n{\"skills\": []}\n```\nDone.";
        let json = extract_json(content).unwrap();
        assert_eq!(json, "{\"skills\": []}");
    }

    #[test]
    fn test_extract_json_bare() {
        let content = "{\"skills\": [], \"sops\": []}";
        let json = extract_json(content).unwrap();
        assert_eq!(json, content);
    }

    #[test]
    fn test_extract_json_none() {
        assert!(extract_json("Just some text").is_none());
    }

    #[test]
    fn test_build_prompt_contains_tools() {
        let seq = vec![
            ("read".into(), serde_json::json!({"path": "/tmp"})),
            ("grep".into(), serde_json::json!({"pattern": "test"})),
        ];
        let prompt = Crystallizer::build_crystallize_prompt("test request", &seq);
        assert!(prompt.contains("read"));
        assert!(prompt.contains("grep"));
        assert!(prompt.contains("test request"));
    }

    #[test]
    fn test_parse_empty_response() {
        let dir = tempfile::tempdir().unwrap();
        let mut store = SkillMcpStore::new(dir.path(), None);
        let seq: Vec<(String, serde_json::Value)> = vec![];
        let results = Crystallizer::parse_crystallize_response("", &mut store, &seq, None);
        assert_eq!(results.len(), 1);
        assert!(matches!(results[0], CrystallizeResult::Nothing));
    }

    #[test]
    fn test_parse_crystallize_skills() {
        let dir = tempfile::tempdir().unwrap();
        let mut store = SkillMcpStore::new(dir.path(), None);
        let json = r#"{"skills": [{"name": "web_search_skill", "description": "Search the web", "tags": ["web"], "procedure": "1. Search\n2. Return"}], "sops": [], "facts": []}"#;
        let seq = vec![("read".into(), serde_json::json!({}))];
        let results = Crystallizer::parse_crystallize_response(json, &mut store, &seq, None);
        assert_eq!(results.len(), 1);
        assert!(matches!(results[0], CrystallizeResult::SkillCreated { .. }));
    }
}
