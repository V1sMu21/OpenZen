#!/usr/bin/env bash
# One-click: verify → build → install OpenZen.
#
# Default is SAFE: if OpenZen is running, scripts/install-macos.sh builds and
# refuses to replace the running app (AGENTS.md hard rule).
#   bash scripts/steward-oneclick.sh              # verify + build + install if idle
#   bash scripts/steward-oneclick.sh --restart    # explicit: quit/install/relaunch
#   bash scripts/steward-oneclick.sh --no-checks  # skip tests (build only)
set -euo pipefail
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
cd "$ROOT"

RESTART=0
CHECKS=1
for arg in "$@"; do
  case "$arg" in
    --restart) RESTART=1 ;;
    --no-checks) CHECKS=0 ;;
    *) echo "unknown argument: $arg" >&2; exit 2 ;;
  esac
done

if [[ "$CHECKS" -eq 1 ]]; then
  echo "==> 1/4 format"
  cargo fmt --all -- --check
  echo "==> 2/4 check"
  cargo check --workspace --all-targets
  echo "==> 3/4 steward tests"
  cargo test -p oz-core-types -p oz-steward -p openzen-tauri -- --test-threads=1
  echo "==> 4/4 10-scenario fault injection"
  bash scripts/e2e/steward-fault-injection.sh
fi

if ! security find-identity -v -p codesigning 2>/dev/null | grep -q "OpenZen Local Dev"; then
  echo "!! signing identity 'OpenZen Local Dev' missing; building unsigned .app only" >&2
  cargo tauri build --bundles app
  echo "built: target/release/bundle/macos/OpenZen.app"
  exit 0
fi

if [[ "$RESTART" -eq 1 ]]; then
  exec bash scripts/install-macos.sh --restart
else
  exec bash scripts/install-macos.sh
fi
