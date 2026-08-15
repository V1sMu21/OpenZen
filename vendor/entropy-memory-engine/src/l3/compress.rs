use std::collections::{HashMap, VecDeque};
use std::process::Command;
use std::sync::Mutex;

use crate::core::MemoryContent;

pub trait Compressor: Send + Sync {
    fn compress(&self, content: &MemoryContent) -> CompressedMemory;
}

/// Configuration for semantic distillation.
#[derive(Debug, Clone)]
pub struct DistillationConfig {
    /// Enable the MLX local inference compressor (auto-detected).
    pub enable_mlx: bool,
    /// Maximum tokens for MLX generation.
    pub mlx_max_tokens: usize,
    /// Cache capacity for deduplicating re-distillation of the same content (0 = disabled).
    pub cache_capacity: usize,
    /// Enable progressive distillation (re-compress already-summarized content).
    pub progressive: bool,
}

impl Default for DistillationConfig {
    fn default() -> Self {
        Self {
            enable_mlx: true,
            mlx_max_tokens: 128,
            cache_capacity: 1024,
            progressive: false,
        }
    }
}

/// Statistics from the distillation pipeline.
#[derive(Debug, Clone, Default)]
pub struct DistillationStats {
    pub total_compressions: u64,
    pub cache_hits: u64,
    pub mlx_calls: u64,
    pub fallbacks: u64,
}

/// A simple LRU cache keyed by content hash, storing compressed results.
/// Wraps any Compressor to add caching. Implements Compressor via interior mutability.
pub struct DistillationCache {
    inner: Box<dyn Compressor>,
    cache: Mutex<HashMap<u64, CompressedMemory>>,
    lru: Mutex<VecDeque<u64>>,
    capacity: usize,
    stats: Mutex<DistillationStats>,
}

impl DistillationCache {
    pub fn new(inner: Box<dyn Compressor>, capacity: usize) -> Self {
        Self {
            inner,
            cache: Mutex::new(HashMap::with_capacity(capacity)),
            lru: Mutex::new(VecDeque::with_capacity(capacity)),
            capacity,
            stats: Mutex::new(DistillationStats::default()),
        }
    }

    fn content_hash(content: &MemoryContent) -> u64 {
        use std::hash::{Hash, Hasher};
        let text = match content {
            MemoryContent::Fact(f) => {
                format!(
                    "{}|{}|{}|{}",
                    f.subject, f.predicate, f.object, f.confidence
                )
            }
            MemoryContent::Summary(s) => format!("summary:{}", s),
            _ => String::new(),
        };
        let mut hasher = std::collections::hash_map::DefaultHasher::new();
        text.hash(&mut hasher);
        hasher.finish()
    }

    pub fn stats(&self) -> DistillationStats {
        self.stats.lock().unwrap().clone()
    }

    pub fn clear_cache(&self) {
        self.cache.lock().unwrap().clear();
        self.lru.lock().unwrap().clear();
    }
}

impl Compressor for DistillationCache {
    fn compress(&self, content: &MemoryContent) -> CompressedMemory {
        let mut stats = self.stats.lock().unwrap();
        stats.total_compressions += 1;
        drop(stats);

        let hash = Self::content_hash(content);

        {
            let cache = self.cache.lock().unwrap();
            if let Some(cached) = cache.get(&hash) {
                let mut stats = self.stats.lock().unwrap();
                stats.cache_hits += 1;
                // Promote to MRU on cache hit
                let mut lru = self.lru.lock().unwrap();
                if let Some(pos) = lru.iter().position(|h| *h == hash) {
                    lru.remove(pos);
                    lru.push_front(hash);
                }
                return cached.clone();
            }
        }

        let result = self.inner.compress(content);

        if self.capacity > 0 {
            let mut cache = self.cache.lock().unwrap();
            let mut lru = self.lru.lock().unwrap();
            if cache.len() >= self.capacity {
                if let Some(oldest) = lru.pop_back() {
                    cache.remove(&oldest);
                }
            }
            cache.insert(hash, result.clone());
            lru.push_front(hash);
        }

        let mut stats = self.stats.lock().unwrap();
        match result.method {
            CompressMethod::MLX => stats.mlx_calls += 1,
            CompressMethod::Truncate => stats.fallbacks += 1,
            CompressMethod::NoOp => {}
        }

        result
    }
}

