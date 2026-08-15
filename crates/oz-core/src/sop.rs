use std::path::{Path, PathBuf};

/// A standard operating procedure — stored as markdown, read by the agent at runtime.
///
/// Modeled after the Python GenericAgent's memory/*.md SOP system:
/// Each SOP is a markdown file that describes how to perform a specific task.
/// The agent reads the relevant SOP(s) as context before/during execution.
#[derive(Debug, Clone)]
pub struct Sop {
    pub name: String,
    pub description: String,
    pub tags: Vec<String>,
    /// Full markdown body — the agent reads this as executable instructions.
    pub content: String,
    pub source_session: Option<String>,
    pub success_count: u32,
    pub created_at: String,
    pub updated_at: String,
}

impl Sop {
    /// Parse a SOP from a markdown file path and content.
    ///
    /// Format:
    /// ```markdown
    /// # Name — Short description
    /// Tags: tag1, tag2
    ///
    /// Body content...
    /// ```
    pub fn from_markdown(path: &Path, content: &str) -> Self {
        let content = content.trim();
        let lines: Vec<&str> = content.lines().collect();

        // Extract name + description from first `# ` heading
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

        // Extract tags from a `Tags:` line
        let tags: Vec<String> = lines
            .iter()
            .find(|l| l.to_lowercase().starts_with("tags:"))
            .map(|l| {
                l.trim_start_matches("tags:")
                    .trim_start_matches("Tags:")
                    .split(',')
                    .map(|t| t.trim().to_string())
                    .filter(|t| !t.is_empty())
                    .collect()
            })
            .unwrap_or_default();

        let content_body: String = lines
            .iter()
            .filter(|l| !l.to_lowercase().starts_with("tags:"))
            .copied()
            .collect::<Vec<_>>()
            .join("\n");

        Sop {
            name,
            description,
            tags,
            content: content_body,
            source_session: None,
            success_count: 1,
            created_at: chrono::Utc::now().to_rfc3339(),
            updated_at: chrono::Utc::now().to_rfc3339(),
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
        md.push_str(&self.content);
        if !md.ends_with('\n') {
            md.push('\n');
        }
        md
    }
}

// ── Legacy types for backward compatibility ──

/// A single step in a programmatic SOP (auto-crystallized from tool calls).
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct SopStep {
    pub tool: String,
    pub description: String,
    pub args_template: serde_json::Value,
}

/// Programmatic SOP — serializable representation for auto-crystallization.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct CrystalSop {
    pub name: String,
    pub description: String,
    pub steps: Vec<SopStep>,
    pub source_session: Option<String>,
    pub success_count: u32,
    pub created_at: String,
}

// ── SOP Store ──

/// Manages a directory of SOP (.md) files and provides runtime context injection.
///
/// Mirrors the Python GenericAgent's `memory/` directory approach:
/// - SOPs are loaded from a configurable directory (default `memory/`)
/// - At runtime, SOPs matching the current task are injected into the system prompt
/// - The agent reads the SOP markdown and follows the procedure
pub struct SopStore {
    sops: Vec<Sop>,
    storage_dir: PathBuf,
}

impl SopStore {
    /// Create a new store. If `storage_dir` exists, loads all SOPs from it.
    pub fn new(storage_dir: PathBuf) -> Self {
        let mut store = SopStore {
            sops: Vec::new(),
            storage_dir,
        };
        let _ = store.load_all();
        store
    }

    /// Load all `.md` SOP files from the storage directory.
    pub fn load_all(&mut self) -> Result<usize, String> {
        if !self.storage_dir.exists() {
            return Ok(0);
        }
        let mut count = 0;
        if let Ok(entries) = std::fs::read_dir(&self.storage_dir) {
            for entry in entries.flatten() {
                let path = entry.path();
                if path.extension().and_then(|e| e.to_str()) == Some("md") {
                    if let Ok(data) = std::fs::read_to_string(&path) {
                        let sop = Sop::from_markdown(&path, &data);
                        // Avoid duplicates — replace if name matches
                        if let Some(existing) = self.sops.iter_mut().find(|s| s.name == sop.name) {
                            existing.content = sop.content;
                            existing.description = sop.description;
                            existing.tags = sop.tags;
                            existing.updated_at = chrono::Utc::now().to_rfc3339();
                        } else {
                            self.sops.push(sop);
                        }
                        count += 1;
                    }
                }
            }
        }
        Ok(count)
    }

    /// Save a SOP as a markdown file in the storage directory.
    pub fn save(&self, sop: &Sop) -> Result<(), String> {
        std::fs::create_dir_all(&self.storage_dir)
            .map_err(|e| format!("Failed to create SOP dir: {e}"))?;
        let safe_name: String = sop
            .name
            .chars()
            .map(|c| {
                if c.is_alphanumeric() || c == '_' || c == '-' {
                    c
                } else {
                    '_'
                }
            })
            .collect();
        let path = self.storage_dir.join(format!("{}.md", safe_name));
        let md = sop.to_markdown();
        std::fs::write(&path, &md).map_err(|e| format!("Failed to write SOP: {e}"))?;
        Ok(())
    }

