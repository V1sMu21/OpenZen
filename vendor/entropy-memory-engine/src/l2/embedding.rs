use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{mpsc, Mutex};
use std::time::{Duration, SystemTime};

/// How long one embed request may take before the child counts as wedged.
///
/// Generous on purpose: the first request also covers the model load. A child
/// that blows through it is dropped and the embedder degrades to hash.
/// `OPENZEN_EMBED_TIMEOUT_MS` overrides it (the tests use that to stay fast).
fn embed_reply_timeout() -> Duration {
    std::env::var("OPENZEN_EMBED_TIMEOUT_MS")
        .ok()
        .and_then(|ms| ms.parse::<u64>().ok())
        .map(Duration::from_millis)
        .unwrap_or(DEFAULT_EMBED_REPLY_TIMEOUT)
}

const DEFAULT_EMBED_REPLY_TIMEOUT: Duration = Duration::from_secs(45);

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

/// A live helper process plus the thread that keeps draining its stdout.
///
/// One vector per line is ~8 KB of JSON: reading strictly request-by-request
/// lets the child fill its 64 KB stdout pipe, block mid-write and stop reading
/// stdin — after which the parent blocks writing into a full stdin pipe, and
/// both sides sit wedged while the embedding mutex is held. That is precisely
/// how recall (and with it every agent turn) stops answering. The reader thread
/// keeps the pipe empty however the two sides interleave, and each request is
/// bounded by a reply deadline so a wedged child gets dropped instead of
/// hung on.
struct EmbeddingChild {
    child: Child,
    responses: mpsc::Receiver<String>,
    reader: Option<std::thread::JoinHandle<()>>,
}

/// MLX-based neural embedding via a persistent Python subprocess.
///
/// Spawns a Python process at construction that loads an MLX embedding model
/// and keeps it resident. Text is sent via stdin and embeddings read from stdout.
/// Falls back to `HashEmbedding` if the subprocess fails.
pub struct MLXEmbedding {
    process: Mutex<Option<EmbeddingChild>>,
    /// Set once a child failed and its replacement failed too. A broken MLX
    /// install (model missing, network down, load error) must not spawn a
    /// Python process on every single embed call.
    disabled: AtomicBool,
    dim: usize,
    model_name: String,
    fallback: HashEmbedding,
}

impl MLXEmbedding {
    pub fn new(config: &EmbeddingConfig) -> Self {
        Self {
            process: Mutex::new(None),
            disabled: AtomicBool::new(false),
            dim: config.dimension,
            model_name: config.mlx_model.clone(),
            fallback: HashEmbedding::new(config.dimension),
        }
    }

    /// Kill and reap a child we are replacing, so it cannot outlive the
    /// handle we just dropped. Closing its stdout also ends the reader thread.
    fn reap(child: Option<EmbeddingChild>) {
        if let Some(mut io) = child {
            let _ = io.child.kill();
            let _ = io.child.wait();
            if let Some(reader) = io.reader.take() {
                drop(reader);
            }
        }
    }

    fn start_process(&self) -> Option<EmbeddingChild> {
        let python = crate::core::detect_python();
        // The model arrives as argv: a resolved local directory when this
        // machine has the weights, the hub id otherwise.
        let model = resolve_local_model(&self.model_name);
        if model != self.model_name {
            tracing::info!(
                "MLX embedding: using local model {} for {}",
                model,
                self.model_name
            );
        }
        let mut child = Command::new(python)
            .arg("-c")
            .arg(SENTENCE_HELPER_SCRIPT)
            .arg(&model)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .ok()?;

        let stdout = child.stdout.take()?;
        let (tx, responses) = mpsc::channel();
        let reader = std::thread::Builder::new()
            .name("mlx-embed-reader".into())
            .spawn(move || {
                let mut lines = BufReader::new(stdout);
                loop {
                    let mut line = String::new();
                    match lines.read_line(&mut line) {
                        Ok(0) | Err(_) => break,
                        Ok(_) => {
                            if tx.send(line).is_err() {
                                break;
                            }
                        }
                    }
                }
            });
        let reader = match reader {
            Ok(handle) => handle,
            Err(_) => {
                // Without a reader nobody drains the pipe: drop the child.
                let _ = child.kill();
                let _ = child.wait();
                return None;
            }
        };

        Some(EmbeddingChild {
            child,
            responses,
            reader: Some(reader),
        })
    }
}

