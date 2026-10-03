# 📄 输出格式规则：报告一律用 HTML（2026-09-27）

**用户明确要求：所有"报告"类产出都给他 HTML，不要 Markdown。** 原话：
「你记住了所有报告都用html给我看，markdown看着非常不舒适」。

适用范围 —— 凡是**供人阅读的成品/汇总**，一律生成 `.html`：

- 分析报告、调研结论、验收/审计报告、状态汇总、对比表、交付说明、复盘文档
- 多个 Markdown 段落 + 表格 + 代码块组成的"文档型"产出

写法要求：

- 单文件、**离线可打开**：图片 base64 内嵌，不要外链 CSS/JS/字体（`src="data:..."`）。
- 深色主题，与现有报告保持一致：背景 `#0b0d10`、正文 `#c3cbd8`、
  强调色 `#cbb26a`（标题/表头）、成功色 `#5fd3a3`、边框 `#232a35`。
- 用 `<table>` 呈现对比数据，`<pre>` 呈现命令与验收输出，`<code>` 呈现路径/参数。
- 产出 HTML 后**在对话里也要给出关键结论**，不要只说"报告已生成"（避免用户还得开文件才知道结果）。
- 同一主题的多个报告用 `<a href="...">` 互相链接（如主报告 ↔ 说明页）。

例外（仍可用 Markdown）：

- 对话中的即时回答、简短说明、步骤解释 —— 这些不是"报告"。
- 代码注释、commit message、`README`/`CHANGELOG` 等项目内既有 Markdown 文档。

---

# ⛔ 硬性规则：绝不关闭用户正在使用的 OpenZen

**NEVER quit, kill, or restart the user's running OpenZen process.**

用户会把 OpenZen 用于正在执行的任务。修改代码、重建、重新打包时：

- **允许**：编辑源码、`cargo check/test/clippy`、`cargo tauri build`、生成 bundle、校验签名。
- **禁止**：`osascript -e 'quit app "OpenZen"'`、`pkill -f OpenZen`、`killall OpenZen`、
  `kill <pid>`，以及任何等价手段（包括安装脚本里默认执行这些动作）。
- **替换 `/Applications/OpenZen.app` 前**：如果 OpenZen 正在运行，先构建好产物就**停下来**，
  把产物路径和「请自行退出后重装」的提示告诉用户，由用户决定何时操作。
- **唯一例外**：用户明确说「可以关掉/重启 OpenZen」时才可退出。

正确做法见 `scripts/install-macos.sh`：默认只构建+校验，检测到运行中就跳过安装并提示；
仅当显式传入 `--restart` 时才执行退出→安装→重启。

---

## Gateway-leaked tool args — respond reply lost → whole-thinking promoted as reply (2026-10-03)

**Symptom**: agent's final reply and thinking "fused into one blob". In the
noon session (`fd68d2e3`, deepseek-flash via `opencode.ai/zen/go/v1`) the
message had 22 reasoning blocks, **zero** `text_*` events, and
`content == thinking` (48,935 chars of multi-turn reasoning delivered as the
user-visible reply).

**Cause chain** (three failures stacked):
1. The gateway converts the model's DSML/XML tool calls
   (`<parameter name="k">v</parameter>`) into chat-completions `tool_calls`
   but sometimes leaks half-converted fragments as the accumulated
   `arguments` string — `{"response">我按你描述...`, `{"max_chars">2500</parameter>`.
   Strict JSON parse fails → the raw string became the whole argument object.
2. `agent_loop` could not read `arguments.response` → no `respond_override`
   → `full_response` stayed empty (the gateway streamed nothing on the text
   channel for this run).
3. The Bug-1 fallback (agent_loop, end of run) promoted thinking-as-reply —
   and it promoted **all turns concatenated**, not just the final turn.

Note `INTERNAL_TAGS` also lacked `"think"`, so a `</think>` close could never
be matched and Qwen3-style replies could trap in the reasoning channel — the
original Bug-1 this fallback was written for.

**Fix (2026-10-03)**:
- `crates/oz-llm/src/stream.rs` → `repair_parameter_style_args` recovers
  key/value pairs from the leaked shapes (values may run to end-of-string
  when the gateway truncated the call); wired into `normalize_tool_args`'s
  String branch so every parse site ([DONE] flush, end-of-stream flush,
  speculative, checkpoint-resumed blocks via agent_loop) benefits.
  `respond` args then parse to `{"response": "…"}` again and the canonical
  reply path works.
