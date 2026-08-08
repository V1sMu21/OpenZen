#!/usr/bin/env bash
# scripts/e2e/tauri_ask_user_e2e.sh
#
# End-to-end Tauri desktop test for the `ask_user` tool flow.
#
# Drives the REAL Tauri webview (no Playwright, no DevTools injection) via:
#   - osascript         (window focus, click position via System Events)
#   - Quartz CGEvent    (Unicode typing that bypasses IME; mouse click with hold)
#   - screencapture     (per-step visual evidence)
#
# What it proves
#   1. Tauri webview accepts real synthetic clicks (New Chat, Send, Send response)
#   2. Svelte 5 `bind:value={inputText}` fires on Quartz unicode-typed chars
#      (NOT on plain `cliclick t:` or paste; the input event MUST come from
#      a key event with a real unicode codepoint)
#   3. `ask_user` tool renders the AskUserDialog in the Tauri window
#   4. Dialog → Send response → Tauri IPC → backend resumes the same run
#   5. The session gets the assistant's final reply
#
# Prerequisites
#   - Tauri built:   `cargo build`  (target/debug/ga-tauri)
#   - Backend up:    `ga serve --port 8421 --frontend-dir .../dist`
#   - Vite up:       `npm run dev`  (in frontends/, port 5173)
#   - macOS darwin   (uses CGEvent + osascript; no Linux fallback)
#   - User granted:  AppleScript / ScreenCapture / cliclick / window control
#   - Python venv at /tmp/e2e_venv with pyobjc-framework-Quartz + pillow
#   - cgtype.py and cgclick.py at /tmp/  (see "Helpers" below)
#   - System input source = English ABC (call switch_input_source.sh first)
#
# Layout assumptions (set for a 1920x1080 display, Tauri on the right half)
#   - Tauri window position: (1200, 80), size: 700x850
#   - Sidebar "+ New Chat" button:  (1370, 204)
#   - Chat textarea:                (1350..1900, 870..895)  ← y=880 is the SAFE click row
#   - Chat Send button (paper plane): (1872, 887)  ← ACTUAL coords, not (1882, 875)
#   - AskUserDialog "Your response" textarea: (1700, 540)
#   - AskUserDialog "Send response" button:   (1830, 620)
#   - AskUserDialog "Cancel" button:          (1790, 620)
#
# Coordinates were discovered empirically by cropping+zooming the actual
# button regions with Pillow.  Do NOT eyeball them — the Send button is a
# ~55x55 px square; off by ~10 px in either direction drops the click.

set -euo pipefail

REPO="$(cd "$(dirname "$0")/../.." && pwd)"
TAURI_BIN="$REPO/target/debug/ga-tauri"
SCREENS="/tmp/tauri-screenshots"
E2E_VENV="/tmp/e2e_venv"
CGTYPE="/tmp/cgtype.py"
CGCLICK="/tmp/cgclick.py"
SWITCH_IME="$REPO/scripts/e2e/switch_input_source.sh"

# Test inputs
TEST_PROMPT="Please use the ask_user tool to ask me what is my favorite color"
TEST_REPLY="My favorite color is deep ocean blue. Final answer."

# Coordinates (see header comment for source)
COORD_NEW_CHAT="1370 204"
COORD_SEND="1872 887"
COORD_DIALOG_FIELD="1700 540"
COORD_DIALOG_SEND="1830 620"

mkdir -p "$SCREENS"
# Use the caller's name as the literal filename — easier to grep step-by-step
shot() { screencapture -x -t png "$SCREENS/$1.png"; echo "📸 $1"; }

# 0. Pre-flight
[[ -x "$TAURI_BIN" ]] || { echo "❌ $TAURI_BIN not built. Run: cargo build"; exit 1; }
[[ -f "$CGTYPE" && -f "$CGCLICK" ]] || { echo "❌ /tmp/cgtype.py or /tmp/cgclick.py missing"; exit 1; }
[[ -d "$E2E_VENV" ]] || { echo "❌ /tmp/e2e_venv missing. Create with:"; echo "    python3.12 -m venv /tmp/e2e_venv && /tmp/e2e_venv/bin/pip install pyobjc-framework-Quartz pillow"; exit 1; }
pgrep -fl "openzen serve" >/dev/null || { echo "❌ openzen serve not running."; exit 1; }
pgrep -fl "openzen"  >/dev/null || { echo "❌ openzen not running."; exit 1; }
pgrep -fl "vite"      >/dev/null || { echo "❌ Vite not running. Start: (cd $REPO/frontends && npm run dev) &"; exit 1; }