impl EmbeddingModel for MLXEmbedding {
    fn embed(&self, text: &str) -> Vec<f32> {
        if self.disabled.load(Ordering::Relaxed) {
            return self.fallback.embed(text);
        }

        {
            let mut proc_lock = self.process.lock().unwrap();
            if proc_lock.is_none() {
                *proc_lock = self.start_process();
            }
            if proc_lock.is_none() {
                self.disabled.store(true, Ordering::Relaxed);
                return self.fallback.embed(text);
            }
        }

        match self.embed_with_child(text) {
            Ok(v) => v,
            Err(()) => {
                // Replace the child and retry once. The guard has to be
                // released before the retry: `embed_with_child` locks the same
                // non-recursive mutex, so holding it across that call makes
                // this thread wait for a lock it already owns — a deadlock
                // that would also freeze every other embedder through this
                // mutex (recall is on the agent's critical path).
                {
                    let mut proc_lock = self.process.lock().unwrap();
                    Self::reap(proc_lock.take());
                    *proc_lock = self.start_process();
                }
                match self.embed_with_child(text) {
                    Ok(v) => v,
                    Err(()) => {
                        // A freshly spawned child failed too: stop paying for
                        // a Python process per embed and degrade to the hash
                        // fallback for the rest of this process's life.
                        tracing::warn!(
                            "MLX embedding child unusable (model={}); falling back to HashEmbedding",
                            self.model_name
                        );
                        {
                            let mut proc_lock = self.process.lock().unwrap();
                            Self::reap(proc_lock.take());
                        }
                        self.disabled.store(true, Ordering::Relaxed);
                        self.fallback.embed(text)
                    }
                }
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
        let io = proc_lock.as_mut().ok_or(())?;

        let stdin = io.child.stdin.as_mut().ok_or(())?;
        if writeln!(stdin, "{}", text).is_err() {
            return Err(());
        }

        // Bounded wait: a child that stopped answering must never block recall
        // — and, through this mutex, every other embedder — forever.
        let response = io.responses.recv_timeout(embed_reply_timeout()).map_err(|_| ())?;
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
    // D1 (round3): the hash fallback makes semantic similarity near-random,
    // so silent fallback directly degrades "recall what we discussed".
    // Scream about it once at startup with the actionable reason.
    tracing::warn!(
        "ERME embeddings falling back to HashEmbedding (semantic recall will be weak).          enable_mlx={} dimension={} (MLX needs {MLX_EMBEDDING_DIMENSION}) mlx_available={}",
        config.enable_mlx,
        config.dimension,
        crate::l3::compress::check_mlx_available(),
    );
    Box::new(HashEmbedding::new(config.dimension))
}

/// The persistent embedding helper: one Python process per embedder, one text
/// per line on stdin, one JSON vector (or `{"error": ...}`) per line on stdout.
///
/// Kept as a plain `const` rather than a `format!` template: the model path is
/// passed as `argv[1]`, so a path can never be spliced into Python source.
const SENTENCE_HELPER_SCRIPT: &str = r#"import sys, json, os, types
os.environ["TOKENIZERS_PARALLELISM"] = "false"

MODEL = sys.argv[1] if len(sys.argv) > 1 else "mlx-community/all-MiniLM-L6-v2-4bit"

# mlx_embeddings 0.0.1 imports a huggingface_hub private path that newer
# releases moved. Register the alias when the real module is gone so the
# package stays importable instead of failing every embed.
try:
    from huggingface_hub.utils._errors import RepositoryNotFoundError  # noqa: F401
except Exception:
    try:
        import huggingface_hub.errors as _hf_errors
        _compat = types.ModuleType("huggingface_hub.utils._errors")
        _compat.RepositoryNotFoundError = _hf_errors.RepositoryNotFoundError
        sys.modules["huggingface_hub.utils._errors"] = _compat
    except Exception:
        pass

try:
    import mlx.core as mx
except Exception as e:
    print(json.dumps({"error": "mlx.core: %s" % e}))
    sys.stdout.flush()
    sys.exit(1)

# Sentence-transformer models (BERT and friends) load through mlx_embeddings;
# mlx_lm only knows causal LMs and rejects them outright ("Model type bert not
# supported"). mlx_lm stays as the fallback for LM-style embedders.
kind = None
model = None
tokenizer = None
try:
    from mlx_embeddings import load as _load_sentence
    model, tokenizer = _load_sentence(MODEL)
    kind = "sentence"
except Exception as e_sentence:
    try:
        from mlx_lm import load as _load_lm
        model, tokenizer = _load_lm(MODEL)
        kind = "lm"
    except Exception as e_lm:
        print(json.dumps({"error": "mlx_embeddings: %s | mlx_lm: %s" % (e_sentence, e_lm)}))
        sys.stdout.flush()
        sys.exit(1)


def embed(text):
    tokens = tokenizer.encode(text)
    if kind == "sentence":
        # Mean-pool the encoder states, then L2-normalize: the same pooling
        # and metric the sentence-transformers model card prescribes, so the
        # vectors are comparable by distance.
        out = model(mx.array([tokens]))
        hidden = out[0] if isinstance(out, (tuple, list)) else out
        vec = hidden.mean(axis=1).squeeze().tolist()
        norm = sum(x * x for x in vec) ** 0.5
        return [x / norm for x in vec] if norm > 0 else vec
    if hasattr(model, "embed"):
        emb = model.embed(mx.array(tokens)[None, :])
        return emb.mean(axis=1).squeeze().tolist()
    if hasattr(model, "pooler"):
        emb = model.pooler(mx.array(tokens)[None, :])
        return emb.squeeze().tolist()
    logits, _ = model(mx.array(tokens)[None, :])
    return logits[0, -1, :].tolist()


for line in sys.stdin:
    text = line.strip()
    if not text:
        continue
    try:
        print(json.dumps(embed(text)))
    except Exception as e:
        print(json.dumps({"error": str(e)}))
    sys.stdout.flush()
"#;

/// Resolve a model id to a local directory when this machine already holds the
/// weights, so loading never waits on the hub (which is not always reachable —
/// and a child that must ask the network before it can embed turns a missing
/// download into a stalled recall).
///
/// Understands an explicit local path, the HuggingFace cache layout
/// (`models--owner--name/snapshots/<rev>`) and an unpacked store
/// (`<root>/owner/name`, which is how the oMLX model directory is laid out).
/// Anything else is returned unchanged for the Python side to resolve.
fn resolve_local_model(model: &str) -> String {
    find_local_model(model, &local_model_roots()).unwrap_or_else(|| model.to_string())
}

fn local_model_roots() -> Vec<PathBuf> {
    let mut roots = Vec::new();
    if let Some(hf_home) = std::env::var_os("HF_HOME") {
        roots.push(PathBuf::from(hf_home).join("hub"));
    }
    if let Some(cache) = std::env::var_os("HUGGINGFACE_HUB_CACHE") {
        roots.push(PathBuf::from(cache));
    }
    if let Some(home) = std::env::var_os("HOME") {
        let home = PathBuf::from(home);
        roots.push(home.join(".cache").join("huggingface").join("hub"));
        roots.push(home.join(".omlx").join("models"));
    }
    roots
}

fn find_local_model(model: &str, roots: &[PathBuf]) -> Option<String> {
    if Path::new(model).is_dir() {
        return Some(model.to_string());
    }
    let (owner, name) = model.split_once('/')?;
    for root in roots {
        if let Some(snapshot) = newest_snapshot(&root.join(format!("models--{owner}--{name}"))) {
            return Some(snapshot);
        }
        let unpacked = root.join(owner).join(name);
        if unpacked.join("config.json").is_file() {
            return Some(unpacked.to_string_lossy().into_owned());
        }
    }
    None
}

/// Newest `snapshots/<rev>` that actually carries a model config.
fn newest_snapshot(repo_dir: &Path) -> Option<String> {
    let mut newest: Option<(SystemTime, String)> = None;
    for entry in std::fs::read_dir(repo_dir.join("snapshots")).ok()? {
        let Ok(entry) = entry else { continue };
        let path = entry.path();
        if !path.join("config.json").is_file() {
            continue;
        }
        let modified = entry
            .metadata()
            .and_then(|m| m.modified())
            .unwrap_or(SystemTime::UNIX_EPOCH);
        if newest.as_ref().is_none_or(|(best, _)| modified > *best) {
            newest = Some((modified, path.to_string_lossy().into_owned()));
        }
    }
    newest.map(|(_, path)| path)
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

    /// A child that cannot serve embeddings must degrade to the hash fallback
    /// instead of deadlocking, and must not be respawned on every call.
    ///
    /// Regression: the retry path re-locked `process` while its own guard was
    /// still alive. A non-recursive mutex then blocked the thread against
    /// itself, and every other embedder (L2 search on the agent's recall path)
    /// waited on the same mutex forever — the app stopped answering messages
    /// entirely.
    #[test]
    fn test_failed_child_falls_back_without_deadlocking() {
        // A directory that looks like a model but holds no usable weights: the
        // child fails locally, exactly like the broken install in production.
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("config.json"), r#"{"model_type": "bert"}"#).unwrap();
        let model = MLXEmbedding::new(&EmbeddingConfig {
            enable_mlx: true,
            dimension: 384,
            mlx_model: dir.path().to_string_lossy().into_owned(),
        });
        let (tx, rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let first = model.embed("hello").len();
            let started = std::time::Instant::now();
            let second = model.embed("world").len();
            let _ = tx.send((first, second, started.elapsed()));
        });
        match rx.recv_timeout(std::time::Duration::from_secs(120)) {
            Ok((first, second, second_elapsed)) => {
                assert_eq!(first, 384, "fallback keeps the configured dimension");
                assert_eq!(second, 384);
                assert!(
                    second_elapsed < std::time::Duration::from_secs(2),
                    "a disabled embedder must not respawn Python per call, took {second_elapsed:?}"
                );
            }
            Err(_) => panic!("embed() never returned — the failed-child path deadlocked"),
        }
    }

    /// A child that accepts the request but never answers must not hang the
    /// caller: the reply deadline drops it and the embedder degrades to hash.
    /// Without the deadline this test never returns — and in the app the same
    /// state means recall, and every agent turn, waits forever.
    #[test]
    fn test_wedged_child_times_out_instead_of_hanging() {
        let dir = tempfile::tempdir().unwrap();
        let wedged = dir.path().join("wedged.sh");
        std::fs::write(&wedged, "#!/bin/sh\nsleep 300\n").unwrap();
        let mut perms = std::fs::metadata(&wedged).unwrap().permissions();
        std::os::unix::fs::PermissionsExt::set_mode(&mut perms, 0o755);
        std::fs::set_permissions(&wedged, perms).unwrap();

        // detect_python() honours MLX_PYTHON_PATH, so the wedged program takes
        // the child's place; the timeout is shortened for this test only.
        std::env::set_var("MLX_PYTHON_PATH", &wedged);
        std::env::set_var("OPENZEN_EMBED_TIMEOUT_MS", "700");
        let model = MLXEmbedding::new(&EmbeddingConfig {
            enable_mlx: true,
            dimension: 384,
            mlx_model: "irrelevant-for-this-test".to_string(),
        });
        let started = std::time::Instant::now();
        let vec = model.embed("hello");
        let elapsed = started.elapsed();
        std::env::remove_var("MLX_PYTHON_PATH");
        std::env::remove_var("OPENZEN_EMBED_TIMEOUT_MS");

        assert_eq!(vec.len(), 384, "degraded embedder keeps the dimension");
        assert!(
            elapsed < std::time::Duration::from_secs(20),
            "a wedged child must be dropped, not waited on, took {elapsed:?}"
        );
    }

    #[test]
    fn test_find_local_model_resolves_hf_cache_snapshot() {
        let tmp = tempfile::tempdir().unwrap();
        let snapshots = tmp.path().join("models--acme--mini").join("snapshots");
        let older = snapshots.join("aaa");
        let newer = snapshots.join("bbb");
        std::fs::create_dir_all(&older).unwrap();
        std::fs::write(older.join("config.json"), "{}").unwrap();
        std::fs::create_dir_all(&newer).unwrap();
        std::fs::write(newer.join("config.json"), "{}").unwrap();

        let roots = vec![tmp.path().to_path_buf()];
        let found = find_local_model("acme/mini", &roots).expect("cached snapshot found");
        assert_eq!(
            std::path::Path::new(&found),
            newer,
            "the newest snapshot that holds a config wins"
        );
    }

    #[test]
    fn test_find_local_model_resolves_unpacked_store() {
        let tmp = tempfile::tempdir().unwrap();
        let unpacked = tmp.path().join("acme").join("mini");
        std::fs::create_dir_all(&unpacked).unwrap();
        std::fs::write(unpacked.join("config.json"), "{}").unwrap();

        let roots = vec![tmp.path().to_path_buf()];
        assert_eq!(
            find_local_model("acme/mini", &roots),
            Some(unpacked.to_string_lossy().into_owned())
        );
    }

    #[test]
    fn test_find_local_model_passes_through_what_it_cannot_resolve() {
        let tmp = tempfile::tempdir().unwrap();
        let roots = vec![tmp.path().to_path_buf()];
        assert_eq!(find_local_model("acme/absent", &roots), None);
        assert_eq!(find_local_model("bare-name-without-owner", &roots), None);
        // An existing directory is taken as-is.
        let dir = tmp.path().to_string_lossy().into_owned();
        assert_eq!(find_local_model(&dir, &roots), Some(dir));
    }
}

