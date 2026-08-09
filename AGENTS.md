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
  `openzen/` platform data + `facts/`), `memory_erme/`, `.skill_mcp/`,
  `logs/`, `mykey.toml`, `projects.json`, `openzen/sessions.json`.
- If `memory/ memory_erme/ checkpoints/ openzen/ facts/ .skill_mcp/` ever
  reappear at the repo root → an old binary is writing back into the source
  tree. **Block and fix** (see git-skill dimension 11).
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
