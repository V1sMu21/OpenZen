use std::path::PathBuf;
use oz_tools::registry::ToolHandler;

#[test]
fn test_load_plugin_from_file() {
    let wasm_bytes = wat::parse_str(TEST_WAT).unwrap();
    let dir = std::env::temp_dir().join(format!("oz_plugin_test_{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&dir).unwrap();
    let wasm_path = dir.join("test_plugin.wasm");
    std::fs::write(&wasm_path, &wasm_bytes).unwrap();

    let plugin = oz_plugin::WasmPlugin::from_file(&wasm_path).unwrap();
    assert_eq!(plugin.name, "test_tool");
    assert_eq!(plugin.description, "A WASM test tool");

    let mut plugin = plugin;
    let result = plugin.execute(serde_json::json!({"input": "world"})).unwrap();
    assert_eq!(result.data, serde_json::json!("plugin ran"));

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn test_handler_integration() {
    let plugin = oz_plugin::WasmPlugin::from_wat(TEST_WAT).unwrap();
    let handler = oz_plugin::WasmPluginHandler::new(plugin);

    let rt = tokio::runtime::Runtime::new().unwrap();
    let result = rt.block_on(async {
        let ctx = oz_core_types::ToolContext {
            working_dir: ".".into(),
            assets_dir: ".".into(),
            script_dir: ".".into(),
            lang: "en".into(),
            skill_mcp_dir: None,
            harness_dir: None,
            session_id: String::new(),
        };
        handler.execute(serde_json::json!({"x": 1}), &ctx).await
    }).unwrap();
    assert_eq!(result.data, serde_json::json!("plugin ran"));
    assert!(!result.should_exit);
}

#[test]
fn test_multiple_plugin_instances() {
    let p1 = oz_plugin::WasmPlugin::from_wat(TEST_WAT).unwrap();
    let p2 = oz_plugin::WasmPlugin::from_wat(TEST_WAT).unwrap();
    assert_ne!(p1.instance_id, p2.instance_id);

    let mut p1 = p1;
    let mut p2 = p2;
    let r1 = p1.execute(serde_json::json!({"a": 1})).unwrap();
    let r2 = p2.execute(serde_json::json!({"b": 2})).unwrap();
    assert_eq!(r1.data, serde_json::json!("plugin ran"));
    assert_eq!(r2.data, serde_json::json!("plugin ran"));
}

#[test]
fn test_to_tool_registry() {
    let plugin = oz_plugin::WasmPlugin::from_wat(TEST_WAT).unwrap();
    let def = plugin.to_definition();
    assert_eq!(def.function.name, "test_tool");
    assert_eq!(def.function.description, "A WASM test tool");
    assert!(def.function.parameters.is_object());
}

#[test]
fn test_execute_with_empty_args() {
    let mut plugin = oz_plugin::WasmPlugin::from_wat(TEST_WAT).unwrap();
    let result = plugin.execute(serde_json::json!({})).unwrap();
    assert_eq!(result.data, serde_json::json!("plugin ran"));
}

#[test]
fn test_invalid_wat_fails() {
    let result = oz_plugin::WasmPlugin::from_wat("not valid wat");
    assert!(result.is_err());
}

#[test]
fn test_nonexistent_file_fails() {
    let result = oz_plugin::WasmPlugin::from_file(
        PathBuf::from("/nonexistent/plugin.wasm")
    );
    assert!(result.is_err());
}

const TEST_WAT: &str = r#"
(module
    (memory (export "memory") 1)
    (func (export "tool_name") (result i32)
        i32.const 0
    )
    (func (export "tool_description") (result i32)
        i32.const 10
    )
    (func (export "tool_parameters") (result i32)
        i32.const 28
    )
    (func (export "execute") (param i32 i32) (result i32)
        i32.const 100
    )
    (data (i32.const 0) "test_tool\00")
    (data (i32.const 10) "A WASM test tool\00")
    (data (i32.const 28) "{\22type\22:\22object\22,\22properties\22:{}}\00")
    (data (i32.const 100) "{\22data\22:\22plugin ran\22,\22next_prompt\22:\22\5cn\22,\22should_exit\22:false}\00")
)
"#;
