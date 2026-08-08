# Changelog

## v0.2.0 (2026-06-12)

### Protocol — Phase 0 ✅
- **Stream protocol migration**: All LLM parsers emit typed `TextStart/Delta/End`, `ReasoningStart/Delta/End`, `ToolInputStart/Delta/Available`, `ToolOutputAvailable` events directly (no `ProtocolAdapter` double-send).
- **Removed**: `narration.rs` (375 lines heuristic), `content.rs` (559 lines heuristic), `protocol_adapter.rs` (compatibility layer).
- **Deleted**: 6 legacy `StreamEvent` variants (`Token`, `Thinking`, `ToolCall`, `ToolCallReady`, `ToolResult`, `Done`).
- All three UIs (TUI, WebUI, Tauri) consume only `protocol_v1` events.

### TUI — Phase 1 🔶
- **History**: LRU module (1000 entries, dedup, Ctrl+↑↓ search, `/history` command). `reedline` integration was evaluated but blocked by sync API incompatibility with TUI's async event loop.
- **Theme**: Configurable `Theme` struct with `Theme::light()` / `Theme::dark()`, `/theme light|dark` command, `[tui.theme_overrides]` in `mykey.toml`.
- **PromptTemplate**: `{model}`, `{session}`, `{agent}`, `{consume_tokens}`, `{?session}`, `{!session}` variable support for left/right prompts.
- **Deferred**: Phase 1.5 Markdown incremental rendering (748-line markdown.rs refactor, independent PR).
- **Pending**: Phase 1.2 full PromptTemplate config, 1.3 RAG system.

### WebUI — Phase 2 ✅
- **Auth**: Bearer token middleware (auto-generated at startup), SSE/health exempt, frontend `fetchJson()` wrapper with 401 → prompt retry, localStorage persistence.
- **Branching**: Regenerate button on assistant messages, `POST /api/sessions/:id/regenerate` endpoint, `MessageTreeNav` component for multi-branch navigation.
- **Theme switching**: Dark/light/system toggle (`ThemeSwitcher.svelte`), CSS variables in `.theme-light` class, localStorage persistence.
- **Transient data**: `TransientsBar.svelte` — auto-dismissing notification bar for `data_search_stage`, `data_token_meter`, `data_memory_retrieval`, `data_compressing_context` events.
- **Deferred**: Phase 2.2 File attachment (requires multipart upload endpoint + multimodal model support).
- **Config**: `[server] auth_token` in `mykey.toml`, `openzen serve --auth-token <manual>`.

### Agent — Phase 3 ✅
- **New crate**: `ga-agent` — `AgentConfig { model, temperature, instructions, use_tools, documents, variables }`, YAML config loading from `~/.openzen/agents/<name>/config.yaml`.
- **CLI**: `ga agent <name>` starts agent with injected instructions, `ga agent --list` shows all available agents.
- **WebUI**: `AgentPicker.svelte` modal, `GET /api/agents` endpoint.
- **TUI**: `/agent <name>` command switches agent and injects system prompt, `/agent` lists all.

### Desktop — Phase 4 ✅
- **System tray**: Tray icon with Open/Quit menu, left-click restores window.
- **Notifications**: Desktop notification on agent completion with 100-character response preview.
- **Multi-window**: `open_session_window(session_id)` command, each session gets independent window.

### Ecosystem — Phase 5 ✅
- **Tool Discovery**: `linkme` distributed slice (`TOOL_FACTORIES`) auto-registers all tools at link time. `build_default()` calls `build_auto()` first, falls back to `build_manual()`.
- **Smart Router**: `[router]` config section with `route_rules: [{pattern, model}]`, complexity-based fallback chain.
- **Session LRU**: `SessionStore::with_max(n)`, `set_max_sessions()`, oldest sessions archived to `sessions_archive/`.
- **ModelSwitcher**: Enhanced context window display (K/M formatting).

### Engineering
- Zero-warning workspace compilation (except pre-existing Tailwind CSS LSP diagnostic).
- ~400 tests pass (5 pre-existing tool-count test failures persist, unrelated to v0.2.0 changes).
- 12MB release binary.
