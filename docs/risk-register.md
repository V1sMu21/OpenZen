# Risk Register

This document tracks identified risks for the OpenZen Rust rewrite project.
Each risk is assigned a unique ID, assessed for probability and impact, and given
a mitigation plan.

## Severity Matrix

| Probability | Insignificant (1) | Minor (2) | Moderate (3) | Major (4) | Critical (5) |
|-------------|-------------------|-----------|-------------|-----------|--------------|
| Almost Certain (5) | 5 | 10 | 15 | **20** | **25** |
| Likely (4) | 4 | 8 | 12 | **16** | **20** |
| Possible (3) | 3 | 6 | 9 | 12 | 15 |
| Unlikely (2) | 2 | 4 | 6 | 8 | 10 |
| Rare (1) | 1 | 2 | 3 | 4 | 5 |

- **1–4**: Low (accept)
- **5–9**: Medium (monitor)
- **10–15**: High (active mitigation)
- **16–25**: Critical (immediate action required)

## Risk Register

### R-001: SSE stream parsing edge cases across LLM providers

| Field | Value |
|-------|-------|
| Category | Technical |
| Probability | 3 (Possible) |
| Impact | 4 (Major) |
| Severity | 12 (High) |
| Status | Mitigated |
| Owner | Core team |

**Description**: Each LLM provider (Anthropic Claude, OpenAI, MiniMax, Kimi) uses slightly
different SSE event formats. Parsing errors in edge cases (incomplete chunks, multi-line
data, unexpected event types) can cause silent message loss or agent loop crashes.

**Mitigation**: Provider-specific test fixtures with recorded SSE payloads.
`ClaudeStream`, `OaiStream` each have dedicated parsing modules with unit tests.
Retry logic (`retry_with_backoff`) catches transient parse failures.

**Contingency**: Fall back to non-streaming mode if SSE parsing fails repeatedly.
Log raw SSE bytes at `debug` level for forensic analysis.

---

### R-002: LLM API format drift

| Field | Value |
|-------|-------|
| Category | External |
| Probability | 3 (Possible) |
| Impact | 3 (Moderate) |
| Severity | 9 (Medium) |
| Status | Open |
| Owner | Core team |

**Description**: Anthropic and OpenAI frequently update their Messages API — new content
block types, changed response shapes, deprecated fields. A breaking API change could
disrupt the agent loop until the client code is updated.

**Mitigation**: `NativeToolClient` abstraction layer isolates API-specific formatting.
`Session` trait provides a uniform interface across backends. Version-aware dispatch
(via `api_mode` field in config) allows per-provider behavior.

**Contingency**: Pin known-good API versions in config. Add integration tests that run
against live API endpoints weekly.

---

### R-003: CDP protocol version incompatibility

| Field | Value |
|-------|-------|
| Category | Technical |
| Probability | 3 (Possible) |
| Impact | 4 (Major) |
| Severity | 12 (High) |
| Status | Open |
| Owner | Browser integration team |

**Description**: Chrome DevTools Protocol versions differ across Chrome releases.
Commands available in one version may be missing or changed in another. The `web_scan`
tool depends on specific CDP methods (`DOM.getDocument`, `Runtime.evaluate`).

**Mitigation**: Abstract CDP communication layer behind a `CdpClient` trait with version
negotiation. Minimize dependency on specific CDP commands — prefer stable, long-standing
methods.

**Contingency**: Detect CDP version at connect time and warn on incompatibility.
Document required Chrome version range.

---

### R-004: Python subprocess cross-platform behavior differences

| Field | Value |
|-------|-------|
| Category | Technical |
| Probability | 2 (Unlikely) |
| Impact | 3 (Moderate) |
| Severity | 6 (Medium) |
| Status | Mitigated |
| Owner | Core team |

**Description**: The `code_run` tool executes Python code via `std::process::Command`.
Path resolution, signal handling (SIGTERM vs taskkill), and temp file cleanup differ
between Linux, macOS, and Windows.

**Mitigation**: Use `which` crate to locate Python interpreter. Use `tempfile` crate for
cross-platform temp directory management. Use `tokio::time::timeout` for consistent
timeout behavior. Document tested platforms.

