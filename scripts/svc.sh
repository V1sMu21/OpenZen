#!/bin/bash
# OpenZen service manager — start, stop, status, or tail logs.
#
# Usage:
#   ./scripts/svc.sh start   — start both services detached (nohup)
#   ./scripts/svc.sh stop    — stop both services
#   ./scripts/svc.sh status  — show running status + URLs + auth token
#   ./scripts/svc.sh logs    — tail both logs
#   ./scripts/svc.sh restart — stop then start
#   ./scripts/svc.sh token   — print current auth token (or "" if auth disabled)

set -e

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
LOGS="$ROOT/.runlogs"
mkdir -p "$LOGS"

BACK_PID_FILE="$LOGS/backend.pid"
FRONT_PID_FILE="$LOGS/frontend.pid"

is_running() {
  local pid=$(cat "$1" 2>/dev/null)
  [ -n "$pid" ] && kill -0 "$pid" 2>/dev/null
}

# Find the actual PID for a given process pattern, in case the pidfile
# is stale but the process is still alive (e.g. after a process restart
# outside of this script).
find_pid() {
  local pattern="$1"
  pgrep -f "$pattern" | head -1
}

start_one() {
  local name="$1"
  local pidfile="$2"
  local port="$3"
  local workdir="$4"
  local cmd="$5"
  if is_running "$pidfile"; then
    echo "[$name] already running (pid=$(cat "$pidfile"))"
    return 0
  fi
  echo "[$name] starting…"
  # macOS bash tool shells exit when the tool call returns, which kills
  # any child processes attached via SIGHUP. Detach via `osascript` +
  # AppleScript `do script` so the process lives under launchd (PPID=1)
  # and survives the tool call boundary.
  osascript -e "tell application \"Terminal\" to do script \"cd '$workdir' && exec $cmd\""
  sleep 2
  # Find the spawned pid by port and persist it; the AppleScript route
  # makes the pid difficult to capture directly, so we discover it after
  # the fact.
  local pid
  if [ -n "$port" ]; then
    pid=$(lsof -ti tcp:"$port" 2>/dev/null | head -1)
  else
    pid=$(find_pid "$cmd")
  fi
  if [ -n "$pid" ]; then
    echo "$pid" > "$pidfile"
  fi
  sleep 1
  if is_running "$pidfile"; then
    echo "[$name] started (pid=$(cat "$pidfile"))"
  else
    echo "[$name] FAILED to start; check $LOGS/$name.log"
    return 1
  fi
}

stop_one() {
  local name="$1"
  local pidfile="$2"
  local pattern="$3"
  local pid=$(cat "$pidfile" 2>/dev/null)
  if ! is_running "$pidfile"; then
    # pidfile stale; search by pattern as a fallback so we still kill
    # any orphan process that survived.
    pid=$(find_pid "$pattern")
  fi
  if [ -n "$pid" ] && kill -0 "$pid" 2>/dev/null; then
    echo "[$name] stopping (pid=$pid)…"
    kill "$pid" 2>/dev/null || true
    sleep 1
    if kill -0 "$pid" 2>/dev/null; then
      kill -9 "$pid" 2>/dev/null || true
    fi
    rm -f "$pidfile"
    echo "[$name] stopped"
  else
    echo "[$name] not running"
    rm -f "$pidfile"
  fi
}

cmd="${1:-status}"

case "$cmd" in
  start)
    start_one backend  "$BACK_PID_FILE"  18567 "$ROOT"          "./target/release/ga serve --port 18567"
    start_one frontend "$FRONT_PID_FILE" 5173 "$ROOT/frontends" "npm run dev"
    ;;
  stop)
    stop_one frontend "$FRONT_PID_FILE" "npm run dev"
    stop_one backend "$BACK_PID_FILE" "ga serve --port 18567"
    ;;
  restart)
    "$0" stop
    sleep 1
    "$0" start
    ;;
  status)
    echo "=== Backend (port 18567) ==="
    if is_running "$BACK_PID_FILE"; then
      echo "running (pid=$(cat "$BACK_PID_FILE"))"
      curl -s -o /dev/null -w "  health: %{http_code}\n" http://localhost:18567/api/health 2>/dev/null
    else
      echo "NOT running"
    fi
    echo "=== Frontend (port 5173) ==="
    if is_running "$FRONT_PID_FILE"; then
      echo "running (pid=$(cat "$FRONT_PID_FILE"))"
      curl -s -o /dev/null -w "  root:   %{http_code}\n" http://localhost:5173/ 2>/dev/null
    else
      echo "NOT running"
    fi
    echo "=== Logs ==="
    echo "  $LOGS/backend.log"
    echo "  $LOGS/frontend.log"
    ;;
  logs)
    echo "Tailing logs (Ctrl-C to stop)…"
    if [ -f "$LOGS/backend.log" ] && [ -f "$LOGS/frontend.log" ]; then
      tail -f "$LOGS/backend.log" "$LOGS/frontend.log"
    else
      echo "no logs yet — run '$0 start' first"
    fi
    ;;
  token)
    if curl -sf http://localhost:18567/api/health >/dev/null 2>&1; then
      curl -s http://localhost:18567/api/health | python3 -c "import json,sys; d=json.load(sys.stdin); print(d.get('auth_token', '(auth disabled)'))"
    else
      echo "backend is not running"
      exit 1
    fi
    ;;
  *)
    echo "usage: $0 {start|stop|restart|status|logs|token}"
    exit 1
    ;;
esac
