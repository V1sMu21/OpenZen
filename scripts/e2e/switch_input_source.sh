#!/usr/bin/env bash
# scripts/e2e/switch_input_source.sh
#
# Switch the active macOS input source (e.g. "ABC" for English, "Pinyin" for
# Chinese Pinyin IME).  Required before typing into the Tauri webview because
# Quartz unicode injection still routes through the active input source's
# keyboard layout — Pinyin IME will mangle the typed string into candidates.
#
# Usage: switch_input_source.sh ABC
#        switch_input_source.sh "搜狗拼音"

set -euo pipefail
TARGET="${1:-ABC}"

osascript <<OSA
tell application "System Events"
    set currentSource to name of current input source
    if currentSource is "$TARGET" then
        return "already $TARGET"
    end if
end tell
OSA

# Cycle through input sources until we land on the target.  The most reliable
# way on macOS is Cmd+Ctrl+Space which is the "select next input source"
# shortcut (note: this is the OPPOSITE of Spotlight's Cmd+Space).
for i in 1 2 3 4 5 6 7 8; do
    src=$(osascript -e 'tell application "System Events" to return name of current input source' 2>/dev/null || echo unknown)
    [[ "$src" == "$TARGET" ]] && { echo "✅ input source = $src"; exit 0; }
    osascript -e 'tell application "System Events" to keystroke " " using {command down, control down}' >/dev/null 2>&1 || true
    sleep 0.3
done

# Fallback: try setInputSourceLayout via the System Events API
osascript <<OSA || true
tell application "System Events"
    set inputSources to name of every input source
    if "$TARGET" is in inputSources then
        -- no direct API; cycle one more time
        keystroke " " using {command down, control down}
    end if
end tell
OSA
sleep 0.5
src=$(osascript -e 'tell application "System Events" to return name of current input source' 2>/dev/null || echo unknown)
echo "ℹ️  final input source = $src (target was $TARGET)"
