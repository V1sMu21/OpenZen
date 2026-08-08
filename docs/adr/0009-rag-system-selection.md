# ADR-0009: RAG system library selection (fastembed vs rig-core)

## Status

Proposed

## Context

Phase 1.3 of the roadmap calls for a RAG (Retrieval-Augmented Generation) system
with the following requirements:

- Embed documents into vector space for semantic search
- Re-rank results for relevance
- Store vectors persistently
- Support configurable chunk size, overlap, and top-k
- Load documents via external commands (`pdftotext`, etc.)

Two Rust libraries were evaluated:

### fastembed
- **Pros**: Pure Rust implementation, no Python dependency, ONNX runtime, models
  auto-downloaded from HuggingFace. Battle-tested in Qdrant's ecosystem.
- **Cons**: Model downloads (100-500MB) break offline/air-gapped deployments.
  ONNX runtime adds ~8MB to binary size. Limited model selection compared to
  Python alternatives.

### rig-core
- **Pros**: Integrated with the broader `rig` AI framework. Potentially simpler
  API for the RAG + LLM pipeline.
- **Cons**: Less mature than fastembed. Smaller community. May pull in unwanted
  dependencies from the rig ecosystem.

### No vector store (local hash fallback)
- **Pros**: Zero additional dependencies, works fully offline, no model downloads.
- **Cons**: Only exact text matching or simple TF-IDF, no semantic search.

## Decision

**Defer the decision.** The RAG system is not a dependency for any completed
Phase 0-5 features. The current priority is:

1. Complete Phases 2-5 of the WebUI/TUI/Agent/Tauri roadmap (✅ done)
2. Get the Phase 6 cleanup and v0.2.0 release out
3. Revisit RAG in a dedicated PR after v0.2.0

When implementation begins, the recommended approach is:

1. **Start with `local_hash` fallback** — tokenize the document corpus, compute
   TF-IDF or BM25 scores, return top-k matches. This works for codebases and
   markdown docs without any model downloads.
2. **Add `fastembed` as optional feature** — gated behind a Cargo feature flag
   (`rag-fastembed`), enabled by default but opt-out for offline builds.
3. **Evaluate `rig-core`** if the project later adopts the rig framework for
   other purposes.

## Consequences

### Positive
- Avoids adding 8MB to binary size and 500MB model downloads before v0.2.0
- No risk of deployment breakage from failed model downloads
- Local hash fallback is sufficient for the initial use case (code search in
  project docs)

### Negative
- No semantic search in the initial release
- RAG feature gap vs competitors (Hermes Agent has full vector search)
- Will need a separate implementation effort post-v0.2.0

## Configuration

When implemented, the `[rag]` section of `mykey.toml` will look like:

```toml
[rag]
embedder = "local_hash"        # or "fastembed", "rig"
model = "BAAI/bge-small-en-v1.5"  # only for fastembed
chunk_size = 512
chunk_overlap = 64
top_k = 5
rag_template = "Use the following context to answer the question:\\n{context}\\n\\nQuestion: {question}"
```

## Alternatives Considered

1. **Implement now with fastembed**: Rejected — adds deployment complexity before
   v0.2.0, and the feature is not blocking any completed phase.
2. **Use external API (OpenAI embeddings)**: Rejected — violates the project's
   zero-external-dependency philosophy for core functionality. Embedding APIs
   require API keys and internet connectivity.
3. **Skip RAG entirely**: Rejected — Phase 1.3 is in the roadmap and addresses
   a real gap vs competitors. Should be implemented eventually, just not before v0.2.0.
