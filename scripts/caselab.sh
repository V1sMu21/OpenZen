#!/usr/bin/env bash
# caselab.sh — one entry point for the Edge Case Factory.
#
#   bash scripts/caselab.sh --tier pr        # fast gate (< 8 min)
#   bash scripts/caselab.sh --tier nightly   # + full corpus + E2E matrix
#   bash scripts/caselab.sh --tier release   # + workspace tests + merged report
#
# Flags: --case ID | --fc FC-B2 | --no-e2e | --no-mutation
#
# Exit codes are the ECF contract: 0 pass, 1 product failure, 2 release-gate
# failure, 3 infrastructure, 4 corpus invalid.
set -uo pipefail

REPO="$(cd "$(dirname "$0")/.." && pwd)"
cd "$REPO" || exit 3

TIER="pr"
EXTRA=()
NO_E2E=0
NO_MUTATION=0
while [ $# -gt 0 ]; do
  case "$1" in
    --tier)
      [ $# -ge 2 ] || { echo "--tier needs a value" >&2; exit 4; }
      TIER="$2"; shift 2 ;;
    --case|--fc)
      [ $# -ge 2 ] || { echo "$1 needs a value" >&2; exit 4; }
      EXTRA+=("$1" "$2"); shift 2 ;;
    --no-e2e) NO_E2E=1; shift ;;
    --no-mutation) NO_MUTATION=1; shift ;;
    -h|--help) sed -n '2,12p' "$0"; exit 0 ;;
    *) echo "unknown arg: $1" >&2; exit 4 ;;
  esac
done

step() { printf '\n\033[1;36m── %s\033[0m\n' "$*"; }
fail() { printf '\033[31m%s\033[0m\n' "$*" >&2; }

step "L0 corpus check"
cargo run -q -p oz-testkit --bin oz-casegen -- check || exit 4
python3 scripts/caselab/ratchet.py || exit 4

step "L1 workspace compile"
cargo check --workspace --all-targets || exit 1

step "L2 ECF unit corpus"
cargo test -q -p oz-testkit --test cases || exit 1

step "L3 ECF runner"
cargo run -q -p oz-testkit --bin oz-caselab -- run --tier "$TIER" ${EXTRA[@]+"${EXTRA[@]}"} || exit $?

if [ "$TIER" != "pr" ]; then
  if [ "$NO_MUTATION" = 0 ]; then
    step "L4 mutation probe"
    python3 scripts/caselab/mutation_probe.py || exit $?
  fi
  if [ "$NO_E2E" = 0 ]; then
    step "L5 E2E matrix"
    bash scripts/e2e/matrix.sh || exit $?
    step "L6 merged report (includes E2E)"
    cargo run -q -p oz-testkit --bin oz-caselab -- run --tier release || exit $?
  fi
fi

if [ "$TIER" = "release" ]; then
  step "L7 workspace tests"
  cargo test --workspace -- --test-threads=1 || exit 1
  step "L8 frontend"
  ( cd frontends && npm run check && npm run build ) || exit 1
fi

step "done ($TIER)"
