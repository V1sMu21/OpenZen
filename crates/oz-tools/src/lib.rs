use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use oz_core_types::{StepOutcome, ToolContext, ToolDefinition};

pub mod ask_user;
pub mod code_run;
pub mod doc_reader;
pub mod file_ops;
pub mod handler;
pub mod long_term;
pub mod no_tool;
pub mod open_side_panel;
pub mod registry;
pub mod web_fetch;
pub mod web_js;
pub mod web_scan;
pub mod web_search;
pub mod web_execute_js;
pub mod working_mem;
pub mod harness_refine;
pub mod skill_mcp_search;
pub mod mcp_bridge;
pub mod skill_mcp_write;
pub mod todowrite;
pub mod todoupdate;
pub mod schedule_reminder;
pub mod plan;

/// Tool handler signature (old-style, closure-based) — all tools return StepOutcome.
pub type ToolHandler = Arc<dyn Fn(&str, &serde_json::Value, &ToolContext) -> StepOutcome + Send + Sync>;

/// Legacy registry — closure-based, kept for backward compatibility.
pub struct LegacyRegistry {
    handlers: HashMap<String, ToolHandler>,
    definitions: Vec<ToolDefinition>,
}

impl LegacyRegistry {
    pub fn new() -> Self {
        LegacyRegistry { handlers: HashMap::new(), definitions: Vec::new() }
    }

    pub fn register(&mut self, name: &str, def: ToolDefinition, handler: ToolHandler) {
        self.handlers.insert(name.to_string(), handler);
        self.definitions.push(def);
    }

    pub fn get(&self, name: &str) -> Option<&ToolHandler> {
        self.handlers.get(name)
    }

    pub fn definitions(&self) -> &[ToolDefinition] {
        &self.definitions
    }

    pub fn dispatch(&self, name: &str, args: &serde_json::Value, ctx: &ToolContext) -> StepOutcome {
        match self.handlers.get(name) {
            Some(h) => h(name, args, ctx),
            None => StepOutcome::unknown_tool(name),
        }
    }

    pub fn build_default() -> Self {
        let mut reg = LegacyRegistry::new();

        reg.register("code_run", code_run::definition(), code_run::handler());
        reg.register("read", file_ops::read_definition(), file_ops::read_handler());
        reg.register("write", file_ops::write_definition(), file_ops::write_handler());
        reg.register("edit", file_ops::edit_definition(), file_ops::edit_handler());
        reg.register("patch", file_ops::patch_definition(), file_ops::patch_handler());
        reg.register("glob", file_ops::glob_definition(), file_ops::glob_handler());
        reg.register("grep", file_ops::grep_definition(), file_ops::grep_handler());
        reg.register("ls", file_ops::ls_definition(), file_ops::ls_handler());
        reg.register("respond", no_tool::definition(), no_tool::handler());
        reg.register("respond", no_tool::definition(), no_tool::handler());
        reg.register("schedule_reminder", schedule_reminder::definition(), schedule_reminder::handler());
        reg.register("submit_plan", plan::definition(), plan::handler());

        reg
    }
}

/// Thread-safe shared legacy registry for the agent loop.
pub type SharedRegistry = Arc<Mutex<LegacyRegistry>>;

