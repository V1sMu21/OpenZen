use std::io::{BufRead, BufReader, Write};
use std::process::{Child, Command, Stdio};
use std::sync::Mutex;

/// Configuration for the embedding model.
#[derive(Debug, Clone)]
pub struct EmbeddingConfig {
    /// Dimension of the embedding vectors.
    pub dimension: usize,
    /// Whether to use MLX for real neural embeddings (requires mlx python package).
    pub enable_mlx: bool,
    /// MLX model name on HuggingFace (e.g. "mlx-community/all-MiniLM-L6-v2-4bit").
    pub mlx_model: String,
}

impl Default for EmbeddingConfig {
    fn default() -> Self {
        Self {
            dimension: 384,
            enable_mlx: true,
            mlx_model: "mlx-community/all-MiniLM-L6-v2-4bit".to_string(),
        }
    }
}

/// Native output dimension of the default MLX embedding model
/// (`mlx-community/all-MiniLM-L6-v2-4bit` → all-MiniLM-L6-v2 → 384).
pub const MLX_EMBEDDING_DIMENSION: usize = 384;

/// Trait for generating embedding vectors from text.
pub trait EmbeddingModel: Send + Sync {
    fn embed(&self, text: &str) -> Vec<f32>;
    fn dimension(&self) -> usize;
}

/// Hash-based deterministic embedding (fallback when no real model is available).
/// This is the same algorithm that was in L2Engine::text_to_vector.
pub struct HashEmbedding {
    dim: usize,
}

impl HashEmbedding {
    pub fn new(dim: usize) -> Self {
        Self { dim }
    }
}

impl EmbeddingModel for HashEmbedding {
    fn embed(&self, text: &str) -> Vec<f32> {
        let mut vec = vec![0.0_f32; self.dim];
        let text = text.to_lowercase();
        let bytes = text.as_bytes();

        for (i, &b) in bytes.iter().enumerate() {
            let hash = (i as u64)
                .wrapping_mul(6364136223846793005)
                .wrapping_add(b as u64);
            let idx = (hash as usize) % self.dim;
            let val = ((hash >> 32) & 0xFF) as f32 / 128.0 - 1.0;
            vec[idx] += val;
        }

        let norm: f32 = vec.iter().map(|x| x * x).sum::<f32>().sqrt();
        if norm > 0.0 {
            for x in &mut vec {
                *x /= norm;
            }
        }
        vec
    }

    fn dimension(&self) -> usize {
        self.dim
    }
}

/// MLX-based neural embedding via a persistent Python subprocess.
///
/// Spawns a Python process at construction that loads an MLX embedding model
/// and keeps it resident. Text is sent via stdin and embeddings read from stdout.
/// Falls back to `HashEmbedding` if the subprocess fails.
pub struct MLXEmbedding {
    process: Mutex<Option<Child>>,
    dim: usize,
    model_name: String,
    fallback: HashEmbedding,
}

impl MLXEmbedding {
    pub fn new(config: &EmbeddingConfig) -> Self {
        Self {
            process: Mutex::new(None),
            dim: config.dimension,
            model_name: config.mlx_model.clone(),
            fallback: HashEmbedding::new(config.dimension),
        }
    }

    fn start_process(&self) -> Option<Child> {
        let script = format!(
            r#"import sys, json, os
os.environ["TOKENIZERS_PARALLELISM"] = "false"
try:
    import mlx.core as mx
    from mlx_lm import load, generate
except ImportError as e:
    print(json.dumps({{"error": str(e)}}))
    sys.stdout.flush()
    sys.exit(1)

try:
    model, tokenizer = load("{}")
except Exception as e:
    print(json.dumps({{"error": str(e)}}))
    sys.stdout.flush()
    sys.exit(1)

for line in sys.stdin:
    text = line.strip()
    if not text:
        continue
    try:
        tokens = tokenizer.encode(text)
        # Simple mean-pooling embedding approximation
        # For production, use a proper embedding model or pooler
        if hasattr(model, 'embed'):
            emb = model.embed(mx.array(tokens)[None, :])
            vec = emb.mean(axis=1).squeeze().tolist()
        elif hasattr(model, 'pooler'):
            emb = model.pooler(mx.array(tokens)[None, :])
            vec = emb.squeeze().tolist()
        else:
            # Fallback: use last hidden state
            logits, _ = model(mx.array(tokens)[None, :])
            vec = logits[0, -1, :].tolist()
        print(json.dumps(vec))
    except Exception as e:
        print(json.dumps({{"error": str(e)}}))
    sys.stdout.flush()
"#,
            self.model_name
        );

        let python = crate::core::detect_python();
        let child = Command::new(python)
            .arg("-c")
            .arg(&script)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .ok()?;

        Some(child)
    }
}

impl EmbeddingModel for MLXEmbedding {
    fn embed(&self, text: &str) -> Vec<f32> {
        {
            let mut proc_lock = self.process.lock().unwrap();
            if proc_lock.is_none() {
                *proc_lock = self.start_process();
            }
            if proc_lock.is_none() {
                return self.fallback.embed(text);
            }
        }

        match self.embed_with_child(text) {
            Ok(v) => v,
            Err(()) => {
                let mut proc_lock = self.process.lock().unwrap();
                *proc_lock = self.start_process();
                self.embed_with_child(text)
                    .unwrap_or_else(|_| self.fallback.embed(text))
            }
        }
    }