- `agent_loop` respond handling: belt-and-braces repair for String-shaped
  arguments that bypassed the stream repair.
- Bug-1 fallback now promotes **only the final turn's thinking**
  (`last_turn_thinking`), never the whole multi-turn transcript.
- `"think"` added to `INTERNAL_TAGS` (Qwen3/DeepSeek close-tag matching).

**Tests**: `crates/oz-llm/src/stream.rs` `repair_*` / `normalize_repairs_leaked_parameter_string`
use the exact leaked bytes from the noon session as fixtures.

Known limitation: when the gateway truncated the leaked `respond` args
mid-value, the recovered reply is the truncated text — upstream loss that no
client-side repair can undo. Watch for `[oz-llm] normalized degenerate tool
args` / `[agent_loop] recovered respond arguments` in logs to spot recurrences.

---

# Tauri E2E driving — lessons from the ask_user test (2026-06-17)

The `ask_user` tool flow was first verified in the real Tauri webview
(non-Playwright, non-DevTools). The driving technique below is reproducible
and lives at `scripts/e2e/tauri_ask_user_e2e.sh`.

## The hard-won technique

Tauri webviews (WKWebView on macOS) are pickier than a normal browser:

| Action                         | What works                                       | What does NOT work                              |
|--------------------------------|--------------------------------------------------|-------------------------------------------------|
| Click on a sidebar button      | `cliclick c:X Y`                                 | —                                               |
| Click on the **chat Send**     | `/tmp/cgclick.py 1872 887 100` (CGEvent mousedown + 100ms hold + mouseup) | `cliclick c:1882 875`, `osascript "click at"`, `cliclick u:` |
| Type into the **chat textarea**| `/tmp/cgtype.py "..."` (CGEvent + Unicode override) | `cliclick t:"..."` (strips spaces), `pbcopy` + `keystroke v` (Svelte 5 `bind:value` does NOT fire on paste) |
| Click on the **dialog "Send response"** | `/tmp/cgclick.py 1830 620 100`        | `cliclick c:1830 620` (no hold → silently dropped) |
| Type into the **dialog "Your response"** | `/tmp/cgtype.py "..."`                | same as chat textarea                            |

**Why the hold matters on Send clicks**: WKWebView's hit-testing on small
buttons needs the mousedown and mouseup to be a single "gesture". A pure
`cliclick c:` is fast enough to miss the button. A 100 ms hold is reliably
accepted.

**Why CGEvent Unicode override is the only typing path that works**:
Svelte 5's `bind:value={inputText}` subscribes to the *input* DOM event, and
the event only fires when the browser believes a real key was pressed.
`CGEventKeyboardSetUnicodeString(event, len, chars)` with `keycode=0` sends
the unicode string directly, bypassing keycode-to-character translation,
and the browser sees it as a typed key.

`cliclick t:` strips spaces from multi-word arguments; it also doesn't
generate unicode string events reliably in WKWebView.

**IME is the silent killer**: if the system input source is Pinyin / SCIM /
any IME, those unicode strings get intercepted and turned into Pinyin
candidates. The fix is to switch to "ABC" *before* typing:
```bash
osascript -e 'tell application "System Events" to keystroke " " using {command down, control down}'
```
(Cmd+Ctrl+Space is "select next input source" — opposite of Spotlight.)

## Helpers (at /tmp, not in repo)

- `/tmp/cgtype.py` — uses `Quartz.CoreGraphics.CGEventCreateKeyboardEvent`
  with `CGEventKeyboardSetUnicodeString` to send raw unicode chars.
- `/tmp/cgclick.py` — uses `CGEventCreateMouseEvent` + separate down/up
  with `time.sleep(0.08)` between for a single click.
- `/tmp/e2e_venv` — Python 3.12 venv with `pyobjc-framework-Quartz` and
  `pillow` installed.

## Coordinates for a 1920×1080 display, Tauri in the right half

Tauri window position `(1200, 80)`, size `700×850`:

