# Changelog

## v0.1.0 (2026-08-09)

> 首次公开发布。包含从纯 Rust 重写至今的全部能力：流协议 v1、三端 UI、Agent 工作模式、桌面生态、ERME 语义记忆引擎与 L0 灵魂层。

### Core Engine — Protocol & Agent Loop
- **Stream protocol migration**: All LLM parsers emit typed `TextStart/Delta/End`, `ReasoningStart/Delta/End`, `ToolInputStart/Delta/Available`, `ToolOutputAvailable` events directly (no `ProtocolAdapter` double-send).
- **Removed**: `narration.rs` (375 lines heuristic), `content.rs` (559 lines heuristic), `protocol_adapter.rs` (compatibility layer). 6 legacy `StreamEvent` variants deleted (`Token`, `Thinking`, `ToolCall`, `ToolCallReady`, `ToolResult`, `Done`).
- **Session persistence**: Tauri backend now calls `store.save()` after message writes (previously memory-only) — history survives restart.
- **Context token accounting**: `total_input_tokens` now accumulates (was overwrite); frontend context bar consumes the last message's `contextTokens` instead of summing all messages (no more double-counting).
- **Auto-compression system prompt protection**: `compress_messages` skips System-role messages, deleting from the first non-System message — auto-compact no longer wipes the system prompt (was causing `llm_error`).
- **/compact 0 chars**: deserialization aligned with the flat on-disk JSON format (manual `Message` construction from JSON fields instead of `serde_json::from_value`).

### Agent Harness — U1-U6 (Codex-inspired)
- **U1 Explicit planning phase**: `plan_from_todos()` populates `CheckpointPlan` (was dead `default()`); planning philosophy added to sys_prompt.
- **U2 MCP call_tool**: persistent stdio connection + `send_request/read_response`, `McpManager::call_tool`, `McpToolHandler` bridge — tools now callable, not just discoverable.
- **U3 Memory job scheduler**: `MemoryJobScheduler` (lease / retry / crash takeover) + background worker + `McpMemoryDistiller` — async two-stage session distillation, crash-recoverable.
- **U4 Session rollout replay**: `rollout.rs` (RolloutRecorder/read_rollout), recorded at turn boundaries — deterministic replay.
- **U5 Git snapshot**: `git_snapshot()` + LoopCheckpoint `git_sha/branch/origin` fields (serde-default compatible with old checkpoints).
- **U6 Prompt discipline**: sys_prompt gained 5 sections (planning / verification / efficiency / scope / closure philosophy).
- **New crate**: `ga-agent` — `AgentConfig { model, temperature, instructions, use_tools, documents, variables }`, YAML config loading from `~/.openzen/agents/<name>/config.yaml`.

### ERME Memory Engine — M1-M4 + M7 (L0 soul layer)
- **M1 Dependency**: ERME vendored to `vendor/entropy-memory-engine/` (53 .rs files; +`distill_and_store` method, WAL clippy fix) — path dependency so GitHub CI resolves outside the repo.
- **M2 Long-lived instance**: `AppState.erme_store` + `init_erme_store()` (L1 cache + L2 HNSW + L3 WAL + Orchestrator, `align_on_write=true`), created once at Tauri setup.
- **M3 Read path**: `memory_backend = "file" | "erme"` config switch (`MyKeyConfig`, default `"file"`); ERME mode uses `recall_by_text(query, k=5)` semantic retrieval instead of full-file injection.
- **M4 Write path**: crystallization hook additionally distills the session into ERME (`ConsolidationEngine`, automatic conflict resolution), wrapped in `spawn_blocking`, failures only warn.
- **M7 L0 soul layer**: `ErmeRuntime` — Portrait / LifeNarrative / ReflectionEngine / PromptInjector; every turn's system prefix carries identity/narrative/portrait state.
- **M7 Inner loop**: `MemoryOrchestrator::with_idle_cycle(RamblingEngine)` — idle rambling → QuarantineManager validation → RealityAnchor anchoring → ReflectionEngine review; frequency-controlled, non-blocking.
- **Rollback**: set `memory_backend = "file"` (default); data at `{working_dir}/memory_erme/` alongside legacy `memory/`, old memory untouched.

