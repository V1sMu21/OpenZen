
## Unreleased

### 🐛 Bug Fixes

- **core:** Honor tool-declared timeouts, cap code_run at 1800s
- **app:** Session-scoped reminder cleanup on task end
- **app:** Stable streaming UI — live bubble, interjections, scroll
- **app:** Live preview for tool cards with streaming args

### 📚 Documentation

- English-primary readme with chinese companion
## v0.1.0 (2026-08-31)

### ◀️ Revert

- **quality:** Drop image-attachment review (model has no vision)

### ⚙️ Miscellaneous Tasks

- Ignore runtime SQLite artifacts in the source tree
- **erme:** Add fork sync manifest, delta patch and script
- Run on macOS to match the real host platform
- **frontend:** Remove unused Google Fonts + dead deps
- **workspace:** Remove orphan oz-platform-qq crate
- **deps:** Remove zero-reference crate dependencies
- **release:** Make release step idempotent (update_release + overwrite)
- **deps:** Disable dependabot version updates
- **release:** Grant GITHUB_TOKEN contents:write for release creation

### ⚡ Performance

- **frontend:** Precompile keyword regexes + cache markdown renders
- **frontend:** O(1) streaming render path
- **frontend:** Incremental derived caches for message list and tokens
- **frontend:** Cache legacy part conversion + O(1) event pairing
- **frontend:** Lazy-load sidepanel artifact renderers
- **frontend+backend:** Paginated sessions + LRU view cache
- **frontend:** Dedupe startup requests + local message counts
- **frontend:** Virtualize long message lists
- **core:** Coalesce per-token deltas in event collectors
- **sessions:** Incremental persistence — O(dirty) instead of O(all) per save
- **erme:** Bound distillation cost, backfill budget, schedule forgetting
- **logs:** Route tracing through the size-rotated log file
- **llm:** Lazy compress snapshot, precompiled parse regexes, linear SSE scan, retried fallback
- **core:** Build run-finalization payloads outside the sessions lock; compact checkpoints
- **frontend:** Bounded LRU markdown cache, drop streamEvents, O(delta) stream joins
- **frontend:** Measured row heights for the virtual list
- **frontend:** Lazy KaTeX and raw-bytes file IPC
- Precompile per-call regexes on hot paths (round3 P1-a/b/c)
- **core,tauri:** Long-lived skill store with mtime-gated reload (round3 P1-d)
- **core:** Zero-clone periodic checkpoints (round3 P1-f)
- **core:** Cached harness-ledger rendering + per-session tool cap (round3 P2-p/P1-j)
- **core:** Smart_format index slicing, tracing sweep, cached harness call (round3 P1-k/P2-l)

### 🎨 Styling

- Clear workspace clippy warnings for a green CI gate
- Apply rustfmt workspace-wide
- Cargo fmt + clippy clean across all changes
- Cargo fmt + clippy -D warnings across round3 changes
- Clippy -D warnings clean for DQ2 (if-let over single-arm match, dedent review body)
- Cargo fmt for QC-2 signature fallout

### 🐛 Bug Fixes

- **tests:** Correct stale checkpoint UUID and ADR index filter
- Harden desktop security and converge lifecycle/lock handling
- Cap IPC inputs, split LLM retry layers, tighten CSP
- **ui:** Correct sidebar toggle arrow directions and localize labels
- **agent:** Dedup final reply text; review sees deliverable images
- **ui:** Side-panel preview for in-workdir files; rename timeline
- **erme:** Review fixes — soul card bootstrap, config parity, ingest idempotency
- **tests:** Skip docs assertions when docs/ is absent
- **ci:** Restore green gate with linux deps, test syntax, size check
- **agent:** Reset llm error count on success + backoff retries
- **agent:** Stream timeout no longer kills slow-but-alive streams
- **agent:** Gate speculative tool execution by safety guard
- **platform:** Supervisor restarts failed or panicked adapters
- **platform:** Recover from poisoned mutexes in AgentBridge
- **feishu:** Add connect and request timeouts to HTTP client
- **platform:** Decouple message receive from agent execution
- **agent:** Queue multiple ask_user calls instead of overwriting
- **agent:** Approval wait races the stop signal
- **wechat:** /stop really stops + persisted message dedup
- **tauri:** Pass handle_request directly instead of redundant closure
- **mcp:** Process-level manager pool + reap children on stop
- **platform:** RAII cleanup survives panics in agent runs
- **scheduler:** Run maintenance tasks with real data paths
- **events:** Cap tool-output payloads in session collectors
- **runner:** RAII memory tick + rotating debug log
- **compress:** Bound summary concurrency and cancel orphans
- **compress:** Compile-safe orphan cancellation for summaries
- **sessions:** Persist worker carries payloads in the channel
- **proc:** Graceful daemon shutdown, terminal reap, chrome guard, IM cap
- **agent:** Move checkpoint/git/verifier IO off the runtime thread
- Sync reaping in McpClient::drop + clippy cleanups
- Rustc 1.97 lint compatibility
- Clippy 1.97 question-mark lint in commands.rs
- Silence -D warnings across all test targets (CI parity)
- **diagnostics:** 60s checker timeout + pin rust toolchain 1.97.1
- **frontend:** Type-safe lazy artifact renderer
- **a11y:** AA dim contrast + aria labels + document lang
- **platform:** Char-safe truncation + panic-proof wechat running guard
- **tools:** Code_run honors timeout, kills children on cancel, cleans temp scripts
- **mcp:** Bound handshake to 10s so a hung stdio server cannot wedge all requests
- **platform:** Poison-recovery lock helper for the last two bare unwrap()s
- **frontend:** Drain stream queue when rAF stalls in hidden windows
- **frontend:** Re-attach MutationObserver when the message list is recreated
- **frontend:** Restore scroll anchor after virtual-window re-render on load-earlier
- **core:** Cap tool results entering the LLM context at 100K chars
- **scheduler:** Session cleanup actually runs — case fix + in-process pruning
- **tauri:** Format consolidation round count, not the Vec
- **daemon:** Initialize new TaskContext field (disk-side cleanup mode)
- **llm:** Sticky failover, native mixin streaming, retry layer convergence
- **core:** Process-wide tool cap, stale ask_user replies, approval leaks, shared HTTP clients
- **frontend:** HTTP heartbeat indicator, WCAG-AA light theme, dialog focus trap
- **frontend:** Type the lazy-math opts, declare ?url asset modules
- **erme:** Keep a query copy for the FTS fallback; branch value not return
- **core:** Round3 batch-1 correctness in the agent loop
- **safety,tools:** Poison-recovery on remaining std Mutex locks
- **skill-mcp:** Snapshot fingerprint AFTER load, not before
- **ask_user:** Key replies by tool_use_id across the whole stack (round3 P1-i)
- **frontend:** SetPendingAskUser takes PendingAskUser (askId passthrough)
- **core:** Breaker counts a speculated tool once, not twice
- **tools:** Pass verified_by=None at remaining crystallise callers
- **pet:** Seal toggle show/hide with focus handoff
- **tauri:** Updater plugin requires plugins.updater config section
- **app:** Settings-panel leftovers — servers.toml comments, title-bar agent name
- **webui:** Soul rename UX + sync, localized birth name, model row overflow
- **webui:** Backend heartbeat diagnosis + startup resilience
- **core:** Compression threshold = 80% of model window, 10-min summary wait
- **skill-mcp:** Silence unused dir variable in reload test
- **tests:** Gate docs assertions on full doc-tree marker