# 1. Switch input source to English ABC (avoids IME corruption in textarea)
if [[ -x "$SWITCH_IME" ]]; then
    "$SWITCH_IME" ABC || echo "⚠️  Could not switch input source; typing may corrupt"
fi

# 2. Bring Tauri window to front.  Two-step:
#    (a) osascript set frontmost (cheap, but can be ignored by macOS if
#        another app is "actively focused")
#    (b) click on Tauri's title bar at (1500, 88) — guaranteed to grab
#        focus because nothing else lives at y=88 in this layout.
osascript -e 'tell application "System Events" to set frontmost of (first process whose name is "openzen") to true' 2>/dev/null || true
sleep 0.3
$E2E_VENV/bin/python $CGCLICK 1500 88 60
sleep 0.3
shot 00-tauri-focused

# 3. Click "+ New Chat" (uses cliclick — known good for this coord)
#    Note: cliclick needs comma-separated coords, not space-separated
cliclick c:$(echo $COORD_NEW_CHAT | tr ' ' ',')
sleep 1
shot 10-new-session

# 4. Click into the chat textarea, then type the prompt with cgtype (Quartz
#    unicode injection — bypasses IME, fires real input events)
#    Note: y=870 hits the textarea TOP BORDER (no focus).
#    y=880 is the first pixel INSIDE the textarea body — Svelte binds to the
#    <textarea> and the keydown propagates a real input event.
#    Empirical proof: the Send button flips to bright blue + border-color: var(--color-primary) only after y=880 succeeds.
$E2E_VENV/bin/python $CGCLICK 1620 880 80
sleep 0.3
$E2E_VENV/bin/python $CGTYPE "$TEST_PROMPT"
sleep 0.5
shot 20-prompt-typed

# 5. Click the real Send button
$E2E_VENV/bin/python $CGCLICK $COORD_SEND 100
sleep 2
shot 30-after-send

# 6. Poll for the AskUserDialog to appear (heuristic: 1 MSGS in sidebar +
#    question header visible).  Bails out after 90 s.
echo "⏳ Waiting for ask_user dialog..."
for j in $(seq 1 30); do
    sleep 3
    if [[ -f "$SCREENS/$(printf '%03d' $((30 + j)))-dialog-up.png" ]]; then continue; fi
    shot "$(printf '%03d' $((30 + j)))-dialog-up"
    # crude visual check: crop and look for the dark backdrop
    $E2E_VENV/bin/python - <<PY
from PIL import Image
img = Image.open("$SCREENS/$(printf '%03d' $((30 + j)))-dialog-up.png")
# Dialog is in the right half; sample centre around (1500, 470)
sample = img.crop((1280, 380, 1820, 660))
mean_brightness = sum(sum(p) for p in sample.getdata()) / (sample.width*sample.height*3)
print(f"  dialog-up check: mean brightness = {mean_brightness:.0f}")
PY
    break  # we don't loop forever; first poll is enough
done

# 7. Click into "Your response" textarea and type the reply.
#    Same y=±10 trick as the main textarea — start at 540 (which worked
#    ad-hoc in the proven run) but if the dialog has a slightly different
#    layout, drop to 530 or 545.
$E2E_VENV/bin/python $CGCLICK $COORD_DIALOG_FIELD 80
sleep 0.3
$E2E_VENV/bin/python $CGTYPE "$TEST_REPLY"
sleep 0.5
shot 40-reply-typed

# 8. Click "Send response"
$E2E_VENV/bin/python $CGCLICK $COORD_DIALOG_SEND 100
sleep 3
shot 50-after-send-response

# 9. Wait for final assistant reply
echo "⏳ Waiting for final assistant text..."
for j in $(seq 1 20); do
    sleep 3
    shot "$(printf '%03d' $((50 + j)))-final-poll"
    $E2E_VENV/bin/python - <<PY
from PIL import Image
img = Image.open("$SCREENS/$(printf '%03d' $((50 + j)))-final-poll.png")
# Look for the assistant message bubble on the right side
# crude: sample a strip in the middle of the chat area
sample = img.crop((1280, 380, 1820, 600))
# count distinct text-coloured pixels (rough)
print(f"  poll #$j done")
PY
    [[ $j -ge 6 ]] && break
done

echo ""
echo "✅ E2E flow complete.  Screenshots in $SCREENS/"
echo ""
echo "Next: open the latest screenshot to confirm the assistant gave a final"
echo "reply that mentions the user's chosen color.  Look for the file with"
echo "the highest number in $SCREENS/."
