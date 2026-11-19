#!/usr/bin/env bash
# OpenZen release gate — one pass. The fix loop lives in the skill.
#
#   bash scripts/release-gate.sh --version 0.2.6
#
# Exit: 0 sign-off · 1 P0/P1 · 2 not releasable (dirty/version) · 3 infra
set -uo pipefail
cd "$(dirname "$0")/.."

OUT="target/release-gate"
mkdir -p "$OUT"
STATUS="$OUT/status.tsv"
: >"$STATUS"
VERSION=""
ALLOW_DIRTY=0
SKIP_FULL=0
SKIP_UX=0
SKIP_E2E=0
BUNDLE=0
ENV_LIMITED=""

while [ $# -gt 0 ]; do
  case "$1" in
    --version) [ $# -ge 2 ] || { echo "--version needs a value" >&2; exit 2; }; VERSION="$2"; shift 2 ;;
    --allow-dirty) ALLOW_DIRTY=1; shift ;;
    --skip-full) SKIP_FULL=1; shift ;;
    --skip-ux) SKIP_UX=1; shift ;;
    --skip-e2e) SKIP_E2E=1; shift ;;
    --bundle) BUNDLE=1; shift ;;
    *) echo "unknown flag $1" >&2; exit 2 ;;
  esac
done

if [ -z "$VERSION" ]; then
  VERSION=$(python3 - <<'PY'
import json
print(json.load(open("src-tauri/tauri.conf.json"))["version"])
PY
)
fi

SHA=$(git rev-parse --short HEAD 2>/dev/null || echo unknown)
BRANCH=$(git rev-parse --abbrev-ref HEAD 2>/dev/null || echo unknown)
DIRTY=$(git status --short | wc -l | tr -d ' ')
DISK_GB=$(df -g . 2>/dev/null | tail -1 | awk '{print $4+0}')
PKG_VERSION=$(python3 -c 'import json;print(json.load(open("frontends/package.json"))["version"])' 2>/dev/null || echo "?")

echo "release-gate: v$VERSION @ $SHA ($BRANCH), dirty=$DIRTY, disk=${DISK_GB}GB"

python3 - "$OUT" "$VERSION" "$SHA" "$BRANCH" "$DIRTY" "$DISK_GB" "$PKG_VERSION" <<'PY'
import json, sys, datetime
out, version, sha, branch, dirty, disk, pkg = sys.argv[1:]
json.dump({
    "version": version, "sha": sha, "branch": branch, "dirty": int(dirty),
    "disk_gb": int(disk or 0), "package_version": pkg,
    "started_at": datetime.datetime.now().astimezone().isoformat(timespec="seconds"),
}, open(f"{out}/meta.json", "w"), indent=2)
PY

run() {
  local phase="$1" name="$2"; shift 2
  local log="$OUT/${phase}-${name}.log"
  local t0 t1 status detail
  t0=$(python3 -c 'import time;print(time.time())')
  echo "── [$phase] $name"
  if "$@" >"$log" 2>&1; then status=PASS; else status=FAIL; fi
  t1=$(python3 -c 'import time;print(time.time())')
  detail=$(tail -n 3 "$log" | tr '\n' ' ' | tr '\t' ' ' | cut -c1-220)
  printf '%s\t%s\t%s\t%.1f\t%s\n' "$phase" "$name" "$status" "$(python3 -c "print($t1-$t0)")" "$detail" >>"$STATUS"
  [ "$status" = FAIL ] && tail -n 12 "$log" | sed 's/^/    /'
  return 0
}

skip() {
  printf '%s\t%s\tSKIP\t0\t%s\n' "$1" "$2" "$3" >>"$STATUS"
  echo "── [$1] $2 — SKIP ($3)"
}

# ── R0 preflight ─────────────────────────────────────────────────────
if [ "$DIRTY" -gt 0 ] && [ "$ALLOW_DIRTY" -eq 0 ]; then
  printf 'R0\tdirty-tree\tFAIL\t0\t%s uncommitted change(s)\n' "$DIRTY" >>"$STATUS"
else
  printf 'R0\tdirty-tree\tPASS\t0\t%s change(s)\n' "$DIRTY" >>"$STATUS"
