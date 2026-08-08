# ADR-0010: ERME memory engine integration (direct crate + config switch)

## Status

Proposed

## Context

OpenZen currently has two parallel file-based memory systems:

- `oz_memory::MemorySystem` — `{working_dir}/memory/` (L1 insight, L2 global facts,
  L3 SOPs, L4 raw sessions), read via `get_global_memory()` and injected in full
  into the system prompt at 4 entry points (runner.rs, main.rs, bridge.rs, webui).
- `SkillMcpStore` — `{working_dir}/.skill_mcp/`, the write target of the
  LLM-driven `Crystallizer` at `agent_loop.rs:2092`.

Both are full-file reads with string matching — no semantic retrieval, no conflict
handling, no forgetting, no self-model. A separate repo,
`~/Documents/opencode/Entropy-Reduced Memory Engine` (ERME, 221 tests passing),
implements L0 (soul/reflection), L1 (DashMap cache), L2 (HNSW semantic search),
L3 (WAL persistence + budget), conflict resolution, quarantine, and rambling.
MLX is optional with pure-Rust fallbacks.

Four integration options were evaluated:

| Option | Efficiency | Runtime complexity | Memory growth |
|--------|-----------|--------------------|----------------|
| A: direct crate dependency | best (no serialization) | low | full (L0-L3 accessible) |
| B: MemoryBackend trait (hot-swap) | best | medium (trait abstraction across all callers) | full |
| C: ERME as MCP server | worst (IPC + agent-triggered) | medium-high (process lifecycle) | partial (no L0 inner loop) |
| D: progressive sidecar (dual injection) | good but token-heavy | lowest | full |

## Decision

Integrate ERME as a **direct path dependency** (option A's efficiency), with a
**single `memory_backend` config switch** branching at the two integration points
(option B's switchability without the full trait abstraction), staged as a
progressive rollout (option D's low risk). Not an MCP server.

- Read path: system prompt construction branches on `memory_backend`; ERME mode
  uses `recall_by_text(query, k=5)` semantic retrieval instead of full-file injection.
- Write path: the crystallization hook at `agent_loop.rs:2092` additionally
  distills the session into ERME (facts via `ConsolidationEngine`, stored with
  automatic conflict resolution).
- ERME instance is created once at Tauri setup, held in AppState
  (L2 HNSW index and L3 storage are long-lived; must not be rebuilt per run).
- Data lives at `{working_dir}/memory_erme/`, alongside the existing `memory/`
  directory, so rollback leaves the old memory untouched.
- ERME's sync API is wrapped in `tokio::task::spawn_blocking`.
- Rollback: set `memory_backend = "file"` (the default).

## Consequences

**Positive**:

- Zero serialization/IPC overhead; memory recall is nanosecond/microsecond level.
- Semantic recall injects only top-k relevant memories — less context, fewer
  tokens than full-file injection.
- Full access to L0-L3: soul model (user preference portrait), conflict
  resolution (overturn/sublimate/supplement), quarantine + rambling inner loop
  (autonomous memory growth).
- Every step is independently rollbackable via the config switch.

**Negative**:

- OpenZen workspace gains a path dependency on an external repo; ERME changes
  require coordination across two repos.
- ERME is sync API in an async codebase; needs `spawn_blocking` discipline.
- Memory quality depends on the distill hook being wired correctly.

## References

- ERME repo: `~/Documents/opencode/Entropy-Reduced Memory Engine`
- Prior plan: `ERME_OpenZen_Integration_Plan.md` (2026-06-29, outdated assumptions:
  `LoopConfig.memory_backend` did not exist then and still does not)