**Contingency**: Add Windows CI runner. Maintain platform-specific code paths only where
necessary (process tree kill, path normalization).

---

### R-005: HTML simplification output divergence from Python version

| Field | Value |
|-------|-------|
| Category | Technical |
| Probability | 3 (Possible) |
| Impact | 2 (Minor) |
| Severity | 6 (Medium) |
| Status | Open |
| Owner | Browser integration team |

**Description**: The Rust `lol_html`-based simplifier produces different output than the
Python `simphtml.js` version on the same input. This causes behavioral differences in
the `web_scan` tool output.

**Mitigation**: Snapshot tests comparing Rust and Python simplifier output for a curated
set of test pages. Migrate simplifier rules incrementally, verifying equivalence per rule.

**Contingency**: If exact equivalence is not achievable, document known differences and
their impact on LLM context quality. Accept minor divergence if Rust output is
functionally equivalent for token extraction.

---

### R-006: TOML configuration expressiveness limits

| Field | Value |
|-------|-------|
| Category | Technical |
| Probability | 2 (Unlikely) |
| Impact | 2 (Minor) |
| Severity | 4 (Low) |
| Status | Mitigated |
| Owner | Core team |

**Description**: TOML cannot express deeply nested inline structures or multi-line inline
arrays. Complex tool schemas or large API configurations may be awkward to represent.

**Mitigation**: Tool schemas remain in JSON (`assets/tools_schema.json`), compiled into
the binary via `include!()`. Configuration supports environment variable overrides
(`GA_*` prefix) for values that need dynamic substitution.

**Contingency**: Add JSON5 config file support for advanced use cases. Keep TOML for
95% of configuration scenarios.

---

### R-007: WASM plugin ABI stability

| Field | Value |
|-------|-------|
| Category | Technical |
| Probability | 2 (Unlikely) |
| Impact | 2 (Minor) |
| Severity | 4 (Low) |
| Status | Mitigated |
| Owner | Core team |

**Description**: The manual pointer-based WASM ABI (linear memory with fixed scratch
offsets) may need revision as plugin usage grows. Breaking ABI changes would require
updating all existing plugins.

**Mitigation**: Plugin system is internal-only until v0.2.0. ABI is documented in
ADR-0007 and versioned alongside the binary. Built-in tools use native `ToolHandler`
trait, not WASM — the plugin path is purely additive.

**Contingency**: Add a compatibility shim layer when the ABI changes. Use WASM component
model (WIT) in a future version when toolchain support matures.

---

### R-008: tokio async + subprocess timeout race conditions

| Field | Value |
|-------|-------|
| Category | Technical |
| Probability | 3 (Possible) |
| Impact | 3 (Moderate) |
| Severity | 9 (Medium) |
| Status | Mitigated |
| Owner | Core team |

**Description**: Wrapping `std::process::Command::output()` with `tokio::time::timeout`
can leave orphaned subprocesses if the timeout fires before the child exits. The child
continues running, consuming resources and potentially producing stale output.

**Mitigation**: Use `Command::kill_on_drop(true)` combined with explicit `child.kill()`
in the timeout path. Wrap the entire subprocess lifecycle in a structured `ChildGuard`
RAII type that ensures cleanup on drop.

**Contingency**: Log orphaned PID for manual cleanup. Add a `ga cleanup` command that
kills leftover agent subprocesses.

---

### R-009: SSE broadcast channel overflow with slow consumers

| Field | Value |
|-------|-------|
| Category | Technical |
| Probability | 3 (Possible) |
| Impact | 2 (Minor) |
| Severity | 6 (Medium) |
| Status | Mitigated |
| Owner | WebUI team |

**Description**: `tokio::sync::broadcast` has a fixed channel capacity (256 events).
Slow SSE consumers that fall behind cause a `Lagged` error, disconnecting them.
During fast token generation, burst events could overflow the channel for all subscribers.

**Mitigation**: Channel capacity is set to 256, which accommodates typical agent sessions
(< 100 tool calls). SSE clients reconnect automatically on disconnect (EventSource spec).
Events are debounced per-tick rather than per-token to reduce burst volume.

