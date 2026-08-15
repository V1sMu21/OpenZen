//! SOP management — loading, matching, crystallization, and refinement.
//!
//! Migrated and unified from:
//! - `oz-core/src/sop.rs` — SopStore (loading, matching, crystallisation)
//! - `ga-memory/src/lib.rs` — L3 SOP handling
//!
//! SOPs now live in `.skill_mcp/sops/*.md` with optional `.skill_mcp/sops/meta.toml` index.

use std::path::{Path, PathBuf};

use oz_core_types::SkillMcpMetadata;

use crate::meta::MetaStore;
use crate::SkillMcpError;

/// A standard operating procedure.
#[derive(Debug, Clone)]
pub struct Sop {
    pub name: String,
    pub description: String,
    pub tags: Vec<String>,
    /// Full markdown body.
    pub content: String,
    /// Source file path.
    pub source_path: PathBuf,
    /// Usage & quality metadata.
    pub metadata: SkillMcpMetadata,
}

impl Sop {
    /// Parse a SOP from a markdown file path and content.
    ///
    /// Expected format:
    /// ```markdown
    /// # Name — Short description
    /// Tags: tag1, tag2
    ///
    /// Body content...
    /// ```
    pub fn from_markdown(path: &Path, content: &str) -> Self {
        let content = content.trim();
        let lines: Vec<&str> = content.lines().collect();

        let (name, description) = lines
            .iter()
            .find(|l| l.starts_with("# "))
            .map(|h| {
                let rest = h.trim_start_matches("# ").trim();
                if let Some(dash) = rest.find(" — ") {
                    let n = rest[..dash].trim().to_string();
                    let d = rest[dash + 5..].trim().to_string();
                    (n, d)
                } else if let Some(dash) = rest.find(" - ") {
                    let n = rest[..dash].trim().to_string();
                    let d = rest[dash + 3..].trim().to_string();
                    (n, d)
                } else {
                    (rest.to_string(), String::new())
                }
            })
            .unwrap_or_else(|| {
                let stem = path
                    .file_stem()
                    .and_then(|s| s.to_str())
                    .unwrap_or("unknown");
                (stem.to_string(), String::new())
            });

        let tags: Vec<String> = lines
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
            .unwrap_or_default();

        Sop {
            name,
            description,
            tags,
            content: content.to_string(),
            source_path: path.to_path_buf(),
            metadata: SkillMcpMetadata::new("", "", vec![]),
        }
    }

    /// Render the SOP back to markdown for file storage.
    pub fn to_markdown(&self) -> String {
        let mut md = String::new();
        md.push_str(&format!("# {} — {}\n", self.name, self.description));
        if !self.tags.is_empty() {
            md.push_str(&format!("Tags: {}\n", self.tags.join(", ")));
        }
        md.push('\n');
        // Remove the first heading line from content to avoid duplication
        let body = if let Some(first_newline) = self.content.find('\n') {
            let remaining = &self.content[first_newline + 1..];
            remaining.strip_prefix('\n').unwrap_or(remaining)
        } else {
            &self.content
        };
        md.push_str(body);
        if !md.ends_with('\n') {
            md.push('\n');
        }
        md
    }

    /// Score this SOP against a query.
    pub fn match_score(&self, query: &str) -> f32 {
        let query_lower = query.to_lowercase();
        let terms: Vec<&str> = query_lower.split_whitespace().collect();
        if terms.is_empty() {
            return 0.0;
        }

        let mut score: f32 = 0.0;
        let haystack = format!(
            "{} {} {}",
            self.name.to_lowercase(),
            self.description.to_lowercase(),
            self.tags.join(" ").to_lowercase()
        );
        let haystack_words: Vec<&str> = haystack.split_whitespace().collect();

        for term in &terms {
            if haystack.contains(term) {
                score += 0.3;
            }
        }

        for tag in &self.tags {
            if query_lower.contains(&tag.to_lowercase()) && !tag.is_empty() {
                score += 0.2;
            }
        }

        if score < 0.15 {
            for term in &terms {
                if term.len() < 4 {
                    continue;
                }
                for hw in &haystack_words {
                    if hw.len() >= 2 && term.contains(hw) {
                        score += 0.15;
                    }
                }
            }
        }

        score.min(1.0)
    }
}

