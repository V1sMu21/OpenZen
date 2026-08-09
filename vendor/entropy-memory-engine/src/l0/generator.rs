use std::sync::{Arc, RwLock};

use crate::core::types::MemoryContent;
use crate::l0::soul::{SoulHandle, SoulModel};
use crate::memory_store::MemoryStore;

pub struct GeneratorConfig {
    pub interval_secs: u64,
    pub max_self_chars: usize,
    pub context_window_hours: u64,
}

impl Default for GeneratorConfig {
    fn default() -> Self {
        Self {
            interval_secs: 3600,
            max_self_chars: 8000,
            context_window_hours: 24,
        }
    }
}

pub struct SelfModelGenerator {
    config: GeneratorConfig,
    current: SoulHandle,
    store: Arc<MemoryStore>,
}

impl SelfModelGenerator {
    pub fn new(config: GeneratorConfig, store: Arc<MemoryStore>) -> Self {
        Self {
            config,
            current: Arc::new(RwLock::new(SoulModel::new())),
            store,
        }
    }

    pub fn current_model(&self) -> SoulHandle {
        Arc::clone(&self.current)
    }

    /// 周期性刷新灵魂模型。
    ///
    /// 1. 快照当前 `SoulModel`；
    /// 2. 从 L3/L2 构建情境上下文；
    /// 3. 若上下文非空，MLX 增强（JSON 往返，失败则保留原模型）；
    /// 4. 写回并递增版本号。
    pub fn generate(&self) {
        let model = {
            let current = self.current.read().unwrap();
            current.clone()
        };

        let context = self.build_context();
        let updated = if context.trim().is_empty() {
            model
        } else {
            self.generate_with_mlx(&model.to_json(), &context)
                .and_then(|out| SoulModel::from_json(&out).ok())
                .unwrap_or(model)
        };

        {
            let mut current = self.current.write().unwrap();
            *current = updated;
            current.bump_version();
        }
    }

    fn build_context(&self) -> String {
        let mut parts = Vec::new();

        let cutoff = crate::core::now_nanos()
            - (self.config.context_window_hours as i64 * 3600 * 1_000_000_000);

        // Collect L3 summaries from recent period
        let l3 = self.store.router().l3();
        let all = l3.storage().all();
        for mem in &all {
            if mem.metadata.created_at >= cutoff {
                match &mem.content {
                    MemoryContent::Summary(s) => {
                        parts.push(format!("[L3] {}", s));
                    }
                    MemoryContent::Fact(f) => {
                        parts.push(format!("[L3] {} {} {}", f.subject, f.predicate, f.object));
                    }
                    _ => {}
                }
            }
            if parts.len() >= 50 {
                break;
            }
        }

        // Add L2 high-importance nodes
        let l2 = self.store.router().l2();
        for &id in &l2.storage.all_ids() {
            if let Some(mem) = l2.get_by_id(id) {
                if mem.metadata.importance > 0.6 {
                    parts.push(format!("[L2] {}", mem.content_text()));
                }
            }
            if parts.len() >= 100 {
                break;
            }
        }

        parts.join("\n")
    }

    fn generate_with_mlx(&self, current_state: &str, context: &str) -> Option<String> {
        let safe_context = context.replace('"', r#"\""#).replace('\n', " ");
        let safe_state = current_state.replace('"', r#"\""#);

        let script = format!(
            r#"import sys, json
try:
    import mlx.core as mx
    from mlx_lm import load, generate
    model, tokenizer = load("mlx-community/Llama-3.2-1B-Instruct-4bit")
    prompt = '''You maintain a soul-model for an AI memory system. Based on the current state and recent context, produce an updated JSON soul-model (fields: core, state, self_portrait, user_portrait, relationship, narrative, version). Only change fields that need updating. Keep total output under {} characters. Current state: {} Recent context: {} Output ONLY valid JSON, no other text:'''
    messages = [{{"role": "user", "content": prompt}}]
    formatted = tokenizer.apply_chat_template(messages, add_generation_prompt=True)
    response = generate(model, tokenizer, prompt=formatted, max_tokens=256)
    print(response.strip())
except Exception as e:
    print(json.dumps({{"error": str(e)}}))
"#,
            self.config.max_self_chars, safe_state, safe_context
        );

        let python = crate::core::detect_python();
        let output = std::process::Command::new(python)
            .arg("-c")
            .arg(&script)
            .output()
            .ok()?;

        if output.status.success() {
            let stdout = String::from_utf8_lossy(&output.stdout).trim().to_string();
            if !stdout.contains("error") {
                return Some(stdout);
            }
        }
        None
    }

    pub fn config(&self) -> &GeneratorConfig {
        &self.config
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::consolidation::ConsolidationConfig;
    use crate::core::types::{Fact, MemoryContent, MemoryInput};
    use crate::l1::L1Cache;
    use crate::l2::{HnswConfig, L2Config, L2Engine};
    use crate::l3::{BudgetConfig, L3Config, L3Engine};
    use tempfile::tempdir;

    fn make_store() -> Arc<MemoryStore> {
        let l1 = L1Cache::builder().capacity(100).build();
        let l2 = L2Engine::new(L2Config {
            hnsw: HnswConfig {
                dimension: 16,
                ..Default::default()
            },
            dimension: 16,
            ..Default::default()
        });
        let dir = tempdir().unwrap();
        let l3 = L3Engine::new(L3Config {
            storage_path: dir.path().join("l0_test.bin"),
            budget: BudgetConfig {
                daily_token_limit: 1_000_000,
                annual_storage_limit: 10_000_000,
            },
            ..Default::default()
        });
        Arc::new(MemoryStore::new(l1, Arc::new(l2), l3, ConsolidationConfig::default()))
    }

    #[test]
    fn test_generator_creates_model() {
        let store = make_store();
        let gen = SelfModelGenerator::new(GeneratorConfig::default(), store);
        let model = gen.current_model();
        let model = model.read().unwrap();
        assert!(model.core.identity.contains("记忆体"));
    }

    #[test]
    fn test_generate_with_empty_store() {
        let store = make_store();
        let gen = SelfModelGenerator::new(GeneratorConfig::default(), store);
        gen.generate();
        let binding = gen.current_model();
        let model = binding.read().unwrap();
        assert!(model.state.last_updated_nanos > 0);
    }

    #[test]
    fn test_generate_with_data() {
        let store = make_store();
        store
            .store(MemoryInput::new(MemoryContent::Fact(Fact::new(
                "test", "is", "working",
            ))))
            .unwrap();
        store
            .store(MemoryInput::new(MemoryContent::Summary(
                "user prefers Rust".into(),
            )))
            .unwrap();

        let gen = SelfModelGenerator::new(GeneratorConfig::default(), store);
        gen.generate();
        let binding = gen.current_model();
        let model = binding.read().unwrap();
        assert!(model.state.last_updated_nanos > 0);
    }

    #[test]
    fn test_config_defaults() {
        let config = GeneratorConfig::default();
        assert_eq!(config.interval_secs, 3600);
        assert_eq!(config.max_self_chars, 8000);
    }
}