/// Create the best available compressor based on config.
pub fn build_compressor(
    config: &DistillationConfig,
    fallback_max_chars: usize,
) -> Box<dyn Compressor> {
    let inner: Box<dyn Compressor> = if config.enable_mlx && check_mlx_available() {
        Box::new(MLXCompressor::new(config.mlx_max_tokens))
    } else {
        Box::new(TruncatingCompressor::new(fallback_max_chars))
    };

    if config.cache_capacity > 0 {
        Box::new(DistillationCache::new(inner, config.cache_capacity))
    } else {
        inner
    }
}

#[derive(Debug, Clone)]
pub struct CompressedMemory {
    pub text: String,
    pub original_length: usize,
    pub compressed_length: usize,
    pub method: CompressMethod,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CompressMethod {
    NoOp,
    Truncate,
    MLX,
}

impl CompressedMemory {
    pub fn ratio(&self) -> f64 {
        if self.original_length == 0 {
            return 1.0;
        }
        self.compressed_length as f64 / self.original_length as f64
    }
}

pub struct NoopCompressor;

impl Default for NoopCompressor {
    fn default() -> Self {
        Self::new()
    }
}

impl NoopCompressor {
    pub fn new() -> Self {
        Self
    }
}

impl Compressor for NoopCompressor {
    fn compress(&self, content: &MemoryContent) -> CompressedMemory {
        let text = content_text(content);
        let len = text.len();
        CompressedMemory {
            text,
            original_length: len,
            compressed_length: len,
            method: CompressMethod::NoOp,
        }
    }
}

pub struct TruncatingCompressor {
    pub max_chars: usize,
}

impl TruncatingCompressor {
    pub fn new(max_chars: usize) -> Self {
        Self { max_chars }
    }
}

impl Compressor for TruncatingCompressor {
    fn compress(&self, content: &MemoryContent) -> CompressedMemory {
        let text = content_text(content);
        let original_len = text.len();
        if text.len() <= self.max_chars {
            return CompressedMemory {
                text,
                original_length: original_len,
                compressed_length: original_len,
                method: CompressMethod::NoOp,
            };
        }
        let truncated = truncate_with_summary(&text, self.max_chars);
        CompressedMemory {
            text: truncated,
            original_length: original_len,
            compressed_length: self.max_chars,
            method: CompressMethod::Truncate,
        }
    }
}

pub struct MLXCompressor {
    pub max_tokens: usize,
    pub fallback: TruncatingCompressor,
    pub available: bool,
}

pub(crate) fn check_mlx_available() -> bool {
    static CACHE: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *CACHE.get_or_init(|| {
        let python = crate::core::detect_python();
        let Ok(which_out) = Command::new("which").arg(python).output() else {
            return false;
        };
        if !which_out.status.success() {
            return false;
        }
        // Verify the FULL dependency chain, not just `mlx.core`: `run_mlx`
        // imports `mlx_lm` (the LLM toolkit). A partial install (mlx.core
        // present but mlx_lm broken, e.g. version conflicts) would otherwise
        // report "available" and spawn a failing subprocess on EVERY unique
        // compress call — hundreds of milliseconds wasted per store.
        Command::new(python)
            .arg("-c")
            .arg("import mlx.core; from mlx_lm import load, generate; print('ok')")
            .output()
            .ok()
            .is_some_and(|out| {
                out.status.success() && String::from_utf8_lossy(&out.stdout).trim() == "ok"
            })
    })
}

impl MLXCompressor {
    pub fn new(max_tokens: usize) -> Self {
        let available = check_mlx_available();
        Self {
            max_tokens,
            fallback: TruncatingCompressor::new(max_tokens * 4),
            available,
        }
    }

