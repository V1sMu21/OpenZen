#!/usr/bin/env bash
# Install a project skill into the runtime skill directory.
#   bash scripts/install-skill.sh release-gate
#   OPENZEN_DATA_DIR=/tmp/dev bash scripts/install-skill.sh release-gate
set -euo pipefail
cd "$(dirname "$0")/.."
NAME="${1:?usage: install-skill.sh <skill-name>}"
SRC=".opencode/skills/$NAME/SKILL.md"
DATA="${OPENZEN_DATA_DIR:-$HOME/.openzen}"
DST="$DATA/.skill_mcp/skills/$NAME"
[ -f "$SRC" ] || { echo "missing $SRC" >&2; exit 2; }
mkdir -p "$DST"
cp "$SRC" "$DST/SKILL.md"
echo "installed $NAME → $DST/SKILL.md"
echo "OpenZen reloads skills on the next session; no app restart needed."
