pub mod error;
pub mod traits;
pub mod types;

pub use error::{MemoryError, MemoryResult};
pub use traits::{EvictionPolicy, MemoryLayer};
pub use types::{
    extract_keywords, generate_memory_id, now_nanos, Fact, KnowledgeSource, LayerId, Memory,
    MemoryContent, MemoryInput, MemoryMeta, Query,
};

pub fn detect_python() -> &'static str {
    if let Ok(path) = std::env::var("MLX_PYTHON_PATH") {
        if !path.is_empty() {
            // Leak is intentional — the string lives for the program's lifetime
            return Box::leak(path.into_boxed_str());
        }
    }
    if cfg!(target_os = "windows") {
        "python"
    } else {
        "python3"
    }
}
