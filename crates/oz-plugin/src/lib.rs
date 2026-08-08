use std::path::Path;
use std::sync::Mutex;

use async_trait::async_trait;
use oz_core_types::{ToolContext, ToolDefinition, ToolFunction, ToolOutput, ToolError};
use oz_tools::registry::ToolHandler;
use wasmtime::{Engine, Module, Store, Instance, Memory, TypedFunc};

const WASM_PAGE_SIZE: u64 = 65536;
const SCRATCH_OFFSET: usize = 65536;

#[derive(Debug, thiserror::Error)]
pub enum PluginError {
    #[error("Failed to load WASM module: {0}")]
    ModuleLoad(String),
    #[error("Missing required export '{0}' in WASM module")]
    MissingExport(&'static str),
    #[error("Failed to read string from WASM memory: {0}")]
    StringRead(String),
    #[error("Failed to call WASM function '{0}': {1}")]
    FunctionCall(&'static str, String),
    #[error("Plugin returned invalid JSON: {0}")]
    InvalidJson(String),
    #[error("WASM runtime error: {0}")]
    Runtime(String),
    #[error("WAT parse error: {0}")]
    WatParse(String),
}

impl From<wasmtime::Error> for PluginError {
    fn from(e: wasmtime::Error) -> Self {
        PluginError::Runtime(e.to_string())
    }
}

impl From<wat::Error> for PluginError {
    fn from(e: wat::Error) -> Self {
        PluginError::WatParse(e.to_string())
    }
}

fn read_wasm_string(memory: &Memory, store: &Store<()>, ptr: i32) -> Result<String, PluginError> {
    let mut result = Vec::new();
    let mut addr = ptr as usize;
    loop {
        let mut buf = [0u8; 1];
        memory
            .read(&*store, addr, &mut buf)
            .map_err(|e| PluginError::StringRead(e.to_string()))?;
        if buf[0] == 0 {
            break;
        }
        result.push(buf[0]);
        addr += 1;
    }
    String::from_utf8(result).map_err(|e| PluginError::StringRead(e.to_string()))
}

fn ensure_memory_size(memory: &mut Memory, store: &mut Store<()>, needed: u64) -> Result<(), PluginError> {
    let current_size = memory.size(&*store) as u64 * WASM_PAGE_SIZE;
    if needed > current_size {
        let pages_needed = (needed + WASM_PAGE_SIZE - 1) / WASM_PAGE_SIZE;
        let grow_by = pages_needed - memory.size(&*store) as u64;
        memory.grow(&mut *store, grow_by).map_err(|e| {
            PluginError::Runtime(format!("Failed to grow memory: {e}"))
        })?;
    }
    Ok(())
}

fn write_to_memory(memory: &mut Memory, store: &mut Store<()>, addr: usize, data: &[u8]) -> Result<(), PluginError> {
    ensure_memory_size(memory, store, (addr + data.len() + 1) as u64)?;
    memory.write(&mut *store, addr, data)
        .map_err(|e| PluginError::StringRead(e.to_string()))?;

    let end_byte = [0u8; 1];
    memory.write(&mut *store, addr + data.len(), &end_byte)
        .map_err(|e| PluginError::StringRead(e.to_string()))?;
    Ok(())
}

pub struct WasmPlugin {
    pub name: String,
    pub description: String,
    pub parameters: serde_json::Value,
    pub instance_id: String,
    store: Store<()>,
    _instance: Instance,
    execute_fn: TypedFunc<(i32, i32), i32>,
    memory: Memory,
}

impl WasmPlugin {
    pub fn from_file(path: impl AsRef<Path>) -> Result<Self, PluginError> {
        let engine = Engine::default();
        let module = Module::from_file(&engine, path.as_ref())?;
        Self::from_module(engine, module)
    }

    pub fn from_binary(bytes: &[u8]) -> Result<Self, PluginError> {
        let engine = Engine::default();
        let module = Module::new(&engine, bytes)?;
        Self::from_module(engine, module)
    }

    pub fn from_wat(wat_str: &str) -> Result<Self, PluginError> {
        let wasm_bytes = wat::parse_str(wat_str)?;
        Self::from_binary(&wasm_bytes)
    }

    fn from_module(engine: Engine, module: Module) -> Result<Self, PluginError> {
        let mut store = Store::new(&engine, ());
        let instance = Instance::new(&mut store, &module, &[])?;

        let memory: Memory = instance
            .get_export(&mut store, "memory")
            .and_then(|e| e.into_memory())
            .ok_or(PluginError::MissingExport("memory"))?;

        let name_fn = instance
            .get_typed_func::<(), i32>(&mut store, "tool_name")
            .map_err(|_| PluginError::MissingExport("tool_name"))?;
        let name_ptr = name_fn.call(&mut store, ())?;
        let name = read_wasm_string(&memory, &store, name_ptr)?;

        let desc_fn = instance
            .get_typed_func::<(), i32>(&mut store, "tool_description")
            .map_err(|_| PluginError::MissingExport("tool_description"))?;
        let desc_ptr = desc_fn.call(&mut store, ())?;
        let description = read_wasm_string(&memory, &store, desc_ptr)?;

        let params_fn = instance
            .get_typed_func::<(), i32>(&mut store, "tool_parameters")
            .map_err(|_| PluginError::MissingExport("tool_parameters"))?;
        let params_ptr = params_fn.call(&mut store, ())?;
        let params_str = read_wasm_string(&memory, &store, params_ptr)?;
        let parameters: serde_json::Value = serde_json::from_str(&params_str)
            .map_err(|e| PluginError::InvalidJson(e.to_string()))?;

        let execute_fn = instance
            .get_typed_func::<(i32, i32), i32>(&mut store, "execute")
            .map_err(|_| PluginError::MissingExport("execute"))?;

        // engine is kept alive by store internally
        drop(engine);
        Ok(WasmPlugin {
            name,
            description,
            parameters,
            instance_id: uuid::Uuid::new_v4().to_string(),
            store,
            _instance: instance,
            execute_fn,
            memory,
        })
    }

    pub fn execute(&mut self, args: serde_json::Value) -> Result<ToolOutput, PluginError> {
        let args_str = serde_json::to_string(&args)
            .map_err(|e| PluginError::InvalidJson(e.to_string()))?;
        let args_bytes = args_str.as_bytes();

        write_to_memory(&mut self.memory, &mut self.store, SCRATCH_OFFSET, args_bytes)?;

        let result_ptr = self.execute_fn
            .call(&mut self.store, (SCRATCH_OFFSET as i32, args_bytes.len() as i32))
            .map_err(|e| PluginError::FunctionCall("execute", e.to_string()))?;

        let result_str = read_wasm_string(&self.memory, &self.store, result_ptr)?;

        let result_value: serde_json::Value = serde_json::from_str(&result_str)
            .map_err(|e| PluginError::InvalidJson(format!("{e}: {result_str}")))?;

        let data = result_value.get("data").cloned().unwrap_or(serde_json::Value::Null);
        let next_prompt = result_value
            .get("next_prompt")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string());
        let should_exit = result_value
            .get("should_exit")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);

        Ok(ToolOutput { data, next_prompt, should_exit, images: vec![] })
    }