    /// Register a SOP in-memory and persist to disk.
    pub fn register(&mut self, sop: Sop) {
        if let Some(existing) = self.sops.iter_mut().find(|s| s.name == sop.name) {
            existing.success_count += 1;
            existing.content = sop.content;
            existing.description = sop.description;
            existing.tags = sop.tags;
            existing.updated_at = chrono::Utc::now().to_rfc3339();
        } else {
            self.sops.push(sop);
        }
    }

    /// Find SOPs matching a query string (name, description, or tags).
    pub fn find_matching(&self, query: &str) -> Vec<&Sop> {
        let query_lower = query.to_lowercase();
        let terms: Vec<&str> = query_lower.split_whitespace().collect();
        if terms.is_empty() {
            return Vec::new();
        }

        let mut results: Vec<&Sop> = self
            .sops
            .iter()
            .filter(|s| {
                let haystack = format!(
                    "{} {} {}",
                    s.name.to_lowercase(),
                    s.description.to_lowercase(),
                    s.tags.join(" ").to_lowercase()
                );
                terms.iter().any(|t| haystack.contains(t))
            })
            .collect();
        results.sort_by_key(|r| std::cmp::Reverse(r.success_count));
        results
    }

    /// Build a prompt snippet from SOPs matching the query.
    /// This is injected into the system prompt so the agent knows which SOP to follow.
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

    /// Get all registered SOPs.
    pub fn all(&self) -> &[Sop] {
        &self.sops
    }

    /// Number of registered SOPs.
    pub fn len(&self) -> usize {
        self.sops.len()
    }

    /// Whether no SOPs are registered.
    pub fn is_empty(&self) -> bool {
        self.sops.is_empty()
    }

    /// Get a SOP by name.
    pub fn get(&self, name: &str) -> Option<&Sop> {
        self.sops.iter().find(|s| s.name == name)
    }

    // ── Auto-crystallization ──

    /// Crystallize a tool call sequence into a SOP markdown file.
    /// This creates a new SOP describing the procedure used in a successful run.
    ///
    /// Unlike the Python GA (which has manually-crafted SOPs), this
    /// auto-generates a procedure document from observed tool calls.
    /// The generated SOP is saved to the store and can be read by the agent
    /// in future runs as a reference pattern.
    pub fn crystallise(
        &mut self,
        name: &str,
        description: &str,
        tool_sequence: &[(String, serde_json::Value)],
        session_id: Option<String>,
    ) -> Sop {
        // Build markdown content from tool sequence
        let mut content = String::new();
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

        let tags = extract_tags_from_tool_sequence(tool_sequence);

        let sop = Sop {
            name: name.to_string(),
            description: description.to_string(),
            tags,
            content,
            source_session: session_id,
            success_count: 1,
            created_at: chrono::Utc::now().to_rfc3339(),
            updated_at: chrono::Utc::now().to_rfc3339(),
        };

        self.register(sop.clone());
        let _ = self.save(&sop);
        sop
    }
}

// ── Helpers ──

/// Simplify args by keeping structure but replacing long string values with placeholders.
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

/// Extract tags from a tool sequence based on tool names.
fn extract_tags_from_tool_sequence(seq: &[(String, serde_json::Value)]) -> Vec<String> {
    let mut tags: Vec<String> = Vec::new();
    for (tool, _) in seq {
        let tag = match tool.as_str() {
            "read" | "write" | "edit" | "grep" => "file-operations",
            "web_search" | "web_fetch" | "web_scan" => "web",
            "code_run" | "bash" => "code-execution",
            "ask_user" => "interaction",
            _ => continue,
        };
        if !tags.iter().any(|t| t == tag) {
            tags.push(tag.to_string());
        }
    }
    tags.sort();
    tags
}