| Element                                     | Coords            | Notes                              |
|---------------------------------------------|-------------------|------------------------------------|
| Sidebar "+ New Chat"                        | `(1370, 204)`     | Standard `cliclick` works          |
| Chat textarea (for click-to-focus)          | `(1620, 870)`     |                                    |
| **Chat Send button (paper plane)**          | **`(1872, 887)`** | The blue square, NOT (1882, 875)   |
| AskUserDialog "Your response" textarea     | `(1700, 540)`     |                                    |
| AskUserDialog "Cancel" button               | `(1790, 620)`     |                                    |
| AskUserDialog **"Send response"** button    | **`(1830, 620)`** |                                    |
| Tauri "Stop" (red square, bottom-right)     | `(1835, 910)`     | Stops a running agent              |

**Do not eyeball these.** The Send button is a ~55×55 px square; off by
~10 px in either direction drops the click silently. The discovery method
was `Pillow.Image.crop` + 4× upscale of the actual button region.

## Tauri SSE event flow (verified)

1. Frontend `sendMessage` (Tauri mode) → `tauriInvoke("send_message", ...)`
2. Tauri `run_agent_for_session` runs the agent loop in a background task.
3. On `ask_user` tool return with `INTERRUPT`/`HUMAN_INTERVENTION`:
   - Tauri emits `StreamEvent::AskUserPending` over the SSE bus.
   - Tauri sends a `protocol_v1` envelope of type `ask_user_pending` with
     a JSON-stringified `data` field (line 374 of
     `frontends/src/lib/stores/chat.ts` handles this).
4. Frontend `setPendingAskUser({question, candidates})` → AskUserDialog
   renders (rendered at `App.svelte:165` when `$chat.pendingAskUser` is
   truthy).
5. User types reply, clicks "Send response" → `chat.submitAskUserResponse`
   → Tauri command `ask_user_response` (lib.rs:308) sets the slot in
   `ask_user_rxs[session_id]`.
6. The agent loop's `ask_user_rx.lock()` (ga-core/src/agent_loop.rs:852-905)
   sees the value, resumes the same run.

## Known log locations

- Tauri IPC log (only `send_message` entries): `~/.openzen/logs/openzen.log`
- Vite dev log: `/tmp/vite-dev.log`
- Backend `openzen serve` log: `/tmp/openzen-server.log` (only writes on
  MCP/SAVE events; not on every chat)
- `/tmp/openzen.log` is NOT the Tauri log; the real one is in
  `~/.openzen/logs/`.

## Data root & OPENZEN_DATA_DIR (Plan B, 2026-08-09)

All runtime data lives in a **data root**, never in the source tree:

- Default data root: `~/.openzen/`
- Override: `OPENZEN_DATA_DIR=/path/dev` — a dev build can run against an
  isolated data tree with zero pollution of the user's real data.
- Layout under the data root: `workspace/` (agent working dir + `memory/` +
  `openzen/` platform data + `facts/`), `memory_erme/`, `harness/`
  (harness ledger — `harness_state.json`), `.skill_mcp/`, `logs/`,
  `mykey.toml`, `projects.json`, `openzen/sessions.json`.
- If `memory/ memory_erme/ harness/ checkpoints/ openzen/ facts/ .skill_mcp/`
  ever reappear at the repo root → an old binary is writing back into the
  source tree. **Block and fix** (see git-skill dimension 11).
- Dev-mode isolation: `OPENZEN_DATA_DIR=/tmp/openzen-dev cargo tauri dev`.

## ⚠️ Port 8000 is RESERVED for the oMLX model server

`omlx-server` (the oMLX app) owns **`127.0.0.1:8000`** — every local model
in `~/.openzen/mykey.toml` points its `apibase` at `http://127.0.0.1:8000/v1`.
**Never** start any other dev backend/server on port 8000 (or 127.0.0.1:8000).
A `uvicorn`/FastAPI app binding `127.0.0.1:8000` shadows the model server on
macOS (more-specific bind wins) and every LLM call then returns
`HTTP 404 {"detail":"Not Found"}` → agent exit_reason=llm_error, and
/resume stays idle because every turn instantly 404s.

