use std::collections::HashMap;

use async_trait::async_trait;
use linkme::distributed_slice;
use oz_core_types::{ToolContext, ToolDefinition, ToolFunction, ToolOutput};

#[distributed_slice]
pub static TOOL_FACTORIES: [fn(&mut ToolRegistry)] = [..];

/// Async tool handler trait — each tool is a struct implementing this.
#[async_trait]
pub trait ToolHandler: Send + Sync {
    fn name(&self) -> String;
    fn description(&self) -> String;
    /// Fallback description for non-English locales. Default: same as description().
    fn description_zh(&self) -> String {
        self.description()
    }
    fn parameters(&self) -> serde_json::Value;

    async fn execute(
        &self,
        args: serde_json::Value,
        ctx: &ToolContext,
    ) -> Result<ToolOutput, oz_core_types::ToolError>;
}

/// Tool registry — manages all available tools and generates schemas.
/// Supports both static `&'static str` names and dynamic `String` names.
pub struct ToolRegistry {
    handlers: HashMap<String, Box<dyn ToolHandler>>,
}

impl ToolRegistry {
    pub fn new() -> Self {
        ToolRegistry {
            handlers: HashMap::new(),
        }
    }

    pub fn register<T: ToolHandler + 'static>(&mut self, tool: T) {
        self.handlers
            .insert(tool.name().to_string(), Box::new(tool));
    }

    /// Register a tool with an explicit name (for dynamic/MCP tools).
    pub fn register_with_name<T: ToolHandler + 'static>(&mut self, name: &str, tool: T) {
        self.handlers.insert(name.to_string(), Box::new(tool));
    }

    pub fn get(&self, name: &str) -> Option<&dyn ToolHandler> {
        self.handlers.get(name).map(|b| b.as_ref())
    }

    pub async fn dispatch(
        &self,
        name: &str,
        args: serde_json::Value,
        ctx: &ToolContext,
    ) -> Result<ToolOutput, oz_core_types::ToolError> {
        match self.handlers.get(name) {
            Some(tool) => {
                // Last line of defence for weakly schema-adhering models: a
                // call whose payload is correct but whose *keys* are aliases
                // (`cmd`/`command` instead of `code`, `path`/`file` instead of
                // `file_path`, the tool name itself as the key) used to be
                // rejected with "missing `code` argument". Re-key before the
                // tool reads its arguments. See `oz_core_types::tool_args`.
                let args = canonicalize_dispatch_args(name, args, tool.as_ref());
                tool.execute(args, ctx).await
            }
            None => Ok(ToolOutput::unknown_tool(name)),
        }
    }

    pub fn names(&self) -> Vec<String> {
        self.handlers.keys().cloned().collect()
    }

    pub fn to_schema(&self, lang: &str) -> Vec<ToolDefinition> {
        let is_zh = lang == "zh";
        // Sort by name for a byte-stable tools array. HashMap iteration order
        // is randomized per process (RandomState), which silently shuffled the
        // tools JSON between runs and broke omlx's token-prefix cache at the
        // tools block of every new session.
        let mut defs: Vec<ToolDefinition> = self
            .handlers
            .iter()
            .map(|(name, t)| ToolDefinition {
                type_: "function".into(),
                function: ToolFunction {
                    name: name.clone(),
                    description: if is_zh {
                        t.description_zh()
                    } else {
                        t.description()
                    },
                    parameters: t.parameters(),
                },
            })
            .collect();
        defs.sort_by(|a, b| a.function.name.cmp(&b.function.name));
        defs
    }

    pub fn build_default() -> Self {
        // Merge auto-registered (linkme distributed slice) AND manual
        // registrations. The linkme slice is unreliable under LTO
        // — some tool factory functions get dead-stripped because nothing
        // outside the linkme section references them by name. Always
        // also call build_manual so the builtin toolset is present
        // regardless. build_manual.register uses the same name keys, so
        // duplicates collapse to one handler (manual wins if linkme
        // managed to register a stale entry first).
        let mut reg = Self::build_auto();
        let manual = Self::build_manual();
        for (name, handler) in manual.handlers {
            reg.handlers.insert(name, handler);
        }
        reg
    }

    pub fn build_auto() -> Self {
        let mut reg = ToolRegistry::new();
        for factory in TOOL_FACTORIES.iter() {
            factory(&mut reg);
        }
        reg
    }

    fn build_manual() -> Self {
        let mut reg = ToolRegistry::new();

        reg.register(crate::code_run::CodeRunTool);
        reg.register(crate::file_ops::FileReadTool);
        reg.register(crate::file_ops::FileWriteTool);
        reg.register(crate::file_ops::FilePatchTool);
        reg.register(crate::file_ops::FileEditTool);
        reg.register(crate::file_ops::GlobTool);
        reg.register(crate::file_ops::GrepTool);
        reg.register(crate::file_ops::LsTool);
        reg.register(crate::no_tool::NoTool);
        reg.register(crate::working_mem::WorkingMemTool);
        reg.register(crate::harness_refine::HarnessRefineTool);
        reg.register(crate::ask_user::AskUserTool);
        reg.register(crate::web_fetch::WebFetchTool);
        reg.register(crate::web_scan::WebScanTool::new());
        reg.register(crate::web_js::WebJsTool::new());
        reg.register(crate::web_execute_js::WebExecuteJsTool::new());
        reg.register(crate::web_execute_js::WebListTabsTool::new());
        reg.register(crate::long_term::LongTermTool);
        reg.register(crate::web_search::WebSearchTool);
        reg.register(crate::skill_mcp_search::SkillMcpSearchTool);
        reg.register(crate::skill_mcp_search::SkillMcpListTool);
        reg.register(crate::skill_mcp_write::SkillMcpStoreTool);
        reg.register(crate::skill_mcp_write::SkillMcpRefineTool);
        reg.register(crate::todowrite::TodoWriteTool);
        reg.register(crate::todoupdate::TodoUpdateTool);
        reg.register(crate::schedule_reminder::ScheduleReminderTool);
        reg.register(crate::open_side_panel::OpenSidePanelTool);
        crate::computer_use::register_all(&mut reg);

        reg
    }
}

