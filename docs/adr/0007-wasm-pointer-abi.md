# ADR-0007: Pointer-based string ABI for WASM plugins

## Status

Accepted

## Context

WASM plugins need to exchange string data with the host: tool metadata (name, description,
parameters schema) and execution arguments/results. WASM's only shared memory is linear
memory — there is no native string type across the host/guest boundary.

Options considered:
- **WIT (WebAssembly Interface Types)**: Formal IDL with `wasmtime::component`, auto-generates
  bindings. Requires component model support.
- **wit-bindgen**: Generates guest/host bindings from WIT files. Adds build-time code generation.
- **Manual pointer-based ABI**: Export functions returning `i32` pointers into linear memory;
  strings are null-terminated UTF-8. Host reads/writes memory directly.
- **JSON-in/JSON-out**: Single `execute` function taking/returning JSON as a string pointer.
  Simple, universally compatible.

Key constraints:
- Plugin interface must be implementable in any WASM-targeting language without
  language-specific tooling
- Minimal overhead for simple tool invocations
- No build-time code generation (keeps plugin development accessible)
- WASM component model support is not yet universal across toolchains

## Decision

We use a **manual pointer-based ABI** with linear memory as the communication channel.

The ABI is:
```
tool_name() -> i32          ; pointer to null-terminated UTF-8 name
tool_description() -> i32   ; pointer to null-terminated UTF-8 description
tool_parameters() -> i32    ; pointer to null-terminated JSON schema string
execute(args_ptr: i32, args_len: i32) -> i32  ; JSON args in, JSON result out
```

Host-to-guest argument passing uses a fixed scratch offset (65536, start of page 2) in
linear memory:
1. Host serializes JSON args to string, writes to scratch offset, null-terminates
2. Host calls `execute(scratch_offset, arg_len)`
3. Plugin reads args from scratch offset, processes, writes result string to a fixed
   location (e.g., offset 100 from data section)
4. Host reads result string starting at returned pointer

## Consequences

**Positive**:
- No toolchain dependency: a WASM plugin can be written in any language that targets
  WASM (Rust, C, Go, Zig, AssemblyScript)
- No build-time code generation — the ABI is a simple set of exported functions
- ABI is trivial to debug: inspect linear memory at known offsets
- For tools with no args or simple args, overhead is minimal (one function call + memory read)

**Negative**:
- Manual memory management: the plugin must ensure result strings don't overlap with
  input data sections
- Fixed scratch offset (65536) assumes at least 1 page of memory; plugins with tiny
  memory (< 1 page) need adjustment
- No type safety: args and results are JSON strings parsed on both sides — mismatched
  schemas are runtime errors, not compile-time
- Large result data (> 64KB) requires memory growth, which is observable to the plugin
- Not compatible with WASM component model without a wrapper layer
