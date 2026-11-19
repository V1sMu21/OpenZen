#!/usr/bin/env bash
# Switch the active macOS input source (e.g. "ABC").
#
# ⚠️ 2026-11-14: Cmd+Ctrl+Space opens the Character Viewer when only ONE input
# source is enabled — the old version of this script did that unconditionally
# and every typed character went into the emoji search box. This version reads
# the enabled sources first and no-ops when there is nothing to switch.
#
# Usage: switch_input_source.sh ABC
set -euo pipefail
TARGET="${1:-ABC}"

info=$(python3 - "$TARGET" <<'PY'
import plistlib, os, sys
p = os.path.expanduser("~/Library/Preferences/com.apple.HIToolbox.plist")
try:
    d = plistlib.load(open(p, "rb"))
except Exception:
    print("unknown unknown 0"); raise SystemExit
def name(s):
    return s.get("KeyboardLayout Name") or s.get("Input Source Kind") or ""
enabled = [name(s) for s in d.get("AppleEnabledInputSources", [])]
active = [name(s) for s in d.get("AppleSelectedInputSources", [])]
print((active[0] if active else "unknown"), ("|".join(enabled) or "unknown"), len(enabled))
PY
)
read -r CURRENT ENABLED COUNT <<<"$info"

if [[ "$CURRENT" == "$TARGET" ]]; then
  echo "✅ input source already $TARGET"
  exit 0
fi
if [[ "$COUNT" -le 1 ]]; then
  echo "⚠️  only one input source enabled ($ENABLED); not cycling (Cmd+Ctrl+Space would open the emoji panel)" >&2
  exit 0
fi

for _ in 1 2 3 4 5 6 7 8; do
  osascript -e 'tell application "System Events" to keystroke " " using {command down, control down}' >/dev/null 2>&1 || true
  sleep 0.3
  CURRENT=$(python3 - <<'PY'
import plistlib, os
p = os.path.expanduser("~/Library/Preferences/com.apple.HIToolbox.plist")
d = plistlib.load(open(p, "rb"))
a = d.get("AppleSelectedInputSources", [])
print((a[0].get("KeyboardLayout Name") or a[0].get("Input Source Kind") or "unknown") if a else "unknown")
PY
)
  [[ "$CURRENT" == "$TARGET" ]] && { echo "✅ input source = $CURRENT"; exit 0; }
done
echo "❌ could not switch to $TARGET (current: $CURRENT)" >&2
exit 1