### TUI
- **History**: LRU module (1000 entries, dedup, Ctrl+↑↓ search, `/history` command). `reedline` integration evaluated but blocked by sync API incompatibility with TUI's async event loop.
- **Theme**: Configurable `Theme` struct with `Theme::light()` / `Theme::dark()`, `/theme light|dark` command, `[tui.theme_overrides]` in `mykey.toml`.
- **PromptTemplate**: `{model}`, `{session}`, `{agent}`, `{consume_tokens}`, `{?session}`, `{!session}` variable support for left/right prompts.
- **Agent**: `/agent <name>` command switches agent and injects system prompt, `/agent` lists all.

### WebUI
- **Auth**: Bearer token middleware (auto-generated at startup), SSE/health exempt, frontend `fetchJson()` wrapper with 401 → prompt retry, localStorage persistence.
- **Branching**: Regenerate button on assistant messages, `POST /api/sessions/:id/regenerate` endpoint, `MessageTreeNav` component for multi-branch navigation.
- **Theme switching**: Dark/light/system toggle (`ThemeSwitcher.svelte`), CSS variables in `.theme-light` class, localStorage persistence.
- **Transient data**: `TransientsBar.svelte` — auto-dismissing notification bar for `data_search_stage`, `data_token_meter`, `data_memory_retrieval`, `data_compressing_context` events.
- **AgentPicker**: modal + `GET /api/agents` endpoint. **Config**: `[server] auth_token` in `mykey.toml`, `openzen serve --auth-token <manual>`.

### Desktop (Tauri)
- **System tray**: Tray icon with Open/Quit menu, left-click restores window.
- **Notifications**: Desktop notification on agent completion with 100-character response preview.
- **Multi-window**: `open_session_window(session_id)` command, each session gets independent window.
- **App icon**: transparent Song-Celadon cat-head icon (F3, krea2-turbo txt2img + BiRefNet matting + InvertMask) → `cargo tauri icon` all platforms → Dock pixel-verified.

### UI — Phase 7 + Round 8
- **Design language**: `docs/ui-design-language.md` (Song Celadon 宋韵天青) — tokens / glaze / glyphs / motion / component complexity contracts.
- **Todo rail pinned top-right**: scrolling moved to `.messages-scroll` (`flex:1 + min-height:0 + overflow-y:auto`), `.todo-rail` (320px, sticky top:0) becomes its direct child.
- **Cursor spacing**: `.streaming-zone` `min-height:1.5em` + `padding:8px` → `min-height:0` + `padding:2px 0` (final message no longer floats far above the cursor).
- **Final reply not rendering**: respond tool never emits `text_start/text_delta` (pure-text fast path) — `finalizeAssistantMessage` now injects `full_response` as a text part when no identical part exists (matches disk-path behavior).

### Ecosystem
- **Tool Discovery**: `linkme` distributed slice (`TOOL_FACTORIES`) auto-registers all tools at link time. `build_default()` calls `build_auto()` first, falls back to `build_manual()`.
- **Smart Router**: `[router]` config section with `route_rules: [{pattern, model}]`, complexity-based fallback chain.
- **Session LRU**: `SessionStore::with_max(n)`, `set_max_sessions()`, oldest sessions archived to `sessions_archive/`.
- **ModelSwitcher**: Enhanced context window display (K/M formatting).
- **agents-a1-8bit**: added to `config/mykey.toml` (256K context); Tauri `list_models` / `send_message` accept `model_name` override; ModelSwitcher UI support.

### Packaging & Engineering
- **Release build**: fixed `tauri.conf.json` `beforeBuildCommand` (runs inside `frontendDist`; was `cd ../frontends`) → `cargo tauri build` produces OpenZen.app (23MB arm64) + dmg (13MB).
- **Repo hygiene**: git initialized 2026-08-08; removed hardcoded oMLX API key, session records, dev artifacts, and personal files from history.
- **Verification**: vendor 274 tests / oz-core 127 tests / workspace zero errors (~400 tests total); release launch logs `ERME memory engine initialised` with 0 panics.
- 12MB release binary (pre-ERME baseline; see ERME section for current sizing).
