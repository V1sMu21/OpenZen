//! Skill system — SKILL.md loading, matching, and tracking.
//!
//! Skills are capability definitions following the opencode SKILL.md format.
//! Each skill lives in `.skill_mcp/skills/{name}/SKILL.md` with
//! companion `.skill_mcp/skills/{name}/meta.toml` for usage tracking.

use std::path::{Path, PathBuf};

use oz_core_types::SkillMcpMetadata;

use crate::meta::MetaStore;
use crate::SkillMcpError;

// ── Skill data model ──

/// A loaded skill — parsed from SKILL.md.
#[derive(Debug, Clone)]
pub struct Skill {
    /// Canonical name (directory name / first heading).
    pub name: String,
    /// Short description (subtitle after ` — ` in heading).
    pub description: String,
    /// Searchable tags.
    pub tags: Vec<String>,
    /// Required tool names (builtin or MCP).
    pub required_tools: Vec<String>,
    /// Full markdown body content.
    pub content: String,
    /// Disk path to the SKILL.md file.
    pub source_path: PathBuf,
    /// Usage & quality metadata.
    pub metadata: SkillMcpMetadata,
    /// Computed quality score (0.0–1.0).
    pub quality: f32,
}

impl Skill {
    /// Score this skill against a query string.
    /// Returns a relevance score 0.0–1.0.
    pub fn match_score(&self, query: &str) -> f32 {
        let query_lower = query.to_lowercase();
        let terms: Vec<&str> = query_lower.split_whitespace().collect();
        if terms.is_empty() {
            return 0.0;
        }

        let mut score: f32 = 0.0;

        // Name match: strongest signal
        let name_lower = self.name.to_lowercase();
        for term in &terms {
            if name_lower.contains(term) {
                score += 0.5;
            }
        }

        // Description match: moderate signal
        let desc_lower = self.description.to_lowercase();
        for term in &terms {
            if desc_lower.contains(term) {
                score += 0.3;
            }
        }

        // Tag match: moderate signal
        let tags_lower: Vec<String> = self.tags.iter().map(|t| t.to_lowercase()).collect();
        for term in &terms {
            for tag in &tags_lower {
                if tag.contains(term) || *term == tag.as_str() {
                    score += 0.25;
                }
            }
        }

        // Content match: weak signal (keyword in body)
        let content_lower = self.content.to_lowercase();
        for term in &terms {
            if content_lower.contains(term) {
                score += 0.1;
            }
        }

        // Reverse match: query contains tag (essential for Chinese)
        for tag in &self.tags {
            if !tag.is_empty() && query_lower.contains(&tag.to_lowercase()) {
                score += 0.2;
            }
        }

        // Normalize: cap at 1.0
        score.min(1.0)
    }

    /// Build a prompt snippet for injection into the system prompt.
    pub fn to_prompt_snippet(&self) -> String {
        let mut s = String::new();
        s.push_str(&format!("### Skill: {}\n", self.name));
        if !self.description.is_empty() {
            s.push_str(&format!("**{}**\n", self.description));
        }
        if !self.tags.is_empty() {
            s.push_str(&format!("*Tags: {}*\n", self.tags.join(", ")));
        }
        if !self.required_tools.is_empty() {
            s.push_str(&format!("*Tools: {}*\n", self.required_tools.join(", ")));
        }
        s.push_str(&format!(
            "*Use `skill_mcp_search(\"{}\")` to load full content ({} chars).*\n",
            self.name, self.content.len()
        ));
        s
    }

    /// Full body content for on-demand loading via skill_mcp_search.
    pub fn to_full_content(&self) -> String {
        let mut s = String::new();
        s.push_str(&format!("## {}\n\n", self.name));
        s.push_str(&self.content);
        s
    }
}

// ── SKILL.md Parser ──

