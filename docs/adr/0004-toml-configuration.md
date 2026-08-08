# ADR-0004: TOML for configuration files

## Status

Accepted

## Context

The Python original used `mykey.py` — a Python file evaluated at runtime to define LLM
session configurations. The Rust rewrite needs a static configuration format.

Options: **TOML**, **YAML**, **JSON**, **JSON5**, **RON**.

Requirements:
- Human-readable and hand-editable (configuration is checked into version control)
- Support for comments (to document API key purposes, model notes)
- Nested structure support (sessions have api keys, models, parameters)
- Serde native deserialization (avoid custom parsers)
- Environment variable override support for secrets (API keys)

## Decision

We use **TOML** for all static configuration files.

Config files are placed in `config/` directory with the format `config/*.toml`.

The primary config file `config/mykey.toml` uses `serde(flatten)` to support
arbitrary named session entries. Session type is inferred from the key name:
- Key containing `"native_claude"` → `NativeClaudeSession`
- Key containing `"native_oai"` → `NativeOAISession`
- Key containing `"claude"` → `ClaudeSession` (proxy mode)
- Key containing `"oai"` → `OaiSession` (proxy mode)
- Key containing `"mixin"` → `MixinSession` (fallback chain)

Environment variables override any TOML field via `GA_` prefix convention
(e.g., `GA_NATIVE_CLAUDE_APIKEY` overrides the `apikey` field).

## Consequences

**Positive**:
- Comments are supported natively — critical for documenting API key sources, rate limits
- Serde's `#[derive(Deserialize)]` provides compile-time validation of config structure
- `serde(flatten)` enables flexible session naming without predefined keys
- Flattened key naming convention (session type inferred from name) eliminates the need
  for a manual `type` field
- TOML's table structure maps naturally to `HashMap<String, SessionConfig>`

**Negative**:
- TOML lacks multi-line inline arrays (must use `[[array]]` table syntax)
- Cannot represent deeply nested JSON structures inline (tool schemas remain in JSON)
- YAML has broader tooling support (e.g., `yq` for querying) — TOML's tooling is less mature
- Environment variable override requires explicit `serde(deserialize_with)` for each field