fi
if [ "$PKG_VERSION" != "$VERSION" ]; then
  printf 'R0\tversion-sync\tFAIL\t0\ttauri %s != package %s\n' "$VERSION" "$PKG_VERSION" >>"$STATUS"
else
  printf 'R0\tversion-sync\tPASS\t0\t%s\n' "$VERSION" >>"$STATUS"
fi

if ! ps -p $$ >/dev/null 2>&1; then
  ENV_LIMITED="ps/pgrep blocked by sandbox"
fi

# ── R1 static ────────────────────────────────────────────────────────
run R1 fmt cargo fmt --all -- --check
run R1 check cargo check --workspace --all-targets
run R1 clippy cargo clippy --workspace -- -D warnings
run R1 gates bash scripts/verify_gates.sh --base HEAD --quiet

# ── R2 trusted tests ─────────────────────────────────────────────────
run R2 casegen-check cargo run -q -p oz-testkit --bin oz-casegen -- check
run R2 ratchet python3 scripts/caselab/ratchet.py
if [ "$SKIP_FULL" -eq 1 ]; then
  skip R2 workspace-tests "--skip-full"
else
  if [ -n "$ENV_LIMITED" ]; then
    run R2 workspace-tests cargo test --workspace --no-fail-fast -- --test-threads=1 \
      --skip test_timeout_keeps_partial_output_and_kills_process_tree
  else
    run R2 workspace-tests cargo test --workspace --no-fail-fast -- --test-threads=1
  fi
fi
run R2 ecf-nightly cargo run -q -p oz-testkit --bin oz-caselab -- run --tier nightly
run R2 mutation python3 scripts/caselab/mutation_probe.py
if [ "$SKIP_E2E" -eq 1 ]; then
  skip R2 e2e "--skip-e2e"
else
  run R2 e2e bash scripts/e2e/matrix.sh --replay
fi
run R2 merge-release cargo run -q -p oz-testkit --bin oz-caselab -- run --tier release

# ── R2.5 complex scenarios (mandatory) ───────────────────────────────
# Primitive cases prove the pieces; these prove the task graph behaves:
# content propagation, failure blocking, restart, retry, approval, concurrency.
run R2.5 scenario-ratchet python3 scripts/scenarios/check.py
run R2.5 scenario-suite cargo test -p oz-steward --test scenario_suite -- --test-threads=1
run R2.5 scenario-goals cargo test -p oz-steward --test goal_fault_injection -- --test-threads=1
run R2.5 scenario-steward cargo test -p oz-steward --test steward_fault_injection -- --test-threads=1
run R2.5 scenario-subagent cargo test -p openzen-tauri --lib cancel_tests -- --test-threads=1
run R2.5 scenario-compaction cargo test -p oz-core --lib scenario_compaction_tests -- --test-threads=1
run R2.5 scenario-duplicate-goal cargo test -p oz-tools duplicate_open_goal_is_rejected
run R2.5 scenario-verify-commands cargo test -p oz-tools verify_commands_make_a_dependent_task_strict
run R2.5 scenario-custom-verify cargo test -p oz-tools custom_verify_command_is_rejected_at_creation

# ── R3 UX budgets ────────────────────────────────────────────────────
if [ "$SKIP_UX" -eq 1 ]; then
  skip R3 ux-budget "--skip-ux"
else
  run R3 ux-budget python3 scripts/release-gate/perf.py --seed-mb 10 \
    --json "$OUT/perf.json" --bin target/debug/openzen
fi

# ── R4 bundle (optional but required for signing) ────────────────────
if [ "$BUNDLE" -eq 1 ]; then
  run R4 bundle cargo tauri build --bundles app
else
  skip R4 bundle "--bundle not set (manual Tauri build still required)"
fi

# ── report ───────────────────────────────────────────────────────────
python3 scripts/release-gate/report.py --dir "$OUT" --version "$VERSION" \
  --sha "$SHA" --branch "$BRANCH" --dirty "$DIRTY" --env-limited "$ENV_LIMITED"

FAILED=$(awk -F'\t' '$3=="FAIL"{n++} END{print n+0}' "$STATUS")
if [ "$FAILED" -gt 0 ]; then exit 1; fi
if [ "$DIRTY" -gt 0 ] && [ "$ALLOW_DIRTY" -eq 0 ]; then exit 2; fi
exit 0