**Contingency**: Make channel capacity configurable via environment variable.
Monitor `Lagged` error rate in production. Consider per-subscriber buffering with
`tokio::sync::mpsc` as an alternative for high-latency connections.

---

### R-010: wasmtime version API changes

| Field | Value |
|-------|-------|
| Category | External |
| Probability | 4 (Likely) |
| Impact | 3 (Moderate) |
| Severity | 12 (High) |
| Status | Open |
| Owner | Core team |

**Description**: wasmtime has a history of breaking API changes between major versions
(e.g., `Memory::read` changed from single-byte to buffer-based API between versions 14
and 44). These changes require code updates on every upgrade.

**Mitigation**: Pin wasmtime to a known-good major version. Abstract WASM operations
behind a thin `WasmRuntime` wrapper trait. Test plugin loading in CI with the pinned
version. Document upgrade path for each wasmtime release.

**Contingency**: Consider lighter WASM runtime (`wasmi` interpreter) if wasmtime churn
becomes unmanageable. This trades JIT performance for API stability.

---

### R-011: Release binary size exceeds 15MB target

| Field | Value |
|-------|-------|
| Category | Technical |
| Probability | 1 (Rare) |
| Impact | 2 (Minor) |
| Severity | 2 (Low) |
| Status | Closed |
| Owner | Core team |

**Description**: Adding dependencies (especially wasmtime with cranelift JIT) could push
the release binary past the 15MB target, impacting deployment speed and storage.

**Mitigation**: LTO, `codegen-units=1`, `strip=symbols`, `panic=abort` in release profile.
Current binary is 12MB (measured at Phase 5 completion), well under the 15MB target.

**Contingency**: Enable `opt-level = "z"` for size optimization. Split wasmtime into a
separate dynamic feature that can be disabled for size-constrained deployments.

---

### R-012: cranelift JIT compilation time in CI

| Field | Value |
|-------|-------|
| Category | Process |
| Probability | 4 (Likely) |
| Impact | 2 (Minor) |
| Severity | 8 (Medium) |
| Status | Open |
| Owner | Core team |

**Description**: wasmtime's cranelift backend adds approximately 2 minutes to release
builds. This slows CI pipeline feedback and developer iteration on release builds.

**Mitigation**: Development builds (`cargo build`) complete in seconds — cranelift
only affects `--release`. CI runs `cargo check` for fast feedback and only does
release builds for tagging/publishing. Separate release build step in CI with timeout.

**Contingency**: Cache `target/` directory in CI. Use `sccache` for incremental
compilation. Evaluate wasmtime's "pulley" interpreter for debug builds.

---

### R-013: No Windows CI coverage

| Field | Value |
|-------|-------|
| Category | Process |
| Probability | 3 (Possible) |
| Impact | 3 (Moderate) |
| Severity | 9 (Medium) |
| Status | Open |
| Owner | Core team |

**Description**: The project is developed and tested on macOS only. Windows-specific
issues (path separators, signal handling, Python discovery) may go undetected until
a Windows user encounters them.

**Mitigation**: Use cross-platform abstractions (`which`, `tempfile`, `std::path::PathBuf`).
Avoid platform-specific APIs (no Unix-only `signal::unix` in shared code). Document
assumed platform capabilities.

**Contingency**: Add GitHub Actions Windows runner. Use conditional compilation
(`#[cfg(target_os = "windows")]`) for platform-specific code paths.

---

## Risk Status Summary

| Status | Count | Risks |
|--------|-------|-------|
| Open | 6 | R-002, R-003, R-005, R-010, R-012, R-013 |
| Mitigated | 6 | R-001, R-004, R-006, R-007, R-008, R-009 |
| Closed | 1 | R-011 |
| **Total** | **13** | |

## Top 5 Risks by Severity

| Rank | ID | Risk | Severity |
|------|----|------|----------|
| 1 | R-001 | SSE stream parsing edge cases | 12 (High) |
| 2 | R-003 | CDP protocol version incompatibility | 12 (High) |
| 3 | R-010 | wasmtime version API changes | 12 (High) |
| 4 | R-002 | LLM API format drift | 9 (Medium) |
| 5 | R-008 | Subprocess timeout race conditions | 9 (Medium) |