// ── SOP Manager ──

/// Manages SOPs: loading, matching, crystallization, persistence.
pub struct SopManager {
    sops: Vec<Sop>,
    sops_dir: PathBuf,
    meta_store: MetaStore,
}

impl SopManager {
    /// Create a new manager. Loads all SOPs from `base_dir/sops/`.
    pub fn new(base_dir: &Path) -> Self {
        let sops_dir = base_dir.join("sops");
        let meta_store = MetaStore::new(base_dir);
        let mut mgr = SopManager {
            sops: Vec::new(),
            sops_dir,
            meta_store,
        };
        let _ = mgr.load_all();
        mgr
    }

    /// Load all `.md` SOP files from the sops directory.
    pub fn load_all(&mut self) -> Result<usize, SkillMcpError> {
        if !self.sops_dir.exists() {
            return Ok(0);
        }

        let mut count = 0;
        for entry in std::fs::read_dir(&self.sops_dir)? {
            let entry = entry?;
            let path = entry.path();
            if path.extension().and_then(|e| e.to_str()) == Some("md") {
                if let Ok(data) = std::fs::read_to_string(&path) {
                    let mut sop = Sop::from_markdown(&path, &data);

                    // Load metadata from meta.toml if available
                    let meta_key = path
                        .file_stem()
                        .and_then(|s| s.to_str())
                        .unwrap_or(&sop.name);
                    if let Ok(Some(meta)) = self.meta_store.load("sops", meta_key) {
                        sop.metadata = meta;
                    } else {
                        sop.metadata =
                            SkillMcpMetadata::new(&sop.name, &sop.description, sop.tags.clone());
                        let _ = self.meta_store.save("sops", &sop.name, &sop.metadata);
                    }

                    // Merge by name
                    if let Some(existing) = self.sops.iter_mut().find(|s| s.name == sop.name) {
                        *existing = sop;
                    } else {
                        self.sops.push(sop);
                    }
                    count += 1;
                }
            }
        }
        Ok(count)
    }

    /// Find SOPs matching a query string.
    pub fn find_matching(&self, query: &str) -> Vec<&Sop> {
        let mut scored: Vec<(&Sop, f32)> = self
            .sops
            .iter()
            .filter(|s| s.metadata.is_active())
            .map(|s| {
                let score = s.match_score(query);
                (s, score)
            })
            .filter(|(_, score)| *score > 0.0)
            .collect();

        scored.sort_by(|(_, a), (_, b)| b.partial_cmp(a).unwrap_or(std::cmp::Ordering::Equal));

        scored.into_iter().map(|(s, _)| s).collect()
    }

    /// Build a prompt snippet from matching SOPs.
    pub fn build_prompt_snippet(&self, query: &str, max_sops: usize) -> String {
        let matched = self.find_matching(query);
        if matched.is_empty() {
            return String::new();
        }

        let mut snippet =
            String::from("\n\n## 🔴 EXECUTION ORDER — perform these steps with tools NOW\n\n");
        snippet.push_str("The following SOP matches your task. You MUST execute every step using your tools — do NOT describe the procedure, perform each action.\n\n");

        for sop in matched.iter().take(max_sops) {
            snippet.push_str(&format!("### {}\n", sop.name));
            if !sop.description.is_empty() {
                snippet.push_str(&format!("**{}**\n\n", sop.description));
            }
            if !sop.tags.is_empty() {
                snippet.push_str(&format!("*Tags: {}*\n\n", sop.tags.join(", ")));
            }
            snippet.push_str(&sop.content);
            snippet.push_str("\n---\n\n");
        }
        snippet
    }