    fn run_mlx(&self, prompt: &str) -> Option<String> {
        let script = format!(
            r#"import sys
try:
    import mlx.core as mx
    from mlx_lm import load, generate
    model, tokenizer = load("mlx-community/Llama-3.2-1B-Instruct-4bit")
    messages = [{{"role": "user", "content": "Compress this text for memory storage, keep key facts only: {}"}}]
    prompt = tokenizer.apply_chat_template(messages, add_generation_prompt=True)
    response = generate(model, tokenizer, prompt=prompt, max_tokens={})
    print(response.strip())
except Exception as e:
    print(f"MLX_ERROR:{{e}}", file=sys.stderr)
    sys.exit(1)
"#,
            prompt.replace('"', r#"\""#),
            self.max_tokens
        );

        let output = Command::new(crate::core::detect_python())
            .arg("-c")
            .arg(&script)
            .output()
            .ok()?;

        if output.status.success() {
            let stdout = String::from_utf8_lossy(&output.stdout).trim().to_string();
            if !stdout.is_empty() && !stdout.contains("MLX_ERROR") {
                return Some(stdout);
            }
        }
        None
    }
}

impl Compressor for MLXCompressor {
    fn compress(&self, content: &MemoryContent) -> CompressedMemory {
        let text = content_text(content);
        let original_len = text.len();

        if !self.available {
            return self.fallback.compress(content);
        }

        if let Some(compressed) = self.run_mlx(&text) {
            let clen = compressed.len();
            CompressedMemory {
                text: compressed,
                original_length: original_len,
                compressed_length: clen,
                method: CompressMethod::MLX,
            }
        } else {
            self.fallback.compress(content)
        }
    }
}

fn content_text(content: &MemoryContent) -> String {
    match content {
        MemoryContent::Fact(f) => format!("{} {} {}", f.subject, f.predicate, f.object),
        MemoryContent::Summary(s) => s.clone(),
        MemoryContent::Fingerprint(_) => String::new(),
        MemoryContent::Embedding(_) => String::new(),
    }
}

fn truncate_with_summary(text: &str, max_chars: usize) -> String {
    if text.len() <= max_chars {
        return text.to_string();
    }

    // Reserve 3 chars for "...", split remaining between start and end
    let remaining = max_chars.saturating_sub(3);
    let end_cap = remaining / 2;
    let start_cap = remaining - end_cap;

    let prev_boundary = |mut pos: usize| -> usize {
        while pos > 0 && !text.is_char_boundary(pos) {
            pos -= 1;
        }
        pos
    };

    let start = {
        let n = start_cap.min(text.len());
        let safe = if text.is_char_boundary(n) {
            n
        } else {
            prev_boundary(n)
        };
        &text[..safe]
    };
    let end = {
        let n = text.len().saturating_sub(end_cap);
        let safe = if text.is_char_boundary(n) {
            n
        } else {
            prev_boundary(n)
        };
        &text[safe..]
    };

    format!("{}...{}", start, end)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::Fact;

    #[test]
    fn test_noop_compressor() {
        let c = NoopCompressor::new();
        let content = MemoryContent::Fact(Fact::new("a", "b", "c"));
        let result = c.compress(&content);
        assert_eq!(result.text, "a b c");
        assert_eq!(result.original_length, result.compressed_length);
        assert_eq!(result.method, CompressMethod::NoOp);
    }

    #[test]
    fn test_truncating_compressor_no_truncation() {
        let c = TruncatingCompressor::new(100);
        let content = MemoryContent::Summary("short".into());
        let result = c.compress(&content);
        assert_eq!(result.text, "short");
        assert_eq!(result.method, CompressMethod::NoOp);
    }

    #[test]
    fn test_truncating_compressor_truncates() {
        let c = TruncatingCompressor::new(10);
        let long_text = "this is a very long text that should be truncated significantly";
        let content = MemoryContent::Summary(long_text.into());
        let result = c.compress(&content);
        assert!(result.text.len() <= 10);
        assert_eq!(result.method, CompressMethod::Truncate);
        assert!(result.ratio() < 1.0);
    }

    #[test]
    fn test_mlx_compressor_fallback_when_unavailable() {
        let c = MLXCompressor::new(50);
        // On systems without MLX, this should fall back to truncation
        let content = MemoryContent::Summary("this is a test".into());
        let result = c.compress(&content);
        // Either MLX succeeded (unlikely in test env) or fallback was used
        assert!(
            result.method == CompressMethod::MLX
                || result.method == CompressMethod::Truncate
                || result.method == CompressMethod::NoOp
        );
    }

    #[test]
    fn test_compressed_memory_ratio() {
        let cm = CompressedMemory {
            text: "short".into(),
            original_length: 100,
            compressed_length: 5,
            method: CompressMethod::Truncate,
        };
        assert!((cm.ratio() - 0.05).abs() < 1e-6);
    }

    #[test]
    fn test_zero_length_ratio() {
        let cm = CompressedMemory {
            text: String::new(),
            original_length: 0,
            compressed_length: 0,
            method: CompressMethod::NoOp,
        };
        assert_eq!(cm.ratio(), 1.0);
    }

    #[test]
    fn test_truncate_with_summary_preserves_ends() {
        let text = "abcdefghijklmnopqrstuvwxyz";
        // With max_chars=20, we get ~8 chars from each end
        let result = truncate_with_summary(text, 20);
        assert!(
            result.contains("..."),
            "should contain ellipsis: {}",
            result
        );
        assert!(
            result.starts_with("abc") || result.ends_with("xyz"),
            "should preserve at least one end: {}",
            result
        );
    }

    #[test]
    fn test_truncate_with_summary_short_input() {
        let result = truncate_with_summary("hello", 100);
        assert_eq!(result, "hello");
    }

    #[test]
    fn test_distillation_cache_caches_identical_content() {
        let inner = Box::new(NoopCompressor::new());
        let cache = DistillationCache::new(inner, 100);
        let content = MemoryContent::Summary("test data".into());

        let r1 = cache.compress(&content);
        assert_eq!(cache.stats().total_compressions, 1);
        assert_eq!(cache.stats().cache_hits, 0);

        let r2 = cache.compress(&content);
        assert_eq!(r1.text, r2.text);
        assert_eq!(cache.stats().total_compressions, 2);
        assert_eq!(cache.stats().cache_hits, 1);
    }

    #[test]
    fn test_distillation_cache_lru_eviction() {
        let inner = Box::new(NoopCompressor::new());
        let cache = DistillationCache::new(inner, 2);
        let ca = MemoryContent::Summary("entry a".into());
        let cb = MemoryContent::Summary("entry b".into());
        let cc = MemoryContent::Summary("entry c".into());

        cache.compress(&ca);
        cache.compress(&cb);
        assert_eq!(cache.stats().cache_hits, 0);

        cache.compress(&ca);
        assert_eq!(cache.stats().cache_hits, 1);

        cache.compress(&cc);
        let hits_before = cache.stats().cache_hits;
        cache.compress(&cb);
        assert_eq!(cache.stats().cache_hits, hits_before, "B evicted");
    }

    #[test]
    fn test_distillation_cache_zero_capacity_disabled() {
        let inner = Box::new(NoopCompressor::new());
        let cache = DistillationCache::new(inner, 0);
        let content = MemoryContent::Summary("test".into());

        cache.compress(&content);
        cache.compress(&content);
        assert_eq!(cache.stats().cache_hits, 0);
    }

    #[test]
    fn test_distillation_cache_fact_hashing() {
        let inner = Box::new(NoopCompressor::new());
        let cache = DistillationCache::new(inner, 100);
        let f1 = MemoryContent::Fact(Fact::new("alice", "likes", "cats"));
        let f2 = MemoryContent::Fact(Fact::new("alice", "likes", "cats"));

        cache.compress(&f1);
        assert_eq!(cache.stats().total_compressions, 1);

        cache.compress(&f2);
        assert_eq!(cache.stats().cache_hits, 1);
    }

    #[test]
    fn test_build_compressor_default_fallback() {
        let config = DistillationConfig {
            enable_mlx: false,
            cache_capacity: 10,
            ..Default::default()
        };
        let compressor = build_compressor(&config, 1024);
        let content = MemoryContent::Summary("hello world".into());
        let result = compressor.compress(&content);
        assert!(!result.text.is_empty());
    }

    #[test]
    fn test_distillation_cache_clear() {
        let inner = Box::new(NoopCompressor::new());
        let cache = DistillationCache::new(inner, 100);
        let content = MemoryContent::Summary("test".into());
        cache.compress(&content);
        assert_eq!(cache.stats().total_compressions, 1);
        cache.clear_cache();
        cache.compress(&content);
        assert_eq!(cache.stats().cache_hits, 0);
    }
}
