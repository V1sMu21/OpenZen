---
name: verify-check
description: End-to-end verification pipeline for OpenZen. Runs compile check, unit tests, lint, and optional Tauri desktop E2E screenshots. Confirms code changes work correctly at every level.
tags: [verify, check, build, test, lint, e2e, cargo, tauri, cargo-test, cargo-clippy, screenshot]
---

# verify-check — End-to-End Verification Pipeline

Run a layered verification pipeline from fast compile checks to full desktop E2E. Each level is independent — failures in one level don't stop others from running.

## When to Use

- After code-review and simplify pass
- Before opening a PR
- When you need to confirm a change works end-to-end
- Third step in the verification chain

## Required Tools

- bash (cargo commands, osascript, screencapture)
- file_read (check logs)
- The Tauri E2E scripts if running Level 3

## Verification Levels

### Level 0: Compile Check (30s)

```bash
cd /Users/macstu/Documents/apps/openzen
cargo check --workspace 2>&1
```

**Pass condition**: Exit code 0, no errors.
**On failure**: List each error with file:line and message. Do NOT proceed to higher levels until fixed.

### Level 1: Unit Tests (60s)

```bash
cd /Users/macstu/Documents/apps/openzen
cargo test --workspace --test-threads=1 2>&1
```

**Pass condition**: All tests pass. 
**On failure**: List failed tests with error output.
**Note**: Some tests require oMLX service at http://127.0.0.1:8000 — if unavailable, those tests will be skipped.

### Level 2: Lint Check (60s)

```bash
cd /Users/macstu/Documents/apps/openzen
cargo clippy --workspace -- -D warnings 2>&1
```

**Pass condition**: No warnings treated as errors.
**On failure**: List each clippy warning. Distinguish pre-existing warnings from new ones (check if warning file is in the current diff).

### Level 3: Tauri Desktop E2E (2min, macOS only)

Only run if:
- Running on macOS
- Tauri dev app is running (`pgrep openzen-tauri`)
- User requests `--full` or `--e2e` mode

Pre-checks:
```bash
pgrep -fl openzen-tauri
test -f /tmp/cgclick.py && test -f /tmp/cgtype.py
curl -s http://127.0.0.1:8000/v1/models | head -1
```

E2E test flow:
1. Send a test message via CGEvent injection:
   ```bash
   python3 /tmp/cgclick.py <send_x> <send_y> 100
   ```
2. Wait 30 seconds for agent response
3. Capture screenshot:
   ```bash
   screencapture -x -t png /tmp/verify-e2e.png
   ```
4. Verify screenshot using VL model (if available):
   Send to http://127.0.0.1:8000/v1/chat/completions with:
   - Question: "Is there an AI agent response visible in this chat app? Answer YES or NO."
   - Image: /tmp/verify-e2e.png

For detailed E2E procedures, load the `tauri-e2e` skill via `skill_mcp_search("tauri-e2e")`.

### Level 4: Log Check (15s)

Check known log locations for errors after running:

```bash
# Tauri IPC log
tail -50 ~/.openzen/logs/openzen.log

# Check for panic messages
grep -i "panic\|ERROR\|fatal" ~/.openzen/logs/openzen.log | tail -20
```

## Output Format

```
## Verification Report

### Level 0: Compile
✅ cargo check passed (N crates, 0 errors)

### Level 1: Tests  
✅ N tests passed, 0 failed (or: ❌ M tests failed)

### Level 2: Lint
✅ no new clippy warnings (or: ⚠️ N pre-existing warnings)

### Level 3: E2E
⊘ Skipped (use --e2e to enable)

### Level 4: Logs
✅ no panic or ERROR in recent logs

### Verdict
✅ ALL CHECKS PASSED / ❌ CHECKS FAILED
```

## Command Shortcuts

- `verify-check` — runs Level 0 + 1 + 2 (default)
- `verify-check --full` — runs all levels
- `verify-check --quick` — runs Level 0 only
- `verify-check --e2e` — runs Level 3 only

## Important Rules

- Run ALL levels, don't stop early on failure
- Distinguish NEW issues from PRE-EXISTING issues
- Level 3 requires macOS GUI — skip gracefully on other platforms
- Report in a clear table format
- Do not modify code during verify (this is CHECK only, not FIX)