/// Parse a SKILL.md file into a [`Skill`] struct.
///
/// Supports TWO formats:
///
/// 1. Modern YAML frontmatter (opencode standard):
/// ```markdown
/// ---
/// name: skill-name
/// description: Short description
/// ---
///
/// # Optional body heading
/// ## Procedure
/// ...
/// ```
///
/// 2. Legacy opencode-compatible format:
/// ```markdown
/// # skill-name — Short description
/// Tags: tag1, tag2
///
/// ## When to Use
/// ...
///
/// ## Required Tools
/// - tool_name (MCP: server/tool)
/// - another_tool
///
/// ## Procedure
/// ...
/// ```
pub fn parse_skill_md(path: &Path, content: &str) -> Result<Skill, SkillMcpError> {
    let content = content.trim();
    if content.is_empty() {
        return Err(SkillMcpError::InvalidFormat("Empty SKILL.md".into()));
    }

    // ── 1. Try YAML frontmatter first (modern opencode format) ──
    let (frontmatter_name, frontmatter_description, body_after_fm) =
        parse_yaml_frontmatter(content);

    let lines: Vec<&str> = content.lines().collect();

    // ── 2. Extract name/description from frontmatter OR legacy heading ──
    let (name, description) = if let Some(fm_name) = frontmatter_name {
        // YAML frontmatter present — use it as authoritative source
        let desc = frontmatter_description.unwrap_or_default();
        (fm_name, desc)
    } else {
        // Legacy format: extract name from first `# ` heading
        lines
            .iter()
            .find(|l| l.starts_with("# "))
            .map(|h| {
                let rest = h.trim_start_matches("# ").trim();
                if let Some(pos) = rest.find(" — ") {
                    let n = rest[..pos].trim().to_string();
                    let d = rest[pos + 5..].trim().to_string();
                    (n, d)
                } else if let Some(pos) = rest.find(" - ") {
                    let n = rest[..pos].trim().to_string();
                    let d = rest[pos + 3..].trim().to_string();
                    (n, d)
                } else {
                    (rest.to_string(), String::new())
                }
            })
            .unwrap_or_else(|| {
                let stem = path
                    .parent()
                    .and_then(|p| p.file_name())
                    .and_then(|n| n.to_str())
                    .unwrap_or("unknown");
                (stem.to_string(), String::new())
            })
    };

    // ── 3. Extract tags: frontmatter tags[] OR body `Tags:` line ──
    let tags: Vec<String> = if let Some(fm) = frontmatter_tags_from(content) {
        fm
    } else {
        lines
            .iter()
            .find(|l| l.to_lowercase().starts_with("tags:"))
            .map(|l| {
                l.trim_start_matches("Tags:")
                    .trim_start_matches("tags:")
                    .split(',')
                    .map(|t| t.trim().to_string())
                    .filter(|t| !t.is_empty())
                    .collect()
            })
            .unwrap_or_default()
    };

    // ── 4. Extract required tools from `## Required Tools` section ──
    let required_tools = if let Some(ref body) = body_after_fm {
        extract_required_tools(&body.lines().collect::<Vec<_>>())
    } else {
        extract_required_tools(&lines)
    };

    Ok(Skill {
        name,
        description,
        tags,
        required_tools,
        content: content.to_string(),
        source_path: path.to_path_buf(),
        metadata: SkillMcpMetadata::new("", "", vec![]),
        quality: 0.5,
    })
}

/// Parse YAML frontmatter at the top of a SKILL.md file.
/// Returns (name, description, body_after_frontmatter).
fn parse_yaml_frontmatter(content: &str) -> (Option<String>, Option<String>, Option<String>) {
    let trimmed = content.trim_start();
    if !trimmed.starts_with("---") {
        return (None, None, None);
    }

    // Find the closing `---` line
    let after_open = trimmed.strip_prefix("---").unwrap_or("");
    // Skip optional newline after `---`
    let after_open = after_open.trim_start_matches('\n').trim_start_matches("\r\n");

    let close_pos = after_open.find("\n---");
    let (yaml_block, body) = match close_pos {
        Some(p) => {
            let yaml = &after_open[..p];
            let rest = &after_open[p + 4..]; // skip "\n---"
            let rest = rest.trim_start_matches('\n').trim_start_matches("\r\n");
            (yaml, rest.to_string())
        }
        None => return (None, None, None),
    };

    // Lightweight YAML key:value parser (no external dep, handles 99% of cases)
    let mut name: Option<String> = None;
    let mut description: Option<String> = None;

    for line in yaml_block.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        if let Some(colon_pos) = line.find(':') {
            let key = line[..colon_pos].trim();
            let raw_val = line[colon_pos + 1..].trim();
            // Strip surrounding quotes (single or double)
            let val = strip_yaml_value_quotes(raw_val);
            match key.to_lowercase().as_str() {
                "name" => name = Some(val),
                "description" => description = Some(val),
                _ => {}
            }
        }
    }

    (name, description, Some(body))
}