    fn dimension(&self) -> usize {
        self.dim
    }
}

impl MLXEmbedding {
    fn embed_with_child(&self, text: &str) -> Result<Vec<f32>, ()> {
        let mut proc_lock = self.process.lock().map_err(|_| ())?;
        let child = proc_lock.as_mut().ok_or(())?;

        let stdin = child.stdin.as_mut().ok_or(())?;
        let stdout = child.stdout.as_mut().ok_or(())?;

        if writeln!(stdin, "{}", text).is_err() {
            return Err(());
        }

        let mut reader = BufReader::new(stdout);
        let mut response = String::new();
        reader.read_line(&mut response).map_err(|_| ())?;
        let trimmed = response.trim();
        if trimmed.is_empty() || trimmed.starts_with("{\"error\"") {
            return Err(());
        }

        let vec: Vec<f32> = serde_json::from_str(trimmed).map_err(|_| ())?;

        if vec.len() == self.dim {
            Ok(vec)
        } else if vec.len() > self.dim {
            Ok(vec[..self.dim].to_vec())
        } else {
            let mut padded = vec![0.0; self.dim];
            for (i, v) in vec.iter().enumerate() {
                padded[i] = *v;
            }
            Ok(padded)
        }
    }
}

/// Whether MLX neural embeddings will actually be used for this config.
///
/// Requires all of: MLX enabled, a dimension matching the model's native
/// output (a padded/truncated mismatch would silently degrade quality), and
/// a working local MLX installation (checked once and cached).
pub fn uses_mlx_embeddings(config: &EmbeddingConfig) -> bool {
    config.enable_mlx
        && config.dimension == MLX_EMBEDDING_DIMENSION
        && crate::l3::compress::check_mlx_available()
}

/// Build the best available embedding model based on config.
pub fn build_embedding_model(config: &EmbeddingConfig) -> Box<dyn EmbeddingModel> {
    if uses_mlx_embeddings(config) {
        return Box::new(MLXEmbedding::new(config));
    }
    Box::new(HashEmbedding::new(config.dimension))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_hash_embedding_deterministic() {
        let h = HashEmbedding::new(16);
        let v1 = h.embed("hello world");
        let v2 = h.embed("hello world");
        let v3 = h.embed("different text");

        assert_eq!(v1.len(), 16);
        assert_eq!(v1, v2, "same text → same vector");
        assert_ne!(v1, v3, "different text → different vector");
    }

    #[test]
    fn test_hash_embedding_normalized() {
        let h = HashEmbedding::new(384);
        let v = h.embed("test vector");
        let norm: f32 = v.iter().map(|x| x * x).sum::<f32>().sqrt();
        assert!(
            (norm - 1.0).abs() < 0.001,
            "norm should be ~1.0, got {}",
            norm
        );
    }

    #[test]
    fn test_hash_embedding_empty_text() {
        let h = HashEmbedding::new(16);
        let v = h.embed("");
        let norm: f32 = v.iter().map(|x| x * x).sum::<f32>().sqrt();
        assert!(
            (norm - 0.0).abs() < 0.001 || (norm - 1.0).abs() < 0.001,
            "empty text should be near-zero or normalized, norm={}",
            norm
        );
    }

    #[test]
    fn test_hash_embedding_dimension() {
        let h = HashEmbedding::new(128);
        assert_eq!(h.dimension(), 128);
        let v = h.embed("check dim");
        assert_eq!(v.len(), 128);
    }

    #[test]
    fn test_uses_mlx_embeddings_dimension_gate() {
        let cfg_16 = EmbeddingConfig {
            enable_mlx: true,
            dimension: 16,
            ..Default::default()
        };
        assert!(
            !uses_mlx_embeddings(&cfg_16),
            "non-native dimension must stay on HashEmbedding"
        );
        let cfg_disabled = EmbeddingConfig {
            enable_mlx: false,
            dimension: 384,
            ..Default::default()
        };
        assert!(
            !uses_mlx_embeddings(&cfg_disabled),
            "disabled must stay on HashEmbedding"
        );
    }

    #[test]
    fn test_build_embedding_model_fallback() {
        // Without MLX, should return HashEmbedding
        let config = EmbeddingConfig {
            enable_mlx: false,
            dimension: 64,
            ..Default::default()
        };
        let model = build_embedding_model(&config);
        assert_eq!(model.dimension(), 64);
        let v = model.embed("test");
        assert_eq!(v.len(), 64);
    }

    #[test]
    fn test_mlx_embedding_fallback_when_unavailable() {
        // On systems without MLX, MLXEmbedding should fall back to HashEmbedding
        let config = EmbeddingConfig {
            enable_mlx: true,
            dimension: 16,
            ..Default::default()
        };
        let model = build_embedding_model(&config);
        let v = model.embed("hello world");
        assert_eq!(v.len(), 16);
        // Should be a valid float vector
        assert!(v.iter().all(|x| x.is_finite()));
    }
}