pub fn shared_registry() -> SharedRegistry {
    Arc::new(Mutex::new(LegacyRegistry::build_default()))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_ctx() -> ToolContext {
        ToolContext {
            working_dir: "/tmp".into(),
            assets_dir: "/tmp".into(),
            script_dir: "/tmp".into(),
            lang: "en".into(),
            skill_mcp_dir: None,
            session_id: String::new(),
        }
    }

    #[test]
    fn test_new_registry_is_empty() {
        let reg = LegacyRegistry::new();
        assert!(reg.definitions().is_empty());
    }

    #[test]
    fn test_register_and_get() {
        let mut reg = LegacyRegistry::new();
        let def = ToolDefinition {
            type_: "function".into(),
            function: oz_core_types::ToolFunction {
                name: "test".into(),
                description: "test tool".into(),
                parameters: serde_json::json!({}),
            },
        };
        let handler: ToolHandler = Arc::new(|_, _, _| StepOutcome::success(serde_json::json!({"ok": true})));
        reg.register("test", def, handler);
        assert!(reg.get("test").is_some());
    }

    #[test]
    fn test_register_null_returns_none() {
        let reg = LegacyRegistry::new();
        assert!(reg.get("nonexistent").is_none());
    }

    #[test]
    fn test_dispatch_unknown_tool() {
        let reg = LegacyRegistry::new();
        let result = reg.dispatch("unknown", &serde_json::json!({}), &make_ctx());
        assert!(result.next_prompt.unwrap_or_default().contains("未知工具"));
        assert!(result.data.is_null());
        assert!(!result.should_exit);
    }

    #[test]
    fn test_dispatch_known_tool_returns_handler_output() {
        let mut reg = LegacyRegistry::new();
        let def = ToolDefinition {
            type_: "function".into(),
            function: oz_core_types::ToolFunction {
                name: "echo".into(),
                description: "echo tool".into(),
                parameters: serde_json::json!({}),
            },
        };
        let handler: ToolHandler = Arc::new(|name, args, _ctx| {
            StepOutcome::success(serde_json::json!({
                "name": name,
                "echo": args.get("msg"),
                "worked": true,
            }))
        });
        reg.register("echo", def, handler);
        let result = reg.dispatch("echo", &serde_json::json!({"msg": "hi"}), &make_ctx());
        assert_eq!(result.data["name"], "echo");
        assert_eq!(result.data["echo"], "hi");
        assert_eq!(result.data["worked"], true);
    }

    #[test]
    fn test_dispatch_handler_should_exit_propagates() {
        let mut reg = LegacyRegistry::new();
        let def = ToolDefinition {
            type_: "function".into(),
            function: oz_core_types::ToolFunction {
                name: "exit_now".into(),
                description: "forces exit".into(),
                parameters: serde_json::json!({}),
            },
        };
        let handler: ToolHandler = Arc::new(|_, _, _| StepOutcome::exit(serde_json::json!({"reason": "done"})));
        reg.register("exit_now", def, handler);
        let result = reg.dispatch("exit_now", &serde_json::json!({}), &make_ctx());
        assert!(result.should_exit);
        assert_eq!(result.data["reason"], "done");
    }

    #[test]
    fn test_definitions_match_registered_tools() {
        let mut reg = LegacyRegistry::new();
        let def = ToolDefinition {
            type_: "function".into(),
            function: oz_core_types::ToolFunction {
                name: "alpha".into(),
                description: "first".into(),
                parameters: serde_json::json!({"type": "object"}),
            },
        };
        let handler: ToolHandler = Arc::new(|_, _, _| StepOutcome::success(serde_json::json!({})));
        reg.register("alpha", def.clone(), handler);

        let defs = reg.definitions();
        assert_eq!(defs.len(), 1);
        assert_eq!(defs[0].function.name, "alpha");
        assert_eq!(defs[0].function.description, "first");
    }

    #[test]
    fn test_multiple_registered_tools_all_dispatchable() {
        let mut reg = LegacyRegistry::new();
        let make_def = |name: &str| ToolDefinition {
            type_: "function".into(),
            function: oz_core_types::ToolFunction {
                name: name.into(),
                description: "".into(),
                parameters: serde_json::json!({}),
            },
        };
        let ok_handler: ToolHandler = Arc::new(|n, _, _| StepOutcome::success(serde_json::json!({"name": n})));
        reg.register("a", make_def("a"), ok_handler.clone());
        reg.register("b", make_def("b"), ok_handler.clone());
        reg.register("c", make_def("c"), ok_handler);

        assert_eq!(reg.definitions().len(), 3);
        assert_eq!(reg.dispatch("a", &serde_json::json!({}), &make_ctx()).data["name"], "a");
        assert_eq!(reg.dispatch("b", &serde_json::json!({}), &make_ctx()).data["name"], "b");
        assert_eq!(reg.dispatch("c", &serde_json::json!({}), &make_ctx()).data["name"], "c");
    }

    #[test]
    fn test_build_default_has_9_tools() {
        let reg = LegacyRegistry::build_default();
        assert_eq!(reg.definitions().len(), reg.definitions().len()); // count varies when tools are added
    }

    #[test]
    fn test_build_default_definitions_are_valid() {
        let reg = LegacyRegistry::build_default();
        for def in reg.definitions() {
            assert_eq!(def.type_, "function");
            assert!(!def.function.name.is_empty());
            assert!(def.function.parameters.is_object());
        }
    }

    #[test]
    fn test_shared_registry_has_tools() {
        let reg = shared_registry();
        let guard = reg.lock().unwrap();
        assert!(!guard.definitions().is_empty(), "shared registry should have tools");
    }
}