fn strip_yaml_value_quotes(s: &str) -> String {
    let s = s.trim();
    if s.len() >= 2 {
        let first = s.chars().next().unwrap();
        let last = s.chars().last().unwrap();
        if (first == '"' && last == '"') || (first == '\'' && last == '\'') {
            return s[1..s.len() - 1].to_string();
        }
    }
    s.to_string()
}

/// Extract tags[] list from YAML frontmatter.
fn frontmatter_tags_from(content: &str) -> Option<Vec<String>> {
    let trimmed = content.trim_start();
    if !trimmed.starts_with("---") {
        return None;
    }
    let after_open = trimmed.strip_prefix("---").unwrap_or("");
    let after_open = after_open.trim_start_matches('\n').trim_start_matches("\r\n");
    let close_pos = after_open.find("\n---")?;
    let yaml_block = &after_open[..close_pos];

    for line in yaml_block.lines() {
        let line = line.trim();
        let lower = line.to_lowercase();
        if lower.starts_with("tags:") || lower.starts_with("tags :") {
            let val = line[line.find(':').unwrap() + 1..].trim();
            // Inline array form: [tag1, tag2, tag3]
            if val.starts_with('[') && val.ends_with(']') {
                let inner = &val[1..val.len() - 1];
                let tags: Vec<String> = inner
                    .split(',')
                    .map(|t| strip_yaml_value_quotes(t.trim()).to_string())
                    .filter(|t| !t.is_empty())
                    .collect();
                return Some(tags);
            }
            // Comma-separated form: tag1, tag2, tag3
            let tags: Vec<String> = val
                .split(',')
                .map(|t| strip_yaml_value_quotes(t.trim()).to_string())
                .filter(|t| !t.is_empty())
                .collect();
            return Some(tags);
        }
    }
    None
}

/// Extract tool names from the `## Required Tools` section.
fn extract_required_tools(lines: &[&str]) -> Vec<String> {
    let mut in_section = false;
    let mut tools = Vec::new();

    for line in lines {
        let trimmed = line.trim();
        if trimmed.starts_with("## Required Tools") || trimmed.starts_with("## Tools") {
            in_section = true;
            continue;
        }
        if in_section {
            // Stop at the next heading or empty section
            if trimmed.starts_with("## ") {
                break;
            }
            if trimmed.is_empty() && tools.is_empty() {
                continue;
            }
            // Parse `- tool_name (MCP: server/tool)` or `- tool_name`
            if let Some(tool_line) = trimmed.strip_prefix("- ") {
                // Extract just the tool name (before any parentheses)
                let tool_name = tool_line
                    .split('(')
                    .next()
                    .unwrap_or(tool_line)
                    .trim()
                    .to_string();
                if !tool_name.is_empty() {
                    tools.push(tool_name);
                }
            }
        }
    }

    tools
}

// ── Skill Manager ──

/// Manages the collection of loaded skills, providing matching and tracking.
pub struct SkillManager {
    skills: Vec<Skill>,
    skills_dir: PathBuf,
    meta_store: MetaStore,
}

impl SkillManager {
    /// Create a new manager. Loads all skills from `base_dir/skills/`.
    pub fn new(base_dir: &Path) -> Self {
        let skills_dir = base_dir.join("skills");
        let meta_store = MetaStore::new(base_dir);
        let mut mgr = SkillManager {
            skills: Vec::new(),
            skills_dir,
            meta_store,
        };
        let _ = mgr.load_all();
        mgr
    }

