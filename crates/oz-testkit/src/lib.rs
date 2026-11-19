//! OpenZen Edge Case Factory (dev-only).
//!
//! Pipeline: contract → generators → case IR → runner → report.
//! Nothing here is linked into a release binary; the crate is a workspace
//! member so `cargo test --workspace` and `cargo clippy` see it.

pub mod aliases;
pub mod case;
pub mod contract;
pub mod gen_bytes;
pub mod gen_params;
pub mod gen_security;
pub mod props;
pub mod report;
pub mod runner;
pub mod spec;

use std::path::{Path, PathBuf};

pub fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()
        .unwrap_or_else(|_| PathBuf::from("."))
}

pub fn cases_dir() -> PathBuf {
    repo_root().join("cases")
}

pub fn corpus_dir() -> PathBuf {
    repo_root().join("corpus")
}

/// Load every `*.jsonl` under `cases/unit`, sorted by path for determinism.
pub fn load_unit_cases() -> Result<Vec<case::Case>, String> {
    let dir = cases_dir().join("unit");
    let mut paths: Vec<PathBuf> = std::fs::read_dir(&dir)
        .map_err(|e| format!("{}: {e}", dir.display()))?
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.extension().and_then(|x| x.to_str()) == Some("jsonl"))
        .collect();
    paths.sort();
    let mut all = Vec::new();
    for p in paths {
        all.extend(case::read_jsonl(&p)?);
    }
    Ok(all)
}

pub fn load_spec_cases() -> Result<Vec<case::Case>, String> {
    let dir = cases_dir().join("spec");
    if !dir.exists() {
        return Ok(Vec::new());
    }
    let mut paths: Vec<PathBuf> = std::fs::read_dir(&dir)
        .map_err(|e| format!("{}: {e}", dir.display()))?
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.extension().and_then(|x| x.to_str()) == Some("json"))
        .collect();
    paths.sort();
    let mut all = Vec::new();
    for p in paths {
        let text = std::fs::read_to_string(&p).map_err(|e| format!("{}: {e}", p.display()))?;
        let c: case::Case =
            serde_json::from_str(&text).map_err(|e| format!("{}: {e}", p.display()))?;
        all.push(c);
    }
    Ok(all)
}

pub fn load_wire() -> Result<Vec<gen_bytes::WireSeed>, String> {
    let path = corpus_dir().join("wire/seeds.jsonl");
    let text = std::fs::read_to_string(&path).map_err(|e| format!("{}: {e}", path.display()))?;
    let mut out = Vec::new();
    for (i, line) in text.lines().enumerate() {
        if line.trim().is_empty() {
            continue;
        }
        out.push(
            serde_json::from_str(line).map_err(|e| format!("{}:{}: {e}", path.display(), i + 1))?,
        );
    }
    Ok(out)
}
