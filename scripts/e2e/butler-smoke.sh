#!/usr/bin/env bash
# Butler smoke gate: mechanical gates + the three butler test suites.
# Usage: bash scripts/e2e/butler-smoke.sh
set -euo pipefail
cd "$(dirname "$0")/../.."

echo "== 1/4 mechanical gates =="
bash scripts/verify_gates.sh --base HEAD

echo "== 2/4 oz-steward butler unit + e2e =="
cargo test -p oz-steward --lib butler
cargo test -p oz-steward --test butler_e2e

echo "== 3/4 tauri butler runtime =="
cargo test -p openzen-tauri --lib butler

echo "== 4/4 frontend type check =="
cd frontends && npm run check

echo "OK: butler smoke passed"