    /// Load all SKILL.md files from the skills directory.
    ///
    /// Three cases are handled, in order of preference:
    /// 1. `<dir>/SKILL.md` — the canonical opencode/SKILL.md format
    ///    (with YAML frontmatter or legacy heading). Most skills.
    /// 2. `<dir>/meta.toml` only — directory has no SKILL.md but does
    ///    have a meta.toml with the skill's id/description. We
    ///    synthesize a minimal Skill from the metadata so search/list
    ///    still surface the artifact. (Happens when a user copies
    ///    only the meta.toml between machines.)
    /// 3. Subdirectories of the skills root — walk one level deeper.
    ///    This recovers skills that were accidentally nested two
    ///    levels deep (e.g. `Agent Skill: Principal UI/UX Architect &
    ///    Motion Choreographer (Awwwards-Tier)/meta.toml`).
    pub fn load_all(&mut self) -> Result<usize, SkillMcpError> {
        if !self.skills_dir.exists() {
            return Ok(0);
        }

        let mut count = 0;
        for entry in std::fs::read_dir(&self.skills_dir)? {
            let entry = entry?;
            let path = entry.path();
            if !path.is_dir() {
                continue;
            }
            count += self.load_from_dir(&path)?;
            // One-level recursive scan for accidentally-nested skills.
            if let Ok(sub) = std::fs::read_dir(&path) {
                for sub_entry in sub.flatten() {
                    let sub_path = sub_entry.path();
                    if sub_path.is_dir() {
                        count += self.load_from_dir(&sub_path)?;
                    }
                }
            }
        }
        Ok(count)
    }

    /// Try to load a single skill from a directory. Tries SKILL.md
    /// first, then falls back to meta.toml-only synthesis.
    fn load_from_dir(&mut self, dir: &Path) -> Result<usize, SkillMcpError> {
        let mut count = 0;
        let skill_md = dir.join("SKILL.md");
        if skill_md.exists() {
            if let Ok(skill) = self.load_skill(&skill_md) {
                self.upsert_skill(skill);
                count += 1;
                return Ok(count);
            }
        }
        // Fallback: directory has only meta.toml. Synthesize a minimal
        // Skill from the metadata so it still shows up in search/list.
        let meta_toml = dir.join("meta.toml");
        if meta_toml.exists() {
            // Derive a stable name from the directory's file name.
            let dir_name = dir
                .file_name()
                .and_then(|n| n.to_str())
                .unwrap_or("unnamed")
                .to_string();
            // Sanitize: lowercase, replace path-hostile chars.
            let canonical = dir_name.to_lowercase().replace([' ', ':'], "-");
            if let Ok(Some(meta)) = self.meta_store.load("skills", &canonical) {
                let synth = Skill {
                    name: meta.id.clone(),
                    description: meta.description.clone(),
                    tags: meta.tags.clone(),
                    required_tools: vec![],
                    content: format!(
                        "# {}\n\n{}\n\n*This skill was registered from meta.toml only — SKILL.md is missing. The system prompt should reference the description above.*",
                        meta.id, meta.description
                    ),
                    source_path: meta_toml.clone(),
                    metadata: meta.clone(),
                    quality: meta.quality_score,
                };
                self.upsert_skill(synth);
                count += 1;
            }
        }
        Ok(count)
    }

    /// Insert or replace a Skill by name, merging any existing meta.toml.
    fn upsert_skill(&mut self, mut skill: Skill) {
        if let Ok(Some(meta)) = self.meta_store.load("skills", &skill.name) {
            skill.metadata = meta;
            skill.quality = skill.metadata.quality_score;
        } else {
            skill.metadata = SkillMcpMetadata::new(
                &skill.name,
                &skill.description,
                skill.tags.clone(),
            );
            let _ = self.meta_store.save("skills", &skill.name, &skill.metadata);
        }
        if let Some(existing) = self.skills.iter_mut().find(|s| s.name == skill.name) {
            *existing = skill;
        } else {
            self.skills.push(skill);
        }
    }

    /// Load a single SKILL.md file.
    fn load_skill(&self, path: &Path) -> Result<Skill, SkillMcpError> {
        let content = std::fs::read_to_string(path).map_err(SkillMcpError::Io)?;
        parse_skill_md(path, &content)
    }

    /// Find skills matching a query, sorted by relevance.
    pub fn find_matching(&self, query: &str) -> Vec<&Skill> {
        let mut scored: Vec<(&Skill, f32)> = self
            .skills
            .iter()
            .filter(|s| s.metadata.is_active())
            .map(|s| {
                let score = s.match_score(query);
                (s, score)
            })
            .filter(|(_, score)| *score > 0.0)
            .collect();

        scored.sort_by(|(_, a), (_, b)| {
            b.partial_cmp(a).unwrap_or(std::cmp::Ordering::Equal)
        });

        scored.into_iter().map(|(s, _)| s).collect()
    }