    pub fn to_definition(&self) -> ToolDefinition {
        ToolDefinition {
            type_: "function".into(),
            function: ToolFunction {
                name: self.name.clone(),
                description: self.description.clone(),
                parameters: self.parameters.clone(),
            },
        }
    }
}

pub struct WasmPluginHandler {
    plugin: Mutex<WasmPlugin>,
    name: String,
    description: String,
    parameters: serde_json::Value,
}

impl WasmPluginHandler {
    pub fn new(plugin: WasmPlugin) -> Self {
        let name = plugin.name.clone();
        let description = plugin.description.clone();
        let parameters = plugin.parameters.clone();
        WasmPluginHandler {
            plugin: Mutex::new(plugin),
            name,
            description,
            parameters,
        }
    }
}

#[async_trait]
impl ToolHandler for WasmPluginHandler {
    fn name(&self) -> String { self.name.clone() }

    fn description(&self) -> String { self.description.clone() }

    fn parameters(&self) -> serde_json::Value {
        self.parameters.clone()
    }

    async fn execute(
        &self,
        args: serde_json::Value,
        _ctx: &ToolContext,
    ) -> Result<ToolOutput, ToolError> {
        let mut plugin = self.plugin.lock().map_err(|e| {
            ToolError::Custom(format!("Plugin lock poisoned: {e}"))
        })?;
        plugin.execute(args).map_err(|e| {
            ToolError::Custom(format!("Plugin execution failed: {e}"))
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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

    fn test_plugin() -> WasmPlugin {
        WasmPlugin::from_wat(TEST_WAT).unwrap()
    }

    fn test_context() -> ToolContext {
        ToolContext {
            working_dir: ".".into(),
            assets_dir: ".".into(),
            script_dir: ".".into(),
            lang: "en".into(),
            skill_mcp_dir: None,
        }
    }

    #[test]
    fn test_load_from_wat() {
        let p = test_plugin();
        assert_eq!(p.name, "test_tool");
        assert_eq!(p.description, "A WASM test tool");
        assert!(p.parameters.is_object());
    }

    #[test]
    fn test_execute_plugin() {
        let mut p = test_plugin();
        let r = p.execute(serde_json::json!({"input": "hello"})).unwrap();
        assert_eq!(r.data, serde_json::json!("plugin ran"));
        assert!(!r.should_exit);
    }

    #[test]
    fn test_to_definition() {
        let p = test_plugin();
        let def = p.to_definition();
        assert_eq!(def.function.name, "test_tool");
        assert_eq!(def.function.description, "A WASM test tool");
    }

    #[test]
    fn test_handler_execute() {
        let handler = WasmPluginHandler::new(test_plugin());
        let rt = tokio::runtime::Runtime::new().unwrap();
        let r = rt.block_on(handler.execute(serde_json::json!({"x": 1}), &test_context())).unwrap();
        assert_eq!(r.data, serde_json::json!("plugin ran"));
    }

    #[test]
    fn test_missing_export_fails() {
        let r = WasmPlugin::from_wat(r#"(module (memory (export "memory") 1))"#);
        assert!(r.is_err());
    }

    #[test]
    fn test_from_binary_roundtrip() {
        let wasm = wat::parse_str(TEST_WAT).unwrap();
        let p = WasmPlugin::from_binary(&wasm).unwrap();
        assert_eq!(p.name, "test_tool");
    }

    #[test]
    fn test_error_display() {
        let e = PluginError::MissingExport("memory");
        assert!(e.to_string().contains("memory"));
    }
}
