#!/usr/bin/env bash
# Isolated end-to-end run of the OpenZen agent — headless, no GUI, no user data.
#
# Why this exists (2026-10-23): unit tests with real bytes caught the tool-arg
# leaks, but the acceptance-gate bugs (a policy-refused [verify] line counted as
# a failure; `not_run` instead of `synthesis_failed`) were only visible in a real
# run. This script runs one real task through the actual agent loop with:
#   * a throwaway data root      (never ~/.openzen)
#   * a temporary `openzen serve` on a free port
#   * a scripted approver        (answers the approval gate automatically)
# and then prints the run's own quality record, so you can judge the OUTCOME
# instead of guessing from logs.
#
# Usage:
#   bash scripts/e2e/isolated-agent-run.sh                  # default task, cloud model
#   MODEL=Agnes_2_5_Flash TASK="..." bash scripts/e2e/isolated-agent-run.sh
#   MODEL=GLM-5.3-FLASH bash scripts/e2e/isolated-agent-run.sh   # a local model
#
# Exit code: 0 when the session reached a terminal status (idle) — the script
# prints the recorded assertion/review status, which is the actual result.
#
# Gotchas baked in:
#   * NEVER use plain /tmp as the data root: it is a symlink to /private/tmp, so
#     the safety layer's canonicalize check rejects every write in the workspace
#     ("cannot canonicalize path, rejecting"). Use /private/tmp.
#   * `mykey.toml` is symlinked, not copied — no secret is duplicated anywhere.
#   * Port 8000 is reserved for the oMLX model server (see AGENTS.md).
set -uo pipefail

REPO="$(cd "$(dirname "$0")/../.." && pwd)"
ROOT="${E2E_ROOT:-/private/tmp/ozrun}"
PORT="${E2E_PORT:-18610}"
TASK="${TASK:-在 work/e2e 目录下创建 hello.py，文件内容只有一行：print('e2e-ok')。然后用 python3 运行 work/e2e/hello.py 确认输出是 e2e-ok。最后用一句话回复结论。}"
MODEL="${MODEL:-Agnes_2_5_Flash}"
KEY_SRC="${KEY_SRC:-$HOME/.openzen/mykey.toml}"
LOG="$ROOT/serve.log"
TIMEOUT_SECS="${TIMEOUT_SECS:-600}"

echo "== building the CLI =="
cd "$REPO" || exit 1
cargo build --bin openzen > "$ROOT.build.log" 2>&1 \
  || { echo "build failed:"; tail -20 "$ROOT.build.log"; exit 1; }

rm -rf "$ROOT"
mkdir -p "$ROOT/workspace" "$ROOT/openzen" "$ROOT/logs"
ln -sfn "$KEY_SRC" "$ROOT/mykey.toml"

echo "== starting the isolated backend on :$PORT (data root $ROOT) =="
"$REPO/target/debug/openzen" --config "$ROOT/mykey.toml" --dir "$ROOT" \
  --assets "$REPO/assets" serve --port "$PORT" > "$LOG" 2>&1 &
SERVER_PID=$!
trap 'kill "$SERVER_PID" 2>/dev/null; kill "$APPROVER_PID" 2>/dev/null' EXIT

for _ in $(seq 1 30); do
  grep -q "listening on" "$LOG" && break
  sleep 1
done
TOKEN="$(grep -a 'Generated random auth token' "$LOG" | tail -1 | awk '{print $NF}')"
[ -n "$TOKEN" ] || { echo "no auth token; log:"; tail -5 "$LOG"; exit 1; }

# Auto-approver: the headless server has no UI to answer approval requests or
# ask_user questions, so the run would stall. File/command tools are
# auto-approved; desktop-control tools (`computer_*`) are denied — see SAFE.
SID="e2e-$(date +%H%M%S)"
python3 - "$TOKEN" "$LOG" "$PORT" "$SID" <<'PY' &
import json, os, re, subprocess, sys, time

token, log, port = sys.argv[1], sys.argv[2], sys.argv[3]
SAFE = (
    "code_run", "write", "read", "edit", "patch", "glob", "grep", "ls",
    "todowrite", "todoupdate", "skill_mcp_search", "skill_mcp_store",
    "long_term", "harness_refine", "open_side_panel", "schedule_reminder",
)
# Desktop control is always denied: the 2026-10-23 verification run used 6
# clicks, 3 key presses and 14 screenshots against the real desktop, and
# auto-approving those means a test types into whatever window is focused.
# Extra denials for boundary tests: E2E_DENY=write,code_run
DENY = {"computer_click", "computer_key", "computer_screenshot",
        "computer_scroll", "computer_drag", "computer_move"}