    /// Get top N matching skills and build a prompt snippet.
    pub fn build_prompt_snippet(&self, query: &str, max_skills: usize) -> String {
        let matched = self.find_matching(query);
        if matched.is_empty() {
            return String::new();
        }

        let mut snippet = String::from("\n\n## 🔴 EXECUTION ORDER — perform these steps with tools NOW\n\n");
        snippet.push_str("The following skill matches your task. You MUST execute every step using your tools — do NOT describe the steps, perform them.\n\n");

        for (i, skill) in matched.iter().take(max_skills).enumerate() {
            snippet.push_str(&format!("--- Skill {}: {} ---\n\n", i + 1, skill.name));
            snippet.push_str(&skill.to_prompt_snippet());
            snippet.push('\n');
        }
        snippet
    }

    /// Record a successful usage of a skill.
    pub fn record_success(&mut self, name: &str, turns: u32) -> Result<(), SkillMcpError> {
        if let Some(skill) = self.skills.iter_mut().find(|s| s.name == name) {
            skill.metadata.record_success(turns);
            skill.quality = skill.metadata.quality_score;
            self.meta_store.save("skills", name, &skill.metadata)?;
        }
        Ok(())
    }

    /// Record a failed usage of a skill.
    pub fn record_failure(&mut self, name: &str) -> Result<(), SkillMcpError> {
        if let Some(skill) = self.skills.iter_mut().find(|s| s.name == name) {
            skill.metadata.record_failure();
            skill.quality = skill.metadata.quality_score;
            self.meta_store.save("skills", name, &skill.metadata)?;
        }
        Ok(())
    }

    /// Register a new skill (or merge with existing by name).
    pub fn register(&mut self, skill: Skill) -> Result<(), SkillMcpError> {
        let name = skill.name.clone();
        // Write SKILL.md
        let skill_dir = self.skills_dir.join(&name);
        std::fs::create_dir_all(&skill_dir)?;
        let skill_md_path = skill_dir.join("SKILL.md");
        std::fs::write(&skill_md_path, &skill.content)?;

        // Merge or create metadata
        if let Some(existing) = self.skills.iter_mut().find(|s| s.name == name) {
            existing.metadata.record_success(0);
            existing.quality = existing.metadata.quality_score;
            existing.content = skill.content;
            existing.description = skill.description;
            existing.tags = skill.tags.clone();
            existing.required_tools = skill.required_tools;
            self.meta_store.save("skills", &name, &existing.metadata)?;
        } else {
            let mut meta = SkillMcpMetadata::new(&name, &skill.description, skill.tags.clone());
            meta.record_success(0);
            let mut new_skill = skill;
            new_skill.metadata = meta;
            new_skill.quality = new_skill.metadata.quality_score;
            self.meta_store.save("skills", &name, &new_skill.metadata)?;
            self.skills.push(new_skill);
        }
        Ok(())
    }

    /// Get a skill by name.
    pub fn get(&self, name: &str) -> Option<&Skill> {
        self.skills.iter().find(|s| s.name == name)
    }

    /// List all loaded skills.
    pub fn list(&self) -> &[Skill] {
        &self.skills
    }

    /// Number of loaded skills.
    pub fn len(&self) -> usize {
        self.skills.len()
    }

    /// Whether no skills are loaded.
    pub fn is_empty(&self) -> bool {
        self.skills.is_empty()
    }

