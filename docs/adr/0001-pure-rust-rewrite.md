# ADR-0001: Pure Rust rewrite (no Python FFI)

## Status

Accepted

## Context

The original OpenAgent (now OpenZen) was written in Python (~3K lines of core code). The rewrite to Rust
faced a fundamental choice: incremental migration via Python FFI (pyo3, rust-cpython) or a
full rewrite with Python kept only as a subprocess dependency.

Factors considered:

- Python's async model (gevent/asyncio) conflicts with tokio's event loop when both run in
  the same process
- FFI boundary introduces complex error handling — exceptions crossing language boundaries
  lose stack traces and type information
- Python C-API/ABI stability across versions (3.10 vs 3.11 vs 3.12) adds maintenance burden
- Python subprocess execution is a feature requirement (for `code_run` tool), not an
  integration requirement
- Team has full understanding of both codebases, making a clean rewrite tractable

## Decision

We commit to a pure Rust rewrite with **zero CPython dependency at build time**.

Python is retained only as an **external subprocess** (`std::process::Command`) for the
`code_run` tool when executing Python code. This is a functional requirement, not an
integration dependency.

The Rust codebase provides its own implementations of all Python original features:
- TOML-based config replaces `mykey.py` dynamic imports
- Rust trait dispatch replaces `BaseHandler.dispatch()` pattern matching
- `tokio::sync::mpsc` replaces Python generator yields for streaming
- Serde compile-time JSON replaces `json.loads` / dynamic schema injection

## Consequences

**Positive**:
- Single static binary with no Python runtime dependency
- Full Rust ecosystem benefits: type safety, zero-cost abstractions, fearless concurrency
- No cross-language debugging; Rust backtraces cover the entire stack
- Simpler CI — only `cargo build`, no Python environment setup
- Performance-critical paths (SSE parsing, HTML simplification) run at native speed

**Negative**:
- Higher upfront rewrite cost — Python's dynamic features (duck typing, runtime imports)
  require careful static design
- Cannot reuse Python packages directly; every dependency must have a Rust equivalent
- Team must be proficient in both Rust async and the original Python logic