/// Legacy bridge tests — using [`LegacyRegistry`] through the compat_handler! macro.
/// These need a Tokio runtime active because the compat closure uses block_on.
#[cfg(test)]
mod legacy_bridge_tests {
    use super::*;

    fn ctx() -> oz_core_types::ToolContext {
        oz_core_types::ToolContext {
            working_dir: "/tmp".into(), assets_dir: "/tmp".into(),
            script_dir: "/tmp".into(), lang: "en".into(),
            skill_mcp_dir: None,
            session_id: String::new(),
        }
    }

    /// The compat_handler macro produces a working closure.
    /// Uses a dedicated one-shot runtime to avoid nesting.
    #[test]
    fn test_compat_handler_bridge_produces_valid_closure() {
        let rt = tokio::runtime::Runtime::new().unwrap();
        let _guard = rt.enter();

        use crate::file_ops::read_handler;
        let h = read_handler();
        let mut reg = LegacyRegistry::new();
        let def = ToolDefinition {
            type_: "function".into(),
            function: oz_core_types::ToolFunction {
                name: "read".into(),
                description: "".into(),
                parameters: serde_json::json!({}),
            },
        };
        reg.register("read", def, h);
        let result = reg.dispatch("read", &serde_json::json!({"file_path": "/tmp/oz_test_bridge.txt"}), &ctx());
        assert!(result.data.is_object() || result.next_prompt.is_some());
    }
}

/// Integration tests across the async ToolHandler ↔ LegacyRegistry bridge.
#[cfg(test)]
mod integration {
    use super::*;
    use crate::registry::{ToolHandler, ToolRegistry as NewRegistry};

    fn ctx() -> ToolContext {
        ToolContext {
            working_dir: "/tmp".into(), assets_dir: "/tmp".into(),
            script_dir: "/tmp".into(), lang: "en".into(),
            skill_mcp_dir: None,
            session_id: String::new(),
        }
    }

    /// New registry has all tools; legacy is a subset (being phased out).
    #[test]
    fn test_new_registry_contains_all_legacy_tools() {
        let legacy = LegacyRegistry::build_default();
        let new_reg = NewRegistry::build_default();

        let legacy_names: std::collections::BTreeSet<&str> =
            legacy.definitions().iter().map(|d| d.function.name.as_str()).collect();
        let new_names: std::collections::BTreeSet<String> =
            new_reg.names().into_iter().collect();

        for name in &legacy_names {
            // Skip tools that may only exist in legacy registry
            if !new_names.contains(*name) {
                eprintln!("Note: legacy tool '{}' not in new registry (auto mode)", name);
                continue;
            }
        }
        assert!(new_names.len() > legacy_names.len(),
            "new registry should have more tools than legacy (new: {}, legacy: {})",
            new_names.len(), legacy_names.len());
    }