// ── Tests ──

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_sop_from_markdown() {
        let md = r#"# web_search — Search the web for information
Tags: web, search

## Usage
Use web_search when you need to find current information.

## Steps
1. Formulate a query
2. Call web_search with the query
3. Review results
"#;

        let path = PathBuf::from("web_search.md");
        let sop = Sop::from_markdown(&path, md);
        assert_eq!(sop.name, "web_search");
        assert_eq!(sop.description, "Search the web for information");
        assert_eq!(sop.tags, vec!["web", "search"]);
        assert!(sop.content.contains("## Usage"));
    }

    #[test]
    fn test_sop_to_markdown_roundtrip() {
        let md = "# test_sop — A test SOP\nTags: test, example\n\nBody content here.\n";
        let path = PathBuf::from("test_sop.md");
        let sop = Sop::from_markdown(&path, md);
        let rendered = sop.to_markdown();
        assert!(rendered.contains("# test_sop — A test SOP"));
        assert!(rendered.contains("Tags: test, example"));
        assert!(rendered.contains("Body content here."));
    }

    #[test]
    fn test_sop_store_load_and_save() {
        let dir = std::env::temp_dir().join("sop_store_test");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();

        // Write a test SOP file
        let md = "# test_sop — Test\nTags: test\n\nDo something.\n";
        std::fs::write(dir.join("test_sop.md"), md).unwrap();

        let store = SopStore::new(dir.clone());
        assert_eq!(store.len(), 1);
        assert_eq!(store.all()[0].name, "test_sop");

        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn test_find_matching() {
        let dir = std::env::temp_dir().join("sop_store_find");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();

        std::fs::write(
            dir.join("web_search.md"),
            "# web_search — Search\nTags: web\n\nContent.\n",
        )
        .unwrap();
        std::fs::write(
            dir.join("file_ops.md"),
            "# file_ops — File operations\nTags: file\n\nContent.\n",
        )
        .unwrap();

        let store = SopStore::new(dir.clone());
        assert_eq!(store.find_matching("web").len(), 1);
        assert_eq!(store.find_matching("file").len(), 1);
        assert_eq!(store.find_matching("search").len(), 1);
        assert_eq!(store.find_matching("database").len(), 0);

        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn test_build_prompt_snippet() {
        let dir = std::env::temp_dir().join("sop_store_snippet");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();

        std::fs::write(
            dir.join("verify.md"),
            "# verify — Verification SOP\nTags: testing\n\nAlways verify.\n",
        )
        .unwrap();

        let store = SopStore::new(dir.clone());
        let snippet = store.build_prompt_snippet("verify", 5);
        assert!(snippet.contains("verify"));
        assert!(snippet.contains("Always verify."));

        // No match
        let empty = store.build_prompt_snippet("database", 5);
        assert!(empty.is_empty());

        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn test_crystallise_creates_sop() {
        let dir = std::env::temp_dir().join("sop_store_crystallise");
        let _ = std::fs::remove_dir_all(&dir);

        let mut store = SopStore::new(dir.clone());
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

        let sop = store.crystallise(
            "check_hosts",
            "Read hosts file and check for localhost entry",
            &sequence,
            Some("sess_abc".into()),
        );

        assert_eq!(sop.name, "check_hosts");
        assert!(sop.content.contains("read"));
        assert!(sop.content.contains("grep"));
        assert!(sop.tags.contains(&"file-operations".to_string()));

        // Should be persisted
        assert!(dir.join("check_hosts.md").exists());

        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn test_register_merges_by_name() {
        let dir = std::env::temp_dir().join("sop_store_merge");
        let _ = std::fs::remove_dir_all(&dir);

        let mut store = SopStore::new(dir.clone());

        let sop1 = Sop {
            name: "task_a".into(),
            description: "First version".into(),
            tags: vec![],
            content: "Step 1\n".into(),
            source_session: None,
            success_count: 1,
            created_at: String::new(),
            updated_at: String::new(),
        };

        let sop2 = Sop {
            name: "task_a".into(),
            description: "Improved version".into(),
            tags: vec!["updated".into()],
            content: "Step 1 improved\n".into(),
            source_session: None,
            success_count: 1,
            created_at: String::new(),
            updated_at: String::new(),
        };

        store.register(sop1);
        assert_eq!(store.len(), 1);
        assert_eq!(store.all()[0].success_count, 1);

        store.register(sop2);
        assert_eq!(store.len(), 1); // merged, not duplicated
        assert_eq!(store.all()[0].success_count, 2); // incremented
        assert_eq!(store.all()[0].description, "Improved version");
        assert!(store.all()[0].tags.contains(&"updated".to_string()));

        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn test_empty_store_returns_empty() {
        let dir = std::env::temp_dir().join("sop_store_empty");
        let _ = std::fs::remove_dir_all(&dir);

        let store = SopStore::new(dir.clone());
        assert_eq!(store.len(), 0);
        assert!(store.find_matching("anything").is_empty());

        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn test_sop_from_markdown_fallback_name() {
        // No heading — fall back to filename stem
        let md = "Just some content without a heading.\n";
        let path = PathBuf::from("fallback_test.md");
        let sop = Sop::from_markdown(&path, md);
        assert_eq!(sop.name, "fallback_test");
    }

    #[test]
    fn test_simplify_args_long_strings() {
        let long = "a".repeat(200);
        let args = serde_json::json!({"code": long, "short": "ok", "items": [1, 2, 3, 4, 5]});
        let simplified = simplify_args(&args);
        assert_eq!(simplified["code"], "<code>");
        assert_eq!(simplified["short"], "ok");
        assert_eq!(simplified["items"].as_array().unwrap().len(), 3); // sampling
    }
}