    /// Save a SOP as a markdown file and persist metadata.
    pub fn save(&self, sop: &Sop) -> Result<(), SkillMcpError> {
        std::fs::create_dir_all(&self.sops_dir)?;
        let safe_name = sanitize_name(&sop.name);
        let path = self.sops_dir.join(format!("{safe_name}.md"));
        let md = sop.to_markdown();
        std::fs::write(&path, &md)?;

        // Save metadata under the SOP name (not the safe filename)
        self.meta_store.save("sops", &sop.name, &sop.metadata)?;
        Ok(())
    }

    /// Register a SOP in-memory and persist to disk.
    pub fn register(&mut self, sop: Sop) -> Result<(), SkillMcpError> {
        let name = sop.name.clone();
        if let Some(existing) = self.sops.iter_mut().find(|s| s.name == name) {
            existing.metadata.record_success(0);
            existing.content = sop.content;
            existing.description = sop.description;
            existing.tags = sop.tags;
            let updated = existing.clone();
            self.save(&updated)
        } else {
            self.sops.push(sop.clone());
            self.save(&sop)
        }
    }

    /// Crystallize a tool call sequence into a new SOP.
    pub fn crystallise(
        &mut self,
        name: &str,
        description: &str,
        tool_sequence: &[(String, serde_json::Value)],
        session_id: Option<String>,
    ) -> Result<Sop, SkillMcpError> {
        let mut content = String::new();
        content.push_str(&format!("# {} — {}\n\n", name, description));
        content.push_str(&format!("**Purpose:** {}\n\n", description));
        content.push_str("## Steps\n\n");

        for (i, (tool, args)) in tool_sequence.iter().enumerate() {
            content.push_str(&format!("### {}. `{}`\n", i + 1, tool));
            content.push_str("\n**Arguments:**\n\n");
            content.push_str("```json\n");
            content.push_str(
                &serde_json::to_string_pretty(&simplify_args(args))
                    .unwrap_or_else(|_| "{}".to_string()),
            );
            content.push_str("\n```\n\n");
        }

        content.push_str("## Notes\n\n");
        content.push_str("- Auto-crystallized from agent run");
        if let Some(ref sid) = session_id {
            content.push_str(&format!(" (session: {})", sid));
        }
        content.push('\n');

        let tags = extract_tags_from_tools(tool_sequence);

        let mut meta = SkillMcpMetadata::new(name, description, tags.clone());
        meta.source_session = session_id;
        meta.record_success(0);

        let sop = Sop {
            name: name.to_string(),
            description: description.to_string(),
            tags,
            content,
            source_path: self.sops_dir.join(format!("{}.md", sanitize_name(name))),
            metadata: meta,
        };

        self.register(sop.clone())?;
        Ok(sop)
    }

    /// Record a successful usage.
    pub fn record_success(&mut self, name: &str, turns: u32) -> Result<(), SkillMcpError> {
        if let Some(sop) = self.sops.iter_mut().find(|s| s.name == name) {
            sop.metadata.record_success(turns);
            self.meta_store.save("sops", name, &sop.metadata)?;
        }
        Ok(())
    }

    /// Get all SOPs.
    pub fn all(&self) -> &[Sop] {
        &self.sops
    }

    /// Number of SOPs.
    pub fn len(&self) -> usize {
        self.sops.len()
    }

    /// Whether no SOPs are loaded.
    pub fn is_empty(&self) -> bool {
        self.sops.is_empty()
    }

    /// Get a SOP by name.
    pub fn get(&self, name: &str) -> Option<&Sop> {
        self.sops.iter().find(|s| s.name == name)
    }
}

// ── Helpers ──

fn sanitize_name(s: &str) -> String {
    s.chars()
        .map(|c| {
            if c.is_alphanumeric() || c == '-' || c == '_' {
                c
            } else {
                '_'
            }
        })
        .collect::<String>()
        .trim_matches('_')
        .to_lowercase()
}

