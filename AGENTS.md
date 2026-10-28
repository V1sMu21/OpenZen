# 📄 输出格式规则：报告一律用 HTML（2026-09-27）

**用户明确要求：所有"报告"类产出都给他 HTML，不要 Markdown。** 原话：
「你记住了所有报告都用html给我看，markdown看着非常不舒适」。

适用范围 —— 凡是**供人阅读的成品/汇总**，一律生成 `.html`：

- 分析报告、调研结论、验收/审计报告、状态汇总、对比表、交付说明、复盘文档
- 多个 Markdown 段落 + 表格 + 代码块组成的"文档型"产出

**边界（2026-10-23 补充）**：判据是「这是不是一次**调研**」，不是「我做了多少工作」。

- ✅ **出报告**：调研 / 分析 / 审计 / 验收 / 复盘 / 方案对比 —— 结论本身就是要交付的成品。
- ❌ **不出报告**：改代码、改配置、接模型、装依赖、跑构建、修 bug 等**实施类任务** ——
  做完直接在对话里说清「改了什么、怎么生效、怎么回滚」即可，不要额外生成 `.html`。
  用户原话：「你不需要出报告的，这不是调研」。

写法要求：

- 单文件、**离线可打开**：图片 base64 内嵌，不要外链 CSS/JS/字体（`src="data:..."`）。
- 深色主题，与现有报告保持一致：背景 `#0b0d10`、正文 `#c3cbd8`、
  强调色 `#cbb26a`（标题/表头）、成功色 `#5fd3a3`、边框 `#232a35`。
- 用 `<table>` 呈现对比数据，`<pre>` 呈现命令与验收输出，`<code>` 呈现路径/参数。
- 产出 HTML 后**在对话里也要给出关键结论**，不要只说"报告已生成"（避免用户还得开文件才知道结果）。
- 同一主题的多个报告用 `<a href="...">` 互相链接（如主报告 ↔ 说明页）。

例外（仍可用 Markdown）：

- **实施/配置类任务的交付**（见上文「边界」）：修 bug、改配置、接入模型、安装依赖、构建产物等，做完在对话里说明结果即可，不产出报告文件。
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

**Fix (2026-10-03, second pass after the noon session continued on the new build)**:
- Tier-1 **JSON reconstruction** in `repair_parameter_style_args`: the gateway
  sometimes splices junk (`<parameter: `) between a half-open key and an
  otherwise-complete JSON continuation —
  `{"file_path"><parameter: "/p", "content": "…\n"}`. Pair extraction only
  recovered `file_path` and the write still failed `missing content`. Tier 1
  rebuilds `{"KEY": <everything from the first quote of the remainder>}` and
  accepts it only when it parses (the tail usually carries its own closing
  brace; add one only when the call was truncated).
- The system-reminder `Prior task failures: .openzen/reflections.jsonl` now
  carries the **absolute** workspace path — the bare relative form was being
  resolved against the data root by the model, landed outside the sandbox,
  and the read was rejected every session.