impl Default for ToolRegistry {
    fn default() -> Self {
        Self::new()
    }
}

/// Re-key a tool call's arguments onto the tool's canonical parameter names.
///
/// Curated builtin tools use the alias table in `oz_core_types::tool_args`;
/// every other tool (MCP / dynamic) falls back to a schema-driven repair that
/// maps the tool-name key or a lone unrecognized string key onto the schema's
/// first required property. Unchanged arguments are returned untouched, so the
/// happy path pays only a few string comparisons.
fn canonicalize_dispatch_args(
    name: &str,
    args: serde_json::Value,
    tool: &dyn ToolHandler,
) -> serde_json::Value {
    if !args.is_object() {
        return args;
    }
    let repaired = oz_core_types::canonicalize_tool_args(name, args.clone());
    let repaired = if oz_core_types::has_alias_table(name) {
        repaired
    } else {
        oz_core_types::repair_args_against_schema(name, repaired, &tool.parameters())
    };
    if repaired != args {
        tracing::warn!(
            "[tools] re-keyed non-canonical arguments for {name}: keys {:?} -> keys {:?}",
            oz_core_types::arg_keys(&args),
            oz_core_types::arg_keys(&repaired),
        );
    }
    repaired
}

#[cfg(test)]
mod tests {
    use super::*;
    use oz_core_types::ToolError;

    struct DummyTool;

    #[async_trait]
    impl ToolHandler for DummyTool {
        fn name(&self) -> String {
            "dummy".to_string()
        }
        fn description(&self) -> String {
            "A test tool".to_string()
        }
        fn parameters(&self) -> serde_json::Value {
            serde_json::json!({"type": "object", "properties": {}})
        }
        async fn execute(
            &self,
            _args: serde_json::Value,
            _ctx: &ToolContext,
        ) -> Result<ToolOutput, ToolError> {
            Ok(ToolOutput::success(serde_json::json!({"done": true})))
        }
    }

    fn ctx() -> ToolContext {
        ToolContext {
            working_dir: "/tmp".into(),
            assets_dir: "/tmp".into(),
            script_dir: "/tmp".into(),
            lang: "en".into(),
            skill_mcp_dir: None,
            harness_dir: None,
            session_id: String::new(),
            tool_budget_secs: 0,
        }
    }

    #[tokio::test]
    async fn test_register_and_get() {
        let mut reg = ToolRegistry::new();
        reg.register(DummyTool);
        assert!(reg.get("dummy").is_some());
        assert!(reg.get("nonexistent").is_none());
    }