### 💼 Other

- **deps:** Rustls reqwest + explicit tokio features
- **deps:** Teloxide 0.13 -> 0.16
- **deps:** Eliminate native-tls from reqwest/teloxide stack
- **deps:** Unify quick-xml with calamine 0.26
- **release:** Harden tauri bundle + dependabot

### 📚 Documentation

- **design:** Align DESIGN.md with actual Song Celadon tokens
- **readme:** Add bilingual README with bench GIFs and MIT license
- **readme:** Credit EverMind, fix pet corner wording, drop 中国 from colophon line
- **readme:** Bench details — Codex desktop vision rule, local ComfyUI, A-Qing asset source
- **readme:** Rename A-Qing to Ah-Qing, drop fish-cracker reward phrasing

### 🚀 Features

- **erme:** Add entropy-reduced memory engine and integrate
- **ci:** Add git-cliff version derivation and release automation
- **harness:** Complete harness improvement plan P0-P2 and batch-1 borrowings
- **tauri:** Sound-enabled desktop notifications with focus gating
- **quality:** Delivery pipeline with spec-first acceptance and review
- **ui:** Scheduled/heartbeat reminder cards in the right rail
- **quality:** Review scans recent images, not just listed deliverables
- **agent:** Tool-contract rules + submit_plan state machine
- **quality:** Plan approval, in-turn quick verify, failure reflections
- **harness:** Add ToolContext.harness_dir and unify ledger location
- **erme:** Default-enable semantic memory with lazy init
- **ui:** Soul memory status card + get_memory_status command
- **frontend:** Keep previous session visible while loading
- **tauri:** Ordered graceful shutdown on quit
- **erme:** Wire the L0 portrait event loop and user facts into prompts
- **erme:** Rebuild L2 index on startup + CJK-aware keyword extraction
- **erme:** Idle-cycle gating, reality anchor, crystallization on by default
- **quality:** Relevant harness injection, per-project trust decay, adapter health polls
- **core:** Wire MmapWal into the checkpoint pipeline
- **erme:** FTS fallback on the ERME read path
- **platform:** Real adapter health via shared connection state
- **frontend:** Compress-pending notice, poll-on-expand SoulCard, svelte-check zero (round3 P1-g/P1-h/P2-m)
- **frontend:** Wire compress-pending marker + pet toast (companion to 44463e1)
- **erme,tauri:** Embedding visibility, proactive stuck-notice, recall eval (round3 D1-D3)
- **quality:** DQ1 gate completion — auto-spec synthesis, reviewer swap, diff self-check, delivery contract (QA-1/2/3/4)
- **quality:** DQ2 — per-language post-write verify chains + opt-in TDD nudge (QB-1/QB-2)
- **skill-mcp:** SOPs carry verification evidence (DQ3 QC-2)
- **tauri:** Get_quality_report — cross-project quality aggregation (DQ3 QC-3)
- **pet:** Desktop pet window with native drag region
- **app:** Agent naming, auto-update entry, fluid chat widths
- **webui:** Muted ink rows for finished reminders, pulse for running
- **app:** Settings panel — models, skills/MCP, soul status, token stats
- **tools:** Computer use — screenshot/click/type/key/scroll + AX read_screen

### 🚜 Refactoring

- **frontend:** Guard double load-earlier + 44px touch target
- **core:** Hoist LEGACY_ASK_USER_KEY to module scope (simplify pass)
- **quality:** Drop redundant synthesis-prompt clone (simplify pass)

### 🧪 Testing

- **diagnostics:** Deterministic render test + tolerant spawn check
- Sync ci job assertions with macOS-only workflow
- **llm:** Fix mixin fake-session trait stubs
- **skill-mcp:** Fix reload_incremental test paths + first-call sentinel semantics