**Third pass (2026-10-03 evening run)** — the gateway evolved to a THIRD
shape: VALID JSON whose string values silently swallow the model's DSML
parameters (`{"file_path": "/p.sh</parameter>\n<｜DSML｜  parameter
name=\"content\"…>#!/bin/bash…"}` — parses fine, so no repair path ran,
but `content` was buried inside `file_path`). Added
`sanitize_dsml_pollution` (cuts values at the `</parameter>` boundary when
DSML/parameter junk follows and lifts the embedded named params back into
the object) plus nameless-DSML mapping (`<｜DSML｜: "…" → the tool's first
missing canonical param, via `canonical_param_names`) in the repair path.
The tier-1 reconstruction now refuses when the gap before the first quote
crosses a `</parameter>` boundary (that quote belongs to a nameless value,
not the opener key).

**Thinking dial (2026-10-03)**: per-model `reasoning_effort` now flows end to
end: ChatInput pills → `send_message` (tauri `reasoningEffort` / webui
`reasoning_effort`) → `run_agent_for_session` → `SessionConfig.reasoning_effort`
→ `openai.rs` payload (`reasoning_effort` for chat_completions, `reasoning.effort`
for the Responses API; claude.rs already consumed it). UI levels: 默认 (follow
mykey.toml, sends nothing) / low / medium / high. The opencode zen gateway
accepts minimal/low/medium/high and rejects `off` (422) — hence no "off" option.

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

**Fourth pass (2026-10-23 morning desktop session `4596ac63`)** — two new
gateway shapes, both reproduced from the real bytes (fixtures under
`crates/oz-llm/tests/fixtures/`):
1. **Separator corruption**: `{"candidates": […], "question">Go: "…"}` — the
   `": "` separator became `">Go: "` and the whole question was repeated
   dozens of times. `repair_parameter_style_args` saw no `{"key">` opener and
   returned `None`, so `ask_user` received `question: ""`; the loop then
   parked on it. Eight such questions burned **40 minutes** (8 × the old 300s
   timeout) while the user was away.
2. **Fullwidth separator + nameless parameter**:
   `{"file_path"｜>work/x.py</parameter>\n<parameter: "…content…", …}` — no
   opening quote on the first value, and the second parameter lost `name=`.
   The valid-JSON variant `{"file_path": "work/x.py</｜DSML｜:"}` parses
   cleanly but swallows the dangling marker into the path;
   `sanitize_dsml_pollution` now cuts at `</｜DSML` too.

**Fix (2026-10-23)**:
- `crates/oz-llm/src/stream.rs` -> new field-level salvage scanner
  (`salvage_leaked_fields` / `field_values` / `pick_field_value`) merged into
  `repair_parameter_style_args`: it reads a field value across arbitrary
  separator junk (a short prose gap before the quoted value, or an unquoted
  value terminated by `</parameter>`), prefers the longest of repeated
  occurrences, unwraps doubly-encoded argument strings first, and maps nameless
  `<parameter: "..."` values onto the tool first missing canonical field.
  `ask_user` gained a canonical alias table so the nameless fallback knows its
  primary key.
- `ask_user` hardening: an empty question is a tool error (never a park); the
  same question twice in one turn is rejected with a tool result; at most 4
  questions per turn; `ask_user` / `submit_plan` bypass the tool semaphore (the
  12 `Tool queue busy (>15s): skipping ask_user` drops came from control tools
  queueing behind long `code_run` calls).
- **Waiting mode**: `LoopConfig.ask_user_timeout_secs` defaults to 0 = wait
  until the user answers or the run is stopped (a non-zero bound is still
  honoured). The session persists `SessionStatus::Waiting` plus a
  `pending_ask_user` record, so the sidebar shows an amber waiting dot, the
  title bar shows the waiting label, the frontend watchdog is suspended while
  waiting, and a page reload restores the dialog. A reply that arrives after an
  app restart is stored as a one-shot `resume_note` and the frontend resumes
  from the checkpoint with the question + answer injected.
- Notifications no longer trust window focus alone: `notify_if_unfocused` also
  probes HID idle time (`ioreg`), so leaving OpenZen focused while walking away
  still delivers a "needs your answer" notification (the 10:28 skip in this
  session).

**Tests**: `real_20261023_ask_user_leak_is_repaired` /
`real_20261023_write_leak_is_repaired` replay the exact 7.5 KB ask_user blob and
the write blob from the session (fixtures in `crates/oz-llm/tests/fixtures/`);
`ask_user_rejects_empty_and_dedupes_identical_questions` covers the park guards;
`waiting_status_and_pending_question_roundtrip` covers the persisted waiting
state.

**Fifth pass (2026-10-23 evening, same session `4596ac63` replayed from
`sessions.json`)** — the 12:58 report of "tool calls still error, acceptance
loops, label wrong" traced to five independent causes, each fixed at the root:

1. **Post-completion verification loop (the "验收自我循环")**. A reviewer that
   answered `insufficient_evidence` was treated as `must_fix` (`!v.pass`), so
   the run re-entered the loop, re-ran verification the assertion gate itself
   blocked ("needs approval"), and got reviewed again with the same evidence —
   twice per run, every run. Now only a *named defect* earns another round
   (`quality::review_requires_fix`: `Failed` status or a high-severity issue);
   an inconclusive review ends the run with an honest "unverified" note. A single
   run-wide budget, `MAX_GATE_CONTINUATIONS = 4`, now bounds EVERY gate that can
   send the agent back to work (assertions, review, quick checks, diff
   self-check, unresolved suspicion) — before, each had its own bound and they
   stacked.
2. **`long_term` failed on every call, permanently.** Two real tool bugs found
   in the session data: (a) `category=sop` passed the whole payload as the SOP
   *name*, which became a ~1000-character filename →
   `File name too long (os error 63)`; (b) `category=fact` read
   `~/.openzen/.skill_mcp/facts/global_mem.txt`, which held one invalid byte
   (position 5705) → `stream did not contain valid UTF-8` forever, because
   `read_to_string` fails the whole read on a single bad byte. Fixed with
   `long_term::sop_title` (bounded first-line title) + `sop::bounded_title`
   (80-char cap, hash suffix when truncating, so two long names cannot collide)
   and `memory::read_text_lossy` (lossy read; the next append rewrites the file
   as valid UTF-8 and heals it).
3. **Blank `ask_user` dialogs.** The gateway leaked the same question in three
   shapes; in two of them it repeated the whole `candidates` array *in front of*
   the question, so the old repair could hand back an empty question — the UI
   showed 16 empty dialogs at 10:33 and the user was told nothing
   ("你没有把问题发给我"). The salvage now recovers the real question from all
   three shapes (fixtures + test below), and an empty question is a tool error,
   never a park. The 12 `tool queue busy` drops for `ask_user` came from control
   tools queueing behind six concurrent probes; both semaphores are now skipped
   for `ask_user`/`submit_plan`.
4. **`each_key_duplicate` crashed the message list** (5× at 12:46:33,
   `frontend-errors.log`). Two keyed `{#each}` blocks could receive duplicate
   keys and Svelte 5 aborts the whole render on that. Message ids come from the
   server `idx`, which is only unique *within a page* (paging counts from the
   END of the vector, so a page fetched before a run appended messages can reuse
   a live `idx`); tool rows are keyed by call id, which the gateway reuses when
   it merges calls. Both lists now derive collision-proof keys
   (`uniqueMessageIds` in `stores/chat.ts`, `partKeys` in `ChatMessage.svelte`).
5. **Labels/defaults**: the title-bar switch is back to **完全访问** /
   "Full access" (the 2026-10-22 pass had renamed it 免审批执行（危险）), and
   auto-crystallization now defaults to **off** in `AppState`
   (`AtomicBool::new(false)`).

**Tests**: `real_20261023_ask_user_question_survives_every_go_leak_variant`
replays the three real leak fixtures extracted verbatim from `sessions.json`
(`ask_user_repeated_go_leak_20261023.txt` 8 KB, `…_short_…`, `…_mid_…`);
`only_a_named_defect_earns_another_fix_round` (oz-core) pins the loop rule;
`corrupt_fact_file_does_not_block_memory_forever` (oz-skill-mcp) writes a lone
continuation byte into the fact file and asserts the read degrades and the next
append heals it; `test_long_term_sop_with_a_whole_document_payload` +
`sop_title_is_one_bounded_line` (oz-tools) cover the ENAMETOOLONG path.

Known gap (unchanged): the gateway still truncates values on its side
(`{"code":"cd:"}`, `{"content":"import:"}`, `{"response":"做完了。你之前自己用:"}`
are the raw accumulated bytes). Nothing client-side can restore lost text; the
calls fail loudly and the model re-issues them. Merged-call blobs are refused
with the `__oz_corrupted_call` sentinel rather than executed with wrong fields.

**Sixth pass (2026-10-23 14:20–14:35) — found by an isolated end-to-end run,
not by unit tests.** The harness (worth reusing) is: fresh data root under
`/private/tmp`, `./target/debug/openzen --config <root>/mykey.toml --dir <root>
--assets assets serve --port 18610`, the random auth token is printed to the log
and every `/api/*` call needs `Authorization: Bearer <token>`, and
`POST /api/sessions/<id>/approve {"request_id","decision":"allow"}` answers the
approval gate — a small python loop tailing the log for
`approval_needed sent: … id=<uuid>` makes the run fully autonomous. Use
`POST /api/chat {"message","session_id","model_name"}` to start a task.
Do **not** use plain `/tmp` as the data root: it is a symlink to `/private/tmp`,
so the safety layer's canonicalize check rejects every write inside the
workspace ("cannot canonicalize path, rejecting"). Use `/private/tmp/...`.

1. **Policy-refused `[verify]` assertions were counted as failures.** The three
   synthesized assertions for a one-file task included
   `python3 …/hello.py | grep -q "e2e-ok"`; policy refuses pipes. The gate put
   that command into `outcome.failures` *as well as* `blocked`, so the run
   burned its assertion budget, re-entered the loop, and closed with
   "验收断言未通过（1 条），修复预算已耗尽" for work that was **never executed** —
   and the run record was self-contradictory (`assertion_status=needs_approval`,
   `assertions_failed=1`, `assertions_blocked=0`). Fixed: refused assertions are
   only ever `blocked`/`needs_approval`; the agent instead gets one bounded
   `format_assertion_policy_feedback` round that names the refused commands and
   the executable rewrite rules (`test -f`, `python3 -m py_compile`,
   `node --check`, `cargo check --quiet`; no pipes/`&&`/`;`/backticks, relative
   paths only). `build_spec_synthesis_prompt` now states that grammar, so
   synthesized specs are executable in the first place.
2. **The gate reported `not_run` instead of `synthesis_failed`** when no summary
   model is configured: the first finishing turn recorded nothing (the
   `is_configured()` branch had no `else`), while a later turn hit the
   `else if` branch and recorded `synthesis_failed` — the same run answered
   differently depending on how many finishing turns it took. Now recorded on
   the first turn.
3. **A workspace-wide compile break my per-crate runs missed**:
   `StreamEvent::AskUserAnswered` was not handled in `crates/oz-tui/src/event.rs`,
   so `cargo build --bin openzen` and the release gate failed while the Tauri
   app still built. Always finish with `cargo check --workspace --all-targets`
   (or the gate's `cargo test --workspace`), not with `-p <crate>` alone.

**Tests**: `blocked_assertions_are_visible_and_not_executed` now also pins that a
refused assertion is not a failure, and
`synthesis_prompt_constrains_the_assertion_grammar` pins the executable-grammar
instructions.

**Seventh pass (2026-10-23 evening) — out-of-workspace writes are now
APPROVABLE instead of a hard failure.** User decision: "改成审批".

`write` / `edit` / `patch` are on `SafetyGuard`'s safe list, so the approval
pipeline never saw them and their workspace boundary lived entirely inside the
tool. An out-of-workspace edit was therefore a flat failure the model could only
answer with a prose `ask_user` — the user could not *approve* it.

- `crates/oz-core-types/src/path_trust.rs` (new) — the two halves:
  `write_target_path` / `outside_workspace_target` (what the loop consults, with
  the deepest-existing-ancestor resolution) and the session-scoped registry
  `approve_path` / `deny_path` / `is_path_approved` / `is_path_denied` /
  `forget_session` (bounded: 64 paths × 32 sessions). Only an approval decision
  writes to it — the model cannot reach it, and a denial records nothing that
  could be mistaken for consent.
- `crates/oz-core/src/agent_loop.rs` — before dispatch, a mutating call whose
  resolved target is outside the workspace goes through the SAME approval
  channel as `code_run` (`approve_outside_write`: audit line, stop-signal race,
  `完全访问` auto-allow inside the handler). Allow → the path is recorded and the
  tool lets it through. Deny → the call fails with "the user denied…, do not
  retry"; the denial is recorded so a retrying model gets a deterministic answer
  instead of re-opening the dialog. Approvals are cleared at run start
  (`forget_session`), so an answer from an earlier run never authorizes this one.
- `crates/oz-tools/src/file_ops.rs` — the three mutating tools consult the
  registry (`write_path_permitted`) and share one boundary message.

**Verified end-to-end** (isolated harness, `scripts/e2e/isolated-agent-run.sh`,
cloud model, task = "write this line to a path outside the workspace"):
allow run → `requesting approval for write/outside workspace: …` → approved →
file written, and a second write to the same path did NOT ask again; deny run
(`E2E_DENY=write`) → the `write` tool did not execute. Caveat worth knowing:
with `write` denied the model simply used `code_run` (`echo … > path`), which
has its own approval gate — denying `write,code_run` together is what keeps the
file from existing at all. The harness now also answers `ask_user` (the app
waits forever for an answer, which made a denied run look like a stall) and
denies `computer_*` unconditionally.

**Tests**: `only_mutating_tools_name_a_write_target`,
`inside_the_workspace_needs_no_approval`,
`approvals_are_per_session_and_only_for_the_named_path`,
`denials_are_recorded_so_the_question_is_not_re_asked`,
`missing_parent_directories_still_resolve` (oz-core-types) and
`outside_write_needs_an_approval_and_then_succeeds` (oz-tools: unapproved →
blocked and file untouched, approved → written, other session → still blocked).

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

---

## 方案审查（plan review）— 大动作先交方案

凡是**会花时间或不可轻易回退**的动作（出片、批量替换、重构、跑长测试），先投一份方案到
`.openzen/plans/inbox/`，等用户在右侧栏批准后再动手。批准前不要开工。

```bash
python3 scripts/plan-submit.py --title "…" --file plan.md --est "约 20 分钟" \
  --option "稳=…" --option "快=…"
```

OpenZen 每 2 秒扫一次收件箱，方案会出现在右侧栏；用户的选择写在
`.openzen/plans/<id>.decision.json`，`comment` 是他的原话，据此改方案，不要猜。
完整契约见 `docs/plan-review.md`。