    #[tokio::test]
    async fn test_dispatch_known_tool() {
        let mut reg = ToolRegistry::new();
        reg.register(DummyTool);
        let result = reg.dispatch("dummy", serde_json::json!({}), &ctx()).await;
        assert!(result.is_ok());
        assert_eq!(result.unwrap().data["done"], true);
    }

    #[tokio::test]
    async fn test_dispatch_unknown_tool() {
        let reg = ToolRegistry::new();
        let result = reg.dispatch("unknown", serde_json::json!({}), &ctx()).await;
        assert!(result.is_ok());
        assert!(result.unwrap().next_prompt.unwrap().contains("未知工具"));
    }

    #[tokio::test]
    async fn test_to_schema() {
        let mut reg = ToolRegistry::new();
        reg.register(DummyTool);
        let schema = reg.to_schema("en");
        assert_eq!(schema.len(), 1);
        assert_eq!(schema[0].function.name, "dummy");
    }

    #[tokio::test]
    async fn test_names() {
        let mut reg = ToolRegistry::new();
        reg.register(DummyTool);
        assert_eq!(reg.names(), vec!["dummy".to_string()]);
    }

    #[tokio::test]
    async fn test_build_default_has_all_tools() {
        let reg = ToolRegistry::build_default();
        let names = reg.names();
        for &expected in &[
            "code_run",
            "read",
            "write",
            "patch",
            "glob",
            "grep",
            "ls",
            "respond",
            "working_mem",
            "ask_user",
            "web_scan",
            "web_js",
            "long_term",
        ] {
            assert!(
                names.iter().any(|n| n == expected),
                "missing tool: {expected}"
            );
        }
    }

    #[tokio::test]
    async fn test_empty_registry_names_is_empty() {
        let reg = ToolRegistry::new();
        assert!(reg.names().is_empty());
    }

    #[tokio::test]
    async fn test_register_overwrites_existing_tool() {
        struct OverrideTool;
        #[async_trait]
        impl ToolHandler for OverrideTool {
            fn name(&self) -> String {
                "dummy".to_string()
            }
            fn description(&self) -> String {
                "overridden".to_string()
            }
            fn parameters(&self) -> serde_json::Value {
                serde_json::json!({})
            }
            async fn execute(
                &self,
                _a: serde_json::Value,
                _c: &ToolContext,
            ) -> Result<ToolOutput, ToolError> {
                Ok(ToolOutput::success(serde_json::json!({"overridden": true})))
            }
        }

        let mut reg = ToolRegistry::new();
        reg.register(DummyTool);
        reg.register(OverrideTool);

        let result = reg
            .dispatch("dummy", serde_json::json!({}), &ctx())
            .await
            .unwrap();
        assert_eq!(result.data["overridden"], true);
        assert_eq!(reg.names().len(), 1);
    }

    #[tokio::test]
    async fn test_dispatch_propagates_unknown_tool_name_in_message() {
        let reg = ToolRegistry::new();
        let result = reg
            .dispatch("bogus_name_42", serde_json::json!({}), &ctx())
            .await
            .unwrap();
        let msg = result.next_prompt.unwrap_or_default();
        assert!(
            msg.contains("bogus_name_42"),
            "unknown tool name should appear in error: {msg}"
        );
    }

    #[tokio::test]
    async fn test_to_schema_contains_all_registered_tools() {
        struct ToolA;
        struct ToolB;
        #[async_trait]
        impl ToolHandler for ToolA {
            fn name(&self) -> String {
                "a".to_string()
            }
            fn description(&self) -> String {
                "".to_string()
            }
            fn parameters(&self) -> serde_json::Value {
                serde_json::json!({})
            }
            async fn execute(
                &self,
                _a: serde_json::Value,
                _c: &ToolContext,
            ) -> Result<ToolOutput, ToolError> {
                Ok(ToolOutput::success(serde_json::json!({})))
            }
        }
        #[async_trait]
        impl ToolHandler for ToolB {
            fn name(&self) -> String {
                "b".to_string()
            }
            fn description(&self) -> String {
                "".to_string()
            }
            fn parameters(&self) -> serde_json::Value {
                serde_json::json!({})
            }
            async fn execute(
                &self,
                _a: serde_json::Value,
                _c: &ToolContext,
            ) -> Result<ToolOutput, ToolError> {
                Ok(ToolOutput::success(serde_json::json!({})))
            }
        }

        let mut reg = ToolRegistry::new();
        reg.register(ToolB);
        reg.register(ToolA);
        let schema = reg.to_schema("en");
        assert_eq!(schema.len(), 2);
        let names: std::collections::BTreeSet<&str> =
            schema.iter().map(|d| d.function.name.as_str()).collect();
        assert!(names.contains("a"));
        assert!(names.contains("b"));
    }