DENY |= {t for t in os.environ.get("E2E_DENY", "").split(",") if t}
sid_target = sys.argv[4]
answer = os.environ.get(
    "E2E_ANSWER",
    "按你的判断继续，但不要写工作区外的文件；如果做不了就直接结束并说明原因。",
)
answered = set()
seen, deadline = set(), time.time() + 1800
while time.time() < deadline:
    try:
        txt = open(log, encoding="utf-8", errors="replace").read()
    except FileNotFoundError:
        txt = ""
    # The tool field may contain spaces — a path approval logs
    # "tool=write/outside workspace: /path" — so match up to ", id=".
    for m in re.finditer(r"approval_needed sent: session=(\S+), tool=(.+), id=([0-9a-f-]{36})", txt):
        sid, tool, rid = m.groups()
        if rid in seen:
            continue
        seen.add(rid)
        name = tool.split("/", 1)[0]
        if name in DENY:
            decision = "deny"
        elif name in SAFE:
            decision = "allow"
        else:
            decision = "deny"
        out = subprocess.run(
            ["curl", "-s", "-m", "10", "-X", "POST",
             f"http://127.0.0.1:{port}/api/sessions/{sid}/approve",
             "-H", f"Authorization: Bearer {token}", "-H", "Content-Type: application/json",
             "-d", json.dumps({"request_id": rid, "decision": decision})],
            capture_output=True, text=True).stdout
        print(f"[approver] {tool} {rid} -> {decision} {out.strip()}", flush=True)
    # Answer ask_user questions too. The app waits FOREVER for an answer
    # (`ask_user_timeout_secs = 0`), so a run that asks the user would sit in
    # `running` for good and the harness would report a false stall — which is
    # exactly what the 2026-10-23 deny-path run looked like.
    try:
        page = json.loads(subprocess.run(
            ["curl", "-s", "-m", "10", f"http://127.0.0.1:{port}/api/sessions/{sid_target}",
             "-H", f"Authorization: Bearer {token}"],
            capture_output=True, text=True).stdout or "{}")
    except Exception:
        page = {}
    pending = page.get("pending_ask_user") or {}
    qid = pending.get("tool_use_id")
    if qid and qid not in answered:
        answered.add(qid)
        out = subprocess.run(
            ["curl", "-s", "-m", "10", "-X", "POST",
             f"http://127.0.0.1:{port}/api/sessions/{sid_target}/ask_user_response",
             "-H", f"Authorization: Bearer {token}", "-H", "Content-Type: application/json",
             "-d", json.dumps({"response": answer, "tool_use_id": qid})],
            capture_output=True, text=True).stdout
        print(f"[approver] ask_user {qid} -> {answer!r} {out.strip()}", flush=True)
    time.sleep(2)
PY
APPROVER_PID=$!

echo "== task sent (session $SID, model $MODEL) =="
curl -s -m "$TIMEOUT_SECS" -X POST "http://127.0.0.1:$PORT/api/chat" \
  -H "Authorization: Bearer $TOKEN" -H 'Content-Type: application/json' \
  -d "$(python3 -c 'import json,sys; print(json.dumps({"message":sys.argv[1],"session_id":sys.argv[2],"model_name":sys.argv[3]}))' "$TASK" "$SID" "$MODEL")" \
  > "$ROOT/chat.out" 2>&1 &

STATUS=""
for _ in $(seq 1 "$TIMEOUT_SECS"); do
  STATUS=$(curl -s -m 10 "http://127.0.0.1:$PORT/api/sessions/$SID" \
    -H "Authorization: Bearer $TOKEN" \
    | python3 -c 'import json,sys; print(json.load(sys.stdin).get("status",""))' 2>/dev/null)
  [ "$STATUS" = "idle" ] && break
  sleep 1
done

echo
echo "== result =="
echo "session status : ${STATUS:-unknown}"
curl -s -m 10 "http://127.0.0.1:$PORT/api/sessions/$SID" -H "Authorization: Bearer $TOKEN" \
  | python3 -c '
import json, sys
d = json.load(sys.stdin)
for m in d.get("messages") or []:
    if m.get("role") == "assistant":
        print("exit reason    :", m.get("exitReason"))
        print("reply:")
        print((m.get("content") or "")[:1500])
'
echo
echo "== quality record (the run's own verdict) =="
cat "$ROOT"/.openzen/*.jsonl 2>/dev/null | grep run_quality | tail -1
echo
echo "== deliverables =="
find "$ROOT/work" -type f 2>/dev/null | head -10
echo
echo "== gate lines =="
grep -aE "\[quality\]|\[tools\] refusing|assertion" "$LOG" | tail -12
[ "$STATUS" = "idle" ] || { echo; echo "!! session did not reach idle within ${TIMEOUT_SECS}s"; exit 1; }
