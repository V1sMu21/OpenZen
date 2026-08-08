# ADR-0003: WASM as plugin runtime

## Status

Accepted

## Context

The agent loop needs an extension mechanism for third-party tools. Options considered:

- **WASM (wasmtime)**: Cross-platform sandboxed runtime. Plugins compile once, run anywhere.
- **dlopen/liblolading**: Native shared libraries, maximum performance, OS-specific ABI.
- **Lua/rhai scripting**: Embedded scripting, easy to write, limited ecosystem.
- **Unix processes (stdin/stdout JSON)**: Simple IPC, heavy per-call overhead.

Requirements:
- Safety: plugins must not crash the agent process
- Performance: tool call latency should be < 10ms overhead vs native
- Portability: same plugin binary works on Linux, macOS, Windows
- ABI stability: plugin interface should not break on Rust compiler upgrade
- Ecosystem: should support the widest range of implementation languages

## Decision

We use **wasmtime** as the plugin runtime for first-class WASM support, with dlopen as a
documented future alternative.

WASM plugins communicate via a pointer-based ABI over linear memory:
- Export `memory` (minimum 1 page / 64 KiB)
- Export `tool_name() -> i32`, `tool_description() -> i32`, `tool_parameters() -> i32`
- Export `execute(args_ptr: i32, args_len: i32) -> i32`
- Strings in linear memory are null-terminated UTF-8

## Consequences

**Positive**:
- True cross-platform: a .wasm compiled on any OS runs on any OS
- Memory safety: wasmtime provides sandboxed execution; a plugin cannot access host memory
  or filesystem without explicit host function imports
- Multiple language support: plugins can be written in Rust (wasm32-wasi), C/C++
  (via clang/emscripten), Go (via tinygo), or any WASM-targeting language
- No Rust compiler ABI coupling: WASM interface is stable across Rust versions

**Negative**:
- Increased binary size: wasmtime adds ~3MB to the release binary (12MB total, still
  under the 15MB target)
- Compile-time overhead: wasmtime's cranelift JIT adds ~2 minutes to release builds
- WASM has no direct access to host OS (filesystem, network) — the host must explicitly
  export any needed capabilities
- The pointer-based ABI requires manual linear memory management in both host and guest;
  this is error-prone compared to a higher-level IDL (like WIT)
