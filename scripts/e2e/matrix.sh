#!/usr/bin/env bash
# E2E matrix — see scripts/e2e/matrix.py. Builds the CLI on first use.
set -uo pipefail
REPO="$(cd "$(dirname "$0")/../.." && pwd)"
cd "$REPO" || exit 3
if [ ! -x target/debug/openzen ]; then
  echo "== building the CLI =="
  cargo build --bin openzen || exit 3
fi
exec python3 scripts/e2e/matrix.py "$@"