    #[tokio::test]
    async fn test_dispatch_passes_context_fields() {
        struct CtxCheckTool;
        #[async_trait]
        impl ToolHandler for CtxCheckTool {
            fn name(&self) -> String {
                "ctx_check".to_string()
            }
            fn description(&self) -> String {
                "".to_string()
            }
            fn parameters(&self) -> serde_json::Value {
                serde_json::json!({})
            }
            async fn execute(
                &self,
                _a: serde_json::Value,
                ctx: &ToolContext,
            ) -> Result<ToolOutput, ToolError> {
                Ok(ToolOutput::success(serde_json::json!({
                    "wd": ctx.working_dir,
                    "lang": ctx.lang,
                })))
            }
        }

        let mut reg = ToolRegistry::new();
        reg.register(CtxCheckTool);
        let tc = ToolContext {
            working_dir: "/my/proj".into(),
            assets_dir: "/assets".into(),
            script_dir: "/scripts".into(),
            lang: "en".into(),
            skill_mcp_dir: None,
            harness_dir: None,
            session_id: String::new(),
            tool_budget_secs: 0,
        };
        let result = reg
            .dispatch("ctx_check", serde_json::json!({}), &tc)
            .await
            .unwrap();
        assert_eq!(result.data["wd"], "/my/proj");
        assert_eq!(result.data["lang"], "en");
    }

    #[tokio::test]
    async fn test_default_registry_has_correct_names() {
        let reg = ToolRegistry::build_default();
        let names = reg.names();
        assert!(names.iter().any(|n| n == "code_run"));
        assert!(names.iter().any(|n| n == "read"));
        assert!(names.iter().any(|n| n == "write"));
        assert!(names.iter().any(|n| n == "patch"));
        assert!(names.iter().any(|n| n == "glob"));
        assert!(names.iter().any(|n| n == "grep"));
        assert!(names.iter().any(|n| n == "ls"));
        assert!(names.iter().any(|n| n == "respond"));
        assert!(names.iter().any(|n| n == "web_search"));
        assert!(
            names.len() >= 14,
            "expected at least 14 tools, got {}",
            names.len()
        );
    }

    #[tokio::test]
    async fn test_build_default_schema_is_valid() {
        let reg = ToolRegistry::build_default();
        let schema = reg.to_schema("en");
        assert!(
            schema.len() >= 14,
            "expected at least 14 tools, got {}",
            schema.len()
        );
        for def in &schema {
            assert_eq!(def.type_, "function");
            assert!(!def.function.name.is_empty());
            assert!(!def.function.description.is_empty());
            assert!(def.function.parameters.is_object());
        }
    }

    // ── non-canonical argument keys (weak schema adherence) ──

    /// Records the arguments the tool actually received.
    struct CaptureTool(std::sync::Arc<std::sync::Mutex<serde_json::Value>>);

    #[async_trait]
    impl ToolHandler for CaptureTool {
        fn name(&self) -> String {
            "capture".to_string()
        }
        fn description(&self) -> String {
            "capture".to_string()
        }
        fn parameters(&self) -> serde_json::Value {
            serde_json::json!({
                "type": "object",
                "properties": {"query": {"type": "string"}},
                "required": ["query"]
            })
        }
        async fn execute(
            &self,
            args: serde_json::Value,
            _ctx: &ToolContext,
        ) -> Result<ToolOutput, ToolError> {
            *self.0.lock().unwrap() = args;
            Ok(ToolOutput::success(serde_json::json!({"ok": true})))
        }
    }

