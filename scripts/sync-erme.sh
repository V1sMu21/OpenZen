#!/usr/bin/env bash
# ERME fork sync — track upstream changes and re-apply OpenZen's local delta.
#
# The vendored ERME (vendor/entropy-memory-engine) is a fork of the upstream
# Entropy-Reduced Memory Engine (see vendor/entropy-memory-engine/UPSTREAM.md).
# Local modifications live as openzen-delta.patch; this script detects upstream
# drift and (with --apply) refreshes the fork while replaying that patch.
#
# Usage:
#   bash scripts/sync-erme.sh          # report only: upstream drift + diff list
#   bash scripts/sync-erme.sh --apply  # refresh src from upstream + replay patch
set -euo pipefail

UPSTREAM="${ERME_UPSTREAM:-$HOME/Documents/opencode/Entropy-Reduced Memory Engine}"
VENDOR="$(cd "$(dirname "$0")/.." && pwd)/vendor/entropy-memory-engine"
MANIFEST="$VENDOR/UPSTREAM.md"
PATCH="$VENDOR/openzen-delta.patch"
BASELINE_COMMIT="74e31c8"

cd "$VENDOR"

echo "== ERME fork sync =="
echo "  upstream : $UPSTREAM"
echo "  vendored : $VENDOR"

if [ ! -d "$UPSTREAM/src" ]; then
  echo "ERROR: upstream repo not found at $UPSTREAM" >&2
  echo "  set ERME_UPSTREAM to point at the Entropy-Reduced Memory Engine checkout" >&2
  exit 1
fi

# 1) Upstream drift check (report only)
UPSTREAM_HEAD=""
if git -C "$UPSTREAM" rev-parse --is-inside-work-tree >/dev/null 2>&1; then
  UPSTREAM_HEAD="$(git -C "$UPSTREAM" rev-parse --short HEAD)"
fi
if [ -n "$UPSTREAM_HEAD" ] && [ "$UPSTREAM_HEAD" != "$BASELINE_COMMIT" ]; then
  echo "  ⚠️  upstream has NEW commits: baseline=$BASELINE_COMMIT head=$UPSTREAM_HEAD"
  echo "     review $(git -C "$UPSTREAM" log --oneline "$BASELINE_COMMIT..HEAD" 2>/dev/null | head -10)"
else
  echo "  ✓ upstream unchanged since baseline $BASELINE_COMMIT"
fi

# 2) Current diff between upstream and the vendored copy
echo
echo "  diff vs upstream (vendored files with local changes):"
DIFF_LINES="$(diff -rq "$UPSTREAM/src" "$VENDOR/src" 2>/dev/null || true)"
echo "$DIFF_LINES" | grep "^Files" | sed 's/^Files /    /; s/ and .* differ$//' | head -20
DIFF_COUNT="$(echo "$DIFF_LINES" | grep -c "^Files" || true)"
echo "  (${DIFF_COUNT:-0} files differ)"

if [ "${1:-}" != "--apply" ]; then
  echo
  echo "  dry-run: pass --apply to refresh src from upstream and replay the delta patch"
  exit 0
fi

# 3) --apply: refresh from upstream, then replay the OpenZen delta
echo
echo "== applying upstream refresh + delta replay =="
rsync -a --delete "$UPSTREAM/src/" "$VENDOR/src/"
if [ -f "$PATCH" ]; then
  echo "  replaying $PATCH"
  # Patch headers are "upstream/src/... -> vendor/src/..."; -p1 strips the
  # first component so paths resolve as src/... from inside the vendored dir.
  if ! patch -p1 -s --forward < "$PATCH"; then
    echo "  ⚠️  patch conflict — upstream changed an area OpenZen modifies." >&2
    echo "     resolve manually, then regenerate:" >&2
    echo "       diff -ruN \"$UPSTREAM/src\" \"$VENDOR/src\" > \"$PATCH\"" >&2
    echo "     and update the change list in UPSTREAM.md" >&2
    exit 2
  fi
fi

echo
echo "== verifying =="
cargo check -p entropy_memory_engine 2>/dev/null || {
  echo "  check failed — see errors above" >&2
  exit 3
}
echo "  ✓ vendor builds; remember to update UPSTREAM.md baseline if upstream moved"