Symptom signature in `~/.openzen/logs/openzen.log`:
```
[openzen] LLM stream error (attempt 6/6), retrying turn N: HTTP error: 404 {"detail":"Not Found"}
[openzen] agent outcome: exit_reason=llm_error turn=N error=HTTP error: 404 ... (7 consecutive)
```
Diagnosis: `lsof -nP -iTCP:8000 | cat` — TWO listeners (one `*:8000` from
omlx, one `127.0.0.1:8000` from the intruder) = conflict.
Fix: kill the intruder (`kill <pid>`), verify `curl http://127.0.0.1:8000/v1/models`
returns 401 "API key required" (not a FastAPI 404), then re-run/resume.

If a generated project (e.g. a longtask kanban-board) must run a backend,
use **port 8001+** and update every hardcoded `:8000` reference
(vite proxy, ws client, run.sh, docker-compose, docs).

## Release process & versioning (2026-08-09)

Version numbers are DERIVED from commit history (git-cliff), not chosen by hand.
The FIRST public release is **v0.1.0** (currently private — do not tag or publish yet).

### Commit convention (MANDATORY for all future commits)

Every commit must use Conventional Commits prefixes — git-cliff derives the
next version from them:

| Prefix | Effect on version | Example |
|--------|-------------------|---------|
| `feat:` | MINOR (0.1.0 → 0.2.0) | `feat(erme): add idle rambling cycle` |
| `fix:` | PATCH (0.1.0 → 0.1.1) | `fix(core): correct token accounting` |
| `feat!:` / `BREAKING` | MAJOR | `feat!: switch to protocol v2` |
| `refactor:` / `docs:` / `test:` / `chore:` / `ci:` | no bump | `chore: remove dev artifacts` |

Scope in parens is optional but recommended (`feat(erme):`, `fix(webui):`).

### Release flow

```bash
scripts/release.sh --dry-run   # preview: next version + changelog (no changes)
scripts/release.sh             # full: test gate → bump → sync version → CHANGELOG → build → tag
```

`release.sh` WILL NOT run if:
- working tree is dirty, or not on `main`
- `cargo test --workspace -- --test-threads=1` has any failure (test gate)

After the script creates tag `vX.Y.Z`, push with `git push origin main --tags`.
`.github/workflows/release.yml` (triggered on `v*` tag) re-runs the test gate,
builds the macOS dmg, and attaches it to the GitHub Release — the Release
version number always equals the tag name.

Version fields that must stay in sync: `src-tauri/tauri.conf.json` +
`frontends/package.json` (handled by release.sh). Rust crate versions in
`Cargo.toml` are internal and stay at 0.1.0.

### Known pre-existing test fixes (2026-08-09)

- `recover_from_real_checkpoint_populates_store`: session_id was hardcoded to a
  stale UUID — now matches `tests/longtask/2/` checkpoint data (`fe54c2c0-…`).
  Test self-skips when the local dir is absent (CI-safe).
- `adr_count_matches_readme_index`: ADR file filter `starts_with("000")` failed
  at 0010 (4-digit prefix check now used).
- `test_temporal_query` (vendor): now sleeps before sampling `before` so the
  first edge timestamp is strictly less than the query window (closed-interval
  `temporal_query` was flaky when both landed on the same clock tick).
- `test_run_idle_cycle_promote_dedup` (vendor): assertion compared two
  unrelated random counts (`second.promoted <= first.rambled`); with
  `max_conjectures=3` the second cycle legitimately promotes fresh ids.
  Now asserts the promoted-id set only grows and its delta equals
  `second.promoted` (true dedup invariant, deterministic under randomness).

## Building / installing the macOS app — always `cargo tauri build` (2026-09-09)

`cargo build --release -p openzen-tauri` + copying the binary into an existing
`OpenZen.app` bundle produces a **white-screen app**: the frontend assets
(`frontends/dist`, wired via `frontendDist` in `src-tauri/tauri.conf.json`) are
rebuilt by `beforeBuildCommand` (`npm ci && npm run build`) and embedded by the
tauri CLI pipeline; a bare cargo build embeds nothing (the artifact is ~4MB
smaller than a correct one — exactly the missing embedded frontend assets).

Rules:

- Compile/lint/test: `cargo check` / `cargo test` / `cargo clippy` — fine.
- Produce or update the runnable app: `cargo tauri build` (or
  `cargo tauri build --bundles app` to skip the dmg), then replace
  `/Applications/OpenZen.app` wholesale with
  `target/release/bundle/macos/OpenZen.app` (quit the running app first).
  Never swap just the `MacOS/openzen-tauri` binary.