    #[tokio::test]
    async fn dispatch_rekeys_alias_keys_for_builtin_tools() {
        let mut reg = ToolRegistry::new();
        reg.register(crate::code_run::CodeRunTool);
        let out = reg
            .dispatch(
                "code_run",
                serde_json::json!({"cmd": "echo canonicalized-ok", "mode": "bash", "timeout": 20}),
                &ctx(),
            )
            .await
            .expect("dispatch");
        assert_eq!(out.data["exit_code"], 0, "output: {:?}", out.data);
        assert!(
            out.data["stdout"]
                .as_str()
                .unwrap_or_default()
                .contains("canonicalized-ok"),
            "stdout: {:?}",
            out.data["stdout"]
        );
    }

    #[tokio::test]
    async fn dispatch_rekeys_tool_name_as_argument_key() {
        let seen = std::sync::Arc::new(std::sync::Mutex::new(serde_json::Value::Null));
        let mut reg = ToolRegistry::new();
        reg.register_with_name("code_run", CaptureTool(seen.clone()));
        reg.dispatch(
            "code_run",
            serde_json::json!({"code_run": "echo hi"}),
            &ctx(),
        )
        .await
        .expect("dispatch");
        assert_eq!(seen.lock().unwrap()["code"], "echo hi");
    }

    #[tokio::test]
    async fn dispatch_repairs_mcp_style_args_from_schema() {
        let seen = std::sync::Arc::new(std::sync::Mutex::new(serde_json::Value::Null));
        let mut reg = ToolRegistry::new();
        reg.register_with_name("mcp__docs__search", CaptureTool(seen.clone()));
        reg.dispatch("mcp__docs__search", serde_json::json!({"q": "hi"}), &ctx())
            .await
            .expect("dispatch");
        assert_eq!(seen.lock().unwrap()["query"], "hi");
    }

    // ── fixture-driven regression: real "missing `code` argument" sessions ──

    /// Deep-collect every string in `v` containing a `<tool_use:…>` transcript tag.

    /// Extract `(tool_name, input)` pairs from a session transcript.
    fn tool_uses_from_transcript(text: &str) -> Vec<(String, serde_json::Value)> {
        let mut found = Vec::new();
        let unescaped = text.replace("\\n", "\n");
        for line in unescaped.split('\n') {
            let Some(idx) = line.find("<tool_use:") else {
                continue;
            };
            let rest = &line[idx + "<tool_use:".len()..];
            let Some(close) = rest.find('>') else {
                continue;
            };
            let name = rest[..close].trim().to_string();
            let raw = rest[close + 1..].trim();
            if raw.is_empty() {
                continue;
            }
            // Depending on how many times the transcript was JSON-encoded, the
            // payload arrives either as plain JSON or with escaped quotes.
            let parsed = serde_json::from_str::<serde_json::Value>(raw)
                .ok()
                .or_else(|| {
                    let decoded: String = serde_json::from_str(&format!("\"{raw}\"")).ok()?;
                    serde_json::from_str::<serde_json::Value>(&decoded).ok()
                });
            if let Some(input) = parsed {
                found.push((name, input));
            }
        }
        found
    }