    /// Get mutable reference to a skill (for testing/refinement).
    pub fn get_mut(&mut self, name: &str) -> Option<&mut Skill> {
        self.skills.iter_mut().find(|s| s.name == name)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_skill_md_basic() {
        let md = r#"# web_search — Search the web for information
Tags: web, search

## When to Use
- When you need current information from the internet

## Required Tools
- web_search (MCP: playwright/search)

## Procedure
1. Formulate a search query
2. Call web_search with the query
3. Review and summarize results
"#;
        let path = PathBuf::from("/fake/skills/web_search/SKILL.md");
        let skill = parse_skill_md(&path, md).unwrap();

        assert_eq!(skill.name, "web_search");
        assert_eq!(skill.description, "Search the web for information");
        assert_eq!(skill.tags, vec!["web", "search"]);
        assert_eq!(skill.required_tools, vec!["web_search"]);
        assert!(skill.content.contains("## Procedure"));
    }

    #[test]
    fn test_parse_skill_md_no_heading() {
        let md = "## Some content\nNo heading\n";
        let path = PathBuf::from("/fake/skills/test_skill/SKILL.md");
        let skill = parse_skill_md(&path, md).unwrap();
        // Falls back to parent directory name
        assert_eq!(skill.name, "test_skill");
    }

    #[test]
    fn test_parse_skill_md_yaml_frontmatter() {
        // Modern opencode format used by all installed skills in .skill_mcp/skills/
        let md = r#"---
name: design-taste-frontend
description: Senior UI/UX Engineer. Architect digital interfaces overriding default LLM biases.
---

# High-Agency Frontend Skill

## 1. ACTIVE BASELINE CONFIGURATION
* DESIGN_VARIANCE: 8

## Required Tools
- grep
- read_file

## Procedure
1. Formulate
2. Execute
"#;
        let path = PathBuf::from("/fake/skills/taste-skill/SKILL.md");
        let skill = parse_skill_md(&path, md).unwrap();

        // YAML frontmatter takes priority over the body's # heading
        assert_eq!(skill.name, "design-taste-frontend");
        assert!(skill.description.starts_with("Senior UI/UX Engineer"));
        assert_eq!(skill.required_tools, vec!["grep", "read_file"]);
    }

    #[test]
    fn test_parse_skill_md_yaml_frontmatter_with_tags() {
        let md = r#"---
name: image-to-code
description: Elite website image-to-code skill for Codex.
tags: [frontend, codex, image]
---

# Image to Code

## Procedure
1. Generate image
"#;
        let path = PathBuf::from("/fake/skills/image-to-code-skill/SKILL.md");
        let skill = parse_skill_md(&path, md).unwrap();
        assert_eq!(skill.name, "image-to-code");
        assert_eq!(skill.tags, vec!["frontend", "codex", "image"]);
    }

    #[test]
    fn test_parse_skill_md_yaml_match_score() {
        let md = r#"---
name: design-taste-frontend
description: Senior UI/UX Engineer
---

# Body
"#.to_string();
        let path = PathBuf::from("/fake/skills/taste-skill/SKILL.md");
        let skill = parse_skill_md(&path, &md).unwrap();
        // Searching for "taste" should now match via the YAML name field,
        // not just via content. Score must be > 0 (≥ 0.5 from name match).
        let score = skill.match_score("taste design");
        assert!(
            score >= 0.5,
            "expected 'taste' to match YAML name; got score={score}"
        );
    }

    #[test]
    fn test_parse_skill_md_empty_is_error() {
        let md = "";
        let path = PathBuf::from("/fake/skills/empty/SKILL.md");
        assert!(parse_skill_md(&path, md).is_err());
    }

    #[test]
    fn test_extract_required_tools() {
        let lines = vec![
            "## Required Tools",
            "- tool_a (MCP: server/a)",
            "- tool_b",
            "- tool_c (some explanation here)",
            "",
            "## Next Section",
        ];
        let tools = extract_required_tools(&lines);
        assert_eq!(tools, vec!["tool_a", "tool_b", "tool_c"]);
    }

    #[test]
    fn test_skill_match_score_name() {
        let skill = Skill {
            name: "web_search".into(),
            description: "Search the web".into(),
            tags: vec![],
            required_tools: vec![],
            content: String::new(),
            source_path: PathBuf::new(),
            metadata: SkillMcpMetadata::new("web_search", "", vec![]),
            quality: 0.8,
        };

        let score = skill.match_score("search the internet");
        assert!(score > 0.3, "should match on 'search' in name");
    }

    #[test]
    fn test_skill_match_score_tags() {
        let skill = Skill {
            name: "data_export".into(),
            description: "Export data".into(),
            tags: vec!["csv".into(), "export".into(), "data".into()],
            required_tools: vec![],
            content: String::new(),
            source_path: PathBuf::new(),
            metadata: SkillMcpMetadata::new("data_export", "", vec![]),
            quality: 0.8,
        };

        let score = skill.match_score("csv file");
        assert!(score > 0.0, "should match on 'csv' tag");
    }

    #[test]
    fn test_skill_match_score_no_match() {
        let skill = Skill {
            name: "web_search".into(),
            description: "Search the web".into(),
            tags: vec!["web".into()],
            required_tools: vec![],
            content: String::new(),
            source_path: PathBuf::new(),
            metadata: SkillMcpMetadata::new("web_search", "", vec![]),
            quality: 0.8,
        };

        let score = skill.match_score("database migration postgres");
        assert_eq!(score, 0.0, "should not match unrelated query");
    }

    #[test]
    fn test_skill_to_prompt_snippet() {
        let skill = Skill {
            name: "test".into(),
            description: "A test".into(),
            tags: vec!["test".into()],
            required_tools: vec!["tool_x".into()],
            content: "Step 1\nStep 2\n".into(),
            source_path: PathBuf::new(),
            metadata: SkillMcpMetadata::new("test", "", vec![]),
            quality: 0.8,
        };

        let snippet = skill.to_prompt_snippet();
        assert!(snippet.contains("Skill: test"));
        assert!(snippet.contains("Tags: test"));
        assert!(snippet.contains("Tools: tool_x"));
        assert!(snippet.contains("skill_mcp_search")); // L1 hint to load full content

        // Full content via to_full_content()
        let full = skill.to_full_content();
        assert!(full.contains("Step 1"));
    }

    #[test]
    fn test_skill_manager_load_and_find() {
        let dir = tempfile::tempdir().unwrap();
        let skills_dir = dir.path().join("skills").join("web_search");
        std::fs::create_dir_all(&skills_dir).unwrap();

        let md = r#"# web_search — Search the web
Tags: web, search

## Procedure
1. Search
2. Return results
"#;
        std::fs::write(skills_dir.join("SKILL.md"), md).unwrap();

        let mut mgr = SkillManager::new(dir.path());
        mgr.load_all().unwrap();

        assert_eq!(mgr.len(), 1);
        let matches = mgr.find_matching("search for information");
        assert_eq!(matches.len(), 1);
        assert_eq!(matches[0].name, "web_search");

        // No match for unrelated query
        let no_match = mgr.find_matching("completely unrelated");
        assert!(no_match.is_empty());
    }

    #[test]
    fn test_skill_manager_record_and_quality() {
        let dir = tempfile::tempdir().unwrap();
        let skills_dir = dir.path().join("skills").join("test_skill");
        std::fs::create_dir_all(&skills_dir).unwrap();
        std::fs::write(skills_dir.join("SKILL.md"), "# test_skill — Test\nTags: test\n\n## Procedure\n1. Do\n").unwrap();

        let mut mgr = SkillManager::new(dir.path());

        // Record successes
        mgr.record_success("test_skill", 5).unwrap();
        mgr.record_success("test_skill", 3).unwrap();

        let skill = mgr.get("test_skill").unwrap();
        assert_eq!(skill.metadata.success_count, 2);
        assert!(skill.metadata.quality_score > 0.5);

        // Record failures
        mgr.record_failure("test_skill").unwrap();
        let skill = mgr.get("test_skill").unwrap();
        assert_eq!(skill.metadata.failure_count, 1);
    }

    #[test]
    fn test_skill_manager_register_new() {
        let dir = tempfile::tempdir().unwrap();
        let mut mgr = SkillManager::new(dir.path());

        let skill = Skill {
            name: "registered_skill".into(),
            description: "A registered test".into(),
            tags: vec!["test".into()],
            required_tools: vec!["grep".into()],
            content: "# registered_skill — A registered test\n\n## Procedure\n1. Step\n".into(),
            source_path: PathBuf::new(),
            metadata: SkillMcpMetadata::new("registered_skill", "", vec![]),
            quality: 0.5,
        };

        mgr.register(skill).unwrap();
        assert_eq!(mgr.len(), 1);

        // SKILL.md should exist on disk
        let skill_md = dir.path().join("skills").join("registered_skill").join("SKILL.md");
        assert!(skill_md.exists());

        // meta.toml should exist
        let meta = dir.path().join("skills").join("registered_skill").join("meta.toml");
        assert!(meta.exists());
    }

    #[test]
    fn test_extract_required_tools_no_section() {
        let lines = vec!["# Test", "", "No tools section here"];
        assert!(extract_required_tools(&lines).is_empty());
    }
}