## OpenZen lifecycle rules for AI assistants (2026-09-09)

- **Never quit/restart the running OpenZen app while a task is executing.**
  Check `~/.openzen/logs/openzen.log` for an active run (compress check /
  usage lines within the last minute, or the UI badge 运行中) before any
  quit. Background commands that include a `quit` may fire late and kill a
  run the user just started — sequence installs only in confirmed idle
  windows, or launch a SEPARATE app instance for testing instead of
  touching the user's window.
- Background build tasks are not cancellable mid-command: never bundle
  `osascript quit` into a backgrounded build+install chain.
- Frontend-only changes do NOT trigger a Rust rebuild (assets embed at
  compile time): `touch src-tauri/src/main.rs src-tauri/src/lib.rs` before
  `cargo tauri build`, or the .app ships with the stale frontend.

## Tool-argument aliases — canonicalization layer (2026-09-27)

**Symptom** (recurring, twice mis-diagnosed as "already fixed"):
`{"error":"code_run: missing `code` argument","status":"error"}` and
`{"error":"write: missing file_path","status":"error"}` on calls whose
payload was clearly present. Real frequency in `~/.openzen/openzen/sessions.json`:
`missing `code` argument` ×422, `missing file_path` ×72.

**Cause**: weak-schema models (the local MLX models especially) send the right
value under the wrong *key*:
- `cmd` / `command` / `script` / `content` instead of `code`
- `mode: "bash"` where the schema means `type`
- `path` / `file` / `filename` / `filePath` instead of `file_path`
- the tool name itself as the key: `{"code_run": "…", "type": "bash"}`

The tool read only its canonical keys and rejected the call. Note this is a
*different* shape from the earlier `normalize_tool_args` fix (which collapses
doubly-JSON-encoded and single-key `{"content": "<json>"}` wrappers) — that one
still works and is still needed.

**Fix (two layers, one source of truth)**:
1. `crates/oz-core-types/src/tool_args.rs`
   - `canonicalize_tool_args(tool, args)` — curated alias table for code_run /
     read / write / edit / patch / glob / grep / ls, plus the tool-name-as-key
     and lone-unknown-string-key fallbacks. Moves values, never invents them;
     never overwrites a canonical key; idempotent.
   - `repair_args_against_schema(tool, args, schema)` — schema-driven repair for
     MCP / dynamically registered tools (first `required` property).
   - `missing_arg_error(tool, missing, args)` — errors now name the expected
     key **and** the keys the model actually sent, so the next turn can
     self-correct instead of blind-retrying.
2. Wiring
   - `crates/oz-tools/src/registry.rs` → `ToolRegistry::dispatch` re-keys before
     `execute`. This is the single execution choke point for every runtime path
     (`ToolRegistryHandler` → `registry.dispatch`; the legacy `shared_registry()`
     is test-only). It logs `[tools] re-keyed non-canonical arguments …`.
   - `crates/oz-llm/src/stream.rs` → `normalize_tool_args_for(tool, args)` used at
     all three parse sites (mid-stream flush, speculative `ToolCallReady`,
     stream terminator) and in `oz-core/src/agent_loop.rs`, so the emitted event,
     the persisted `ContentBlock::ToolUse` and the executed call all carry the
     canonical keys — a model imitating its own history then imitates the
     correct shape.

Also changed: `write` no longer silently writes an **empty file** when the
`content` key is absent (an explicit `""` is still a legal touch-style create),
and tool descriptions now state the fixed parameter names.

**Regression guard**: `crates/oz-tools/src/registry.rs` →
`real_session_transcripts_gain_canonical_keys` replays every real
`<tool_use:…>` from `tests/longtask/*/openzen/checkpoints/*.json` and asserts
that any call using a key outside the tool's schema still ends up with its
primary argument (self-skips when the fixture is absent). Per-shape unit tests
live in `crates/oz-core-types/src/tool_args.rs` (`real_shape_*`,
`canonicalization_is_idempotent`).

Do **not** "fix" this by rewriting persisted history at request time — history
is only canonicalized as it is parsed/stored, so an old failing call keeps its
old error text next to it (rewriting it would show a canonical call paired with
a "missing argument" error, which teaches the wrong lesson).