    /// Inline fixture of the REAL failing call shapes.
    ///
    /// These bytes come from the long-task transcripts under
    /// `tests/longtask/*/openzen/checkpoints/*.json` — 374 real `<tool_use:…>`
    /// calls, of which the non-canonical ones are exactly these shapes
    /// (extracted 2026-10-21; the transcripts themselves are gitignored, so a
    /// fixture that read them from disk could never run in CI — the guard was
    /// green-by-absence there). Values are shortened; **key sets and types are
    /// verbatim**, which is what canonicalization keys off.
    ///
    /// Production frequencies behind the same shapes (AGENTS.md, real
    /// `sessions.json`): `missing \`code\` argument` ×422,
    /// `missing file_path` ×72.
    const REAL_FAILING_CALLS: &[(&str, &str)] = &[
        // The dominant code_run shape: `cmd` instead of `code`, `mode`
        // instead of `type` — this is the ×422 failure.
        (
            "code_run",
            r#"{"cmd": "cd ~/Documents/apps/openzen/tests/longtask/2/final && npm run build", "mode": "bash", "timeout": "60"}"#,
        ),
        // Same shape, python type and a bare command.
        (
            "code_run",
            r#"{"cmd": "print(1)", "mode": "python", "timeout": "30"}"#,
        ),
        // `command` / `script` aliases.
        ("code_run", r#"{"command": "echo hi", "type": "bash"}"#),
        ("code_run", r#"{"script": "ls -la", "type": "bash"}"#),
        // The tool name itself used as the key (weak-schema models).
        ("code_run", r#"{"code_run": "git status", "type": "bash"}"#),
        // Real read shape: `count`/`start` instead of `limit`/`offset`.
        (
            "read",
            r#"{"count": "1244", "file_path": "/tmp/longtask/index.html", "start": "200"}"#,
        ),
        // `path` instead of `file_path` (the ×72 failure).
        ("read", r#"{"path": "/tmp/notes.md"}"#),
        ("write", r#"{"file": "/tmp/out.txt", "content": "hello"}"#),
        ("write", r#"{"filename": "/tmp/out.txt", "text": "hello"}"#),
        (
            "write",
            r#"{"filePath": "/tmp/out.txt", "content": "hello"}"#,
        ),
        (
            "edit",
            r#"{"file_path": "/tmp/a.rs", "old_text": "a", "new_text": "b"}"#,
        ),
        (
            "patch",
            r#"{"file": "/tmp/a.rs", "old_string": "a", "new_string": "b"}"#,
        ),
        ("glob", r#"{"glob": "**/*.rs", "path": "/tmp"}"#),
        ("grep", r#"{"query": "fn main", "path": "/tmp"}"#),
        ("ls", r#"{"dir": "/tmp"}"#),
        // Canonical call: must pass through untouched.
        ("code_run", r#"{"code": "echo ok", "type": "bash"}"#),
        ("read", r#"{"file_path": "/tmp/a.md", "limit": 20}"#),
    ];

    /// Every real failing call must end up with a required argument it did not
    /// have before — canonicalization moves values, it never leaves a broken
    /// call broken in the same way.
    #[test]
    fn real_failing_call_shapes_gain_canonical_keys() {
        let reg = ToolRegistry::build_default();
        let strings: Vec<String> = REAL_FAILING_CALLS
            .iter()
            .map(|(name, args)| format!("<tool_use:{name}>{args}"))
            .collect();

        let mut calls = 0usize;
        let mut foreign_keyed = 0usize;
        let mut unrepaired = Vec::new();
        for s in &strings {
            for (name, input) in tool_uses_from_transcript(s) {
                let Some(tool) = reg.get(&name) else { continue };
                if !input.is_object() {
                    continue;
                }
                calls += 1;
                let schema = serde_json::json!(tool.parameters());
                let props: Vec<&str> = schema
                    .get("properties")
                    .and_then(|p| p.as_object())
                    .map(|p| p.keys().map(|k| k.as_str()).collect())
                    .unwrap_or_default();
                // A key the tool's own schema does not define is the signature
                // of the failure this fix targets.
                let uses_foreign_key = input
                    .as_object()
                    .map(|m| m.keys().any(|k| !props.contains(&k.as_str())))
                    .unwrap_or(false);
                let primary = schema
                    .get("required")
                    .and_then(|r| r.as_array())
                    .and_then(|r| r.first())
                    .and_then(|v| v.as_str())
                    .unwrap_or("");
                if !uses_foreign_key || primary.is_empty() {
                    continue;
                }
                foreign_keyed += 1;
                let fixed = canonicalize_dispatch_args(&name, input.clone(), tool);
                if fixed.get(primary).is_none() {
                    unrepaired.push((name.clone(), oz_core_types::arg_keys(&input)));
                }
            }
        }
        assert_eq!(
            calls,
            REAL_FAILING_CALLS.len(),
            "every fixture call must resolve to a registered tool"
        );
        assert!(
            foreign_keyed >= 10,
            "the fixture must actually contain non-canonical calls, got {foreign_keyed}"
        );
        assert!(
            unrepaired.is_empty(),
            "re-keying left calls without their primary argument: {}",
            serde_json::to_string(&unrepaired).unwrap_or_default()
        );
    }

    #[tokio::test]
    async fn dispatch_leaves_canonical_args_alone() {
        let seen = std::sync::Arc::new(std::sync::Mutex::new(serde_json::Value::Null));
        let mut reg = ToolRegistry::new();
        reg.register_with_name("mcp__docs__search", CaptureTool(seen.clone()));
        let args = serde_json::json!({"query": "keep", "limit": 3});
        reg.dispatch("mcp__docs__search", args.clone(), &ctx())
            .await
            .expect("dispatch");
        assert_eq!(*seen.lock().unwrap(), args);
    }
}