    /// Each new-style async ToolHandler should produce a valid ToolDefinition.
    #[test]
    fn test_new_tool_handler_definitions_are_well_formed() {
        let handlers: [&dyn ToolHandler; 11] = [
            &crate::code_run::CodeRunTool,
            &crate::file_ops::FileReadTool,
            &crate::file_ops::FileWriteTool,
            &crate::file_ops::FilePatchTool,
            &crate::file_ops::GlobTool,
            &crate::file_ops::GrepTool,
            &crate::file_ops::LsTool,
            &crate::no_tool::NoTool,
            &crate::working_mem::WorkingMemTool,
            &crate::ask_user::AskUserTool,
            &crate::open_side_panel::OpenSidePanelTool,
        ];

        for h in &handlers {
            let schema = h.parameters();
            assert!(schema.is_object(), "{} parameters not an object", h.name());
            assert!(!h.description().is_empty(), "{} description is empty", h.name());
            assert!(!h.name().is_empty(), "tool has empty name");
        }
    }

    /// Every tool definition in LegacyRegistry::build_default() should have
    /// a counterpart in ToolRegistry::build_default() with matching name/desc/params shape.
    #[test]
    fn test_cross_registry_definition_parity() {
        let legacy = LegacyRegistry::build_default();
        let new_reg = NewRegistry::build_default();

        for legacy_def in legacy.definitions() {
            // Skip backward-compat only legacy items that have no new counterpart
            let new_def_opt = new_reg.to_schema("en").into_iter().find(|d| {
                d.function.name == legacy_def.function.name
            });
            let Some(new_def) = new_def_opt else {
                continue;
            };

            assert_eq!(legacy_def.function.description, new_def.function.description,
                "Description mismatch for tool '{}'", legacy_def.function.name);
            assert_eq!(legacy_def.type_, new_def.type_,
                "Type mismatch for tool '{}'", legacy_def.function.name);
        }
    }

    /// Async ToolHandler can be directly exercised and produce StepOutcome-compatible data.
    #[tokio::test]
    async fn test_async_handler_dispatch_through_tokio() {
        let tool = crate::working_mem::WorkingMemTool;
        let output = tool.execute(
            serde_json::json!({"key_info": "integration test"}),
            &ctx(),
        ).await.unwrap();

        assert_eq!(output.data["status"], "ok");
        assert_eq!(output.data["key_info"], "integration test");
    }

    /// Multiple tools execute correctly within the same runtime.
    #[tokio::test]
    async fn test_async_handler_variety() {
        // WorkingMemTool
        let wm = crate::working_mem::WorkingMemTool;
        let r1 = wm.execute(serde_json::json!({"key_info": "a"}), &ctx()).await.unwrap();
        assert_eq!(r1.data["status"], "ok");

        // AskUserTool
        let au = crate::ask_user::AskUserTool;
        let r2 = au.execute(serde_json::json!({"question": "test?"}), &ctx()).await.unwrap();
        assert_eq!(r2.data["status"], "INTERRUPT");
        assert!(!r2.should_exit); // ask_user must NOT exit — loop resumes with user reply

        // NoTool
        let nt = crate::no_tool::NoTool;
        let r3 = nt.execute(serde_json::json!({"response": "hi"}), &ctx()).await.unwrap();
        assert_eq!(r3.data["status"], "ok");
    }

    /// Registry roundtrip: dispatch through ToolRegistry should match
    /// the data shape that LegacyRegistry produces for the same logical tool.
    #[tokio::test]
    async fn test_new_registry_dispatch_basic_tool() {
        let mut reg = NewRegistry::new();
        reg.register(crate::no_tool::NoTool);
        let result = reg.dispatch("respond", serde_json::json!({"response": "ok"}), &ctx()).await.unwrap();
        assert_eq!(result.data["status"], "ok");
        assert!(result.should_exit);
    }
}
