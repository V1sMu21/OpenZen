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
            Some(tool) => tool.execute(args, ctx).await,
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
}