fn simplify_args(args: &serde_json::Value) -> serde_json::Value {
    match args {
        serde_json::Value::Object(map) => {
            let mut simplified = serde_json::Map::new();
            for (k, v) in map {
                match v {
                    serde_json::Value::String(s) if s.len() > 80 => {
                        simplified.insert(k.clone(), serde_json::Value::String("<code>".into()));
                    }
                    serde_json::Value::Array(items) => {
                        let sampled: Vec<serde_json::Value> =
                            items.iter().take(3).map(simplify_args).collect();
                        simplified.insert(k.clone(), serde_json::Value::Array(sampled));
                    }
                    _ => {
                        simplified.insert(k.clone(), v.clone());
                    }
                }
            }
            serde_json::Value::Object(simplified)
        }
        serde_json::Value::Array(items) => {
            serde_json::Value::Array(items.iter().take(3).map(simplify_args).collect())
        }
        other => other.clone(),
    }
}

fn extract_tags_from_tools(seq: &[(String, serde_json::Value)]) -> Vec<String> {
    let mut tags: Vec<String> = Vec::new();
    for (tool, _) in seq {
        let tag = match tool.as_str() {
            "read" | "write" | "edit" | "patch" | "grep" | "glob" | "ls" => "file-operations",
            "web_search" | "web_fetch" | "web_scan" | "web_js" => "web",
            "code_run" | "bash" => "code-execution",
            "ask_user" | "respond" => "interaction",
            _ => continue,
        };
        if !tags.iter().any(|t| t == tag) {
            tags.push(tag.to_string());
        }
    }
    tags.sort();
    tags
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_sop_from_markdown() {
        let md = r#"# web_search — Search the web
Tags: web, search

## Usage
Use web_search to find current information.
"#;
        let path = PathBuf::from("web_search.md");
        let sop = Sop::from_markdown(&path, md);
        assert_eq!(sop.name, "web_search");
        assert_eq!(sop.description, "Search the web");
        assert_eq!(sop.tags, vec!["web", "search"]);
    }

    #[test]
    fn test_sop_to_markdown_roundtrip() {
        let md = "# test_sop — Test\nTags: test\n\nBody here.\n";
        let path = PathBuf::from("test_sop.md");
        let sop = Sop::from_markdown(&path, md);
        let rendered = sop.to_markdown();
        assert!(rendered.contains("Test"));
        assert!(rendered.contains("Tags: test"));
    }

    #[test]
    fn test_sop_manager_load_and_find() {
        let dir = tempfile::tempdir().unwrap();
        let sops_dir = dir.path().join("sops");
        std::fs::create_dir_all(&sops_dir).unwrap();

        std::fs::write(
            sops_dir.join("test_sop.md"),
            "# test_sop — A test SOP\nTags: test\n\nDo something.\n",
        )
        .unwrap();

        let mgr = SopManager::new(dir.path());
        assert_eq!(mgr.len(), 1);
        let found = mgr.find_matching("test sop");
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].name, "test_sop");
    }

    #[test]
    fn test_crystallise_creates_sop() {
        let dir = tempfile::tempdir().unwrap();
        let mut mgr = SopManager::new(dir.path());

        let sequence = vec![
            (
                "read".to_string(),
                serde_json::json!({"path": "/etc/hosts"}),
            ),
            (
                "grep".to_string(),
                serde_json::json!({"pattern": "localhost"}),
            ),
        ];

        let sop = mgr
            .crystallise(
                "check_hosts",
                "Check hosts file",
                &sequence,
                Some("sess_abc".into()),
            )
            .unwrap();

        assert_eq!(sop.name, "check_hosts");
        assert!(sop.content.contains("read"));
        assert!(sop.content.contains("grep"));
        assert!(sop.tags.contains(&"file-operations".to_string()));

        // Should be persisted
        let sops_dir = dir.path().join("sops");
        assert!(sops_dir.join("check_hosts.md").exists());
    }

    #[test]
    fn test_sanitize_name() {
        assert_eq!(sanitize_name("Hello World!"), "hello_world");
        assert_eq!(sanitize_name("test-123"), "test-123");
    }

    #[test]
    fn test_simplify_args_long_string() {
        let long = "a".repeat(200);
        let args = serde_json::json!({"code": long, "short": "ok"});
        let simplified = simplify_args(&args);
        assert_eq!(simplified["code"], "<code>");
        assert_eq!(simplified["short"], "ok");
    }
}
