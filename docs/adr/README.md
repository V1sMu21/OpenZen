# Architecture Decision Records

This directory contains Architecture Decision Records (ADRs) for the OpenZen Rust rewrite.

An ADR is a short document capturing an important architectural decision made during the project,
along with its context and consequences.

## Format

Each ADR follows the [Michael Nygard format](https://cognitect.com/blog/2011/11/15/documenting-architecture-decisions):

- **Title**: The decision
- **Status**: Proposed, Accepted, Deprecated, or Superseded
- **Context**: Why this decision was needed
- **Decision**: What was decided
- **Consequences**: What becomes easier or harder

## ADR Index

| # | Title | Status |
|---|-------|--------|
| 0001 | Pure Rust rewrite (no Python FFI) | Accepted |
| 0002 | Axum as web framework | Accepted |
| 0003 | WASM as plugin runtime | Accepted |
| 0004 | TOML for configuration files | Accepted |
| 0005 | CDP + lol_html for HTML simplification | Accepted |
| 0006 | Broadcast channel for SSE event bus | Accepted |
| 0007 | Pointer-based string ABI for WASM plugins | Accepted |
| 0008 | Remove narration and content heuristic filters | Accepted |
| 0009 | RAG system library selection (fastembed vs rig-core) | Proposed |
| 0010 | ERME memory engine integration (direct crate + config switch) | Proposed |
