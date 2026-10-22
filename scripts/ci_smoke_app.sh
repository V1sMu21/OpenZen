#!/usr/bin/env bash
# ci_smoke_app.sh — 构建产物的启动冒烟（round5 P1-13⑤）
#
# 为什么需要：CI 的 build job 曾经只跑裸 `cargo build --release`，而
# AGENTS.md 明确写着**唯一正确的产物路径是 `cargo tauri build`** ——
# 裸 cargo build 不嵌入前端资源，产出的是一个白屏 app（产物比正确的小
# ~4MB）。也就是说旧 CI 会对一个"能编译但打不开"的产物给绿灯。
#
# 本脚本做两类检查：
#   A. 静态三条（任何环境都能跑）
#      1. `OpenZen.app` bundle 存在
#      2. 可执行文件存在且可执行
#      3. 前端资源已**嵌入二进制**（拿 dist 里的真实 bundle 文件名做子串匹配）
#   B. 启动冒烟（有窗口服务时才做）
#      起进程 → 存活 N 秒 → 杀掉。macOS CI runner 没有窗口服务，
#      NSApplication 会直接退出；那种情况记为 SKIP 而不是 FAIL
#      （判据：退出码/日志里出现窗口服务相关字样），否则 FAIL。
#
# 用法: bash scripts/ci_smoke_app.sh [--app <path>] [--dist <path>]
# 退出码: 0 = 通过（可含 SKIP）；1 = 有 FAIL

set -uo pipefail
cd "$(dirname "$0")/.."

APP="target/release/bundle/macos/OpenZen.app"
DIST="frontends/dist"
ALIVE_SECS=6

while [[ $# -gt 0 ]]; do
  case "$1" in
    --app)  APP="${2:-}"; shift ;;
    --dist) DIST="${2:-}"; shift ;;
    --alive) ALIVE_SECS="${2:-6}"; shift ;;
    *) echo "unknown arg: $1" >&2; exit 2 ;;
  esac
  shift
done

RED='\033[0;31m'; YELLOW='\033[1;33m'; GREEN='\033[0;32m'; NC='\033[0m'
FAIL=0; SKIP=0
pass() { echo -e "${GREEN}✓${NC} $*"; }
fail() { echo -e "${RED}✗ FAIL${NC} $*"; FAIL=$((FAIL+1)); }
skip() { echo -e "${YELLOW}· SKIP${NC} $*"; SKIP=$((SKIP+1)); }

echo "── ci_smoke_app: app=${APP} dist=${DIST}"

# ── A1. bundle 存在 ────────────────────────────────────────────────────────
if [[ -d "$APP" ]]; then
  pass "app bundle 存在：$APP"
else
  fail "app bundle 缺失：$APP（必须用 cargo tauri build 产出，裸 cargo build 不会生成）"
fi

BIN="$APP/Contents/MacOS/openzen-tauri"
# 兜底：某些配置下产物名不同，取 Contents/MacOS 下第一个可执行文件
if [[ ! -x "$BIN" && -d "$APP/Contents/MacOS" ]]; then
  BIN="$(find "$APP/Contents/MacOS" -type f -perm -u+x 2>/dev/null | head -1)"
fi

# ── A2. 可执行文件存在且可执行 ─────────────────────────────────────────────
if [[ -n "${BIN:-}" && -x "$BIN" ]]; then
  SIZE=$(stat -f%z "$BIN" 2>/dev/null || stat -c%s "$BIN" 2>/dev/null || echo 0)
  pass "可执行文件：${BIN}（$((SIZE/1024/1024)) MB）"
else
  fail "bundle 内没有可执行文件（Contents/MacOS）"
fi

# ── A3. 前端资源已嵌入二进制 ───────────────────────────────────────────────
# 判据：dist/index.html 引用的 bundle 文件名（带内容哈希，构建立即变化）
# 必须出现在二进制里。裸 cargo build 的产物里不会有它。
if [[ -f "$DIST/index.html" ]]; then
  MARKER="$(grep -oE 'assets/[A-Za-z0-9_.-]+\.js' "$DIST/index.html" | head -1)"
  if [[ -z "$MARKER" ]]; then
    skip "dist/index.html 里找不到 assets/*.js 标记（前端构建形态变了？）"
  elif [[ -z "${BIN:-}" || ! -f "$BIN" ]]; then
    fail "无法校验前端嵌入：可执行文件缺失"
  elif grep -qa -- "$MARKER" "$BIN"; then
    pass "前端资源已嵌入二进制（标记 ${MARKER}）"
  else
    # 没命中当前标记时再分两种：二进制里**一个** assets/index-* 标记都没有
    # ⇒ 前端根本没被嵌入（白屏 app，裸 cargo build 的典型症状，FAIL）；
    # 有别的标记 ⇒ 嵌的是更早一次前端构建（陈旧但能开，记 SKIP + 提示）。
    EMBEDDED="$(strings -a "$BIN" 2>/dev/null | grep -oE 'assets/index-[A-Za-z0-9_-]+\.js' | sort -u | head -3)"
    if [[ -z "$EMBEDDED" ]]; then
      fail "二进制里没有任何前端 bundle 标记 —— 这是白屏 app（裸 cargo build 不会嵌入前端资源）"
    else
      # Stale frontend = the bundle ships an OLD UI: the user sees a version
      # that does not match the code that was just built. That used to be a
      # SKIP with a green summary, so a mismatched artifact could be uploaded
      # as a release (review C9). It is a FAIL now; `cargo tauri build` always
      # rebuilds the frontend first, so a correct pipeline never hits this.
      fail "二进制嵌入的是更早的前端构建（${EMBEDDED//$'\n'/, }），当前 dist 是 ${MARKER} —— 产物与前端不一致"
      echo "       修复：先 npm run build 再 cargo tauri build（tauri 的 beforeBuildCommand 会做这件事）"
    fi
  fi
else
  fail "前端构建产物缺失：$DIST/index.html（先跑 npm run build）"
fi

# ── B. 启动冒烟 ────────────────────────────────────────────────────────────
if [[ -z "${BIN:-}" || ! -x "$BIN" ]]; then
  skip "启动冒烟：没有可执行文件"
else
  LOG="$(mktemp -t oz-smoke)"
  "$BIN" >"$LOG" 2>&1 &
  PID=$!
  slept=0
  while [[ $slept -lt $ALIVE_SECS ]]; do
    if ! kill -0 "$PID" 2>/dev/null; then break; fi
    sleep 1
    slept=$((slept+1))
  done

  if kill -0 "$PID" 2>/dev/null; then
    kill "$PID" 2>/dev/null
    wait "$PID" 2>/dev/null
    pass "启动冒烟：进程存活 ${slept}s 后被主动结束"
  else
    # 已退出：区分"没有窗口服务"（CI 常态，记 SKIP）与真实崩溃（FAIL）
    if grep -qiE "window server|NSApplication|not permitted to|no display|CGSession|XPC|connection invalid" "$LOG"; then
      skip "启动冒烟：无窗口服务（CI runner 常态）—— $(head -1 "$LOG" | cut -c1-100)"
    else
      fail "启动冒烟：进程在 ${slept}s 内退出。日志尾部："
      tail -5 "$LOG" | sed 's/^/      /'
    fi
  fi
  rm -f "$LOG"
fi

echo
if [[ $FAIL -gt 0 ]]; then
  echo -e "${RED}Summary${NC}  FAIL=${FAIL} SKIP=${SKIP}"
  exit 1
fi
if [[ $SKIP -gt 0 ]]; then
  echo -e "${YELLOW}Summary${NC}  FAIL=0 SKIP=${SKIP} —— 有检查被跳过，结论不完整（见上方 SKIP 原因）"
  exit 0
fi
echo -e "${GREEN}Summary${NC}  FAIL=0 SKIP=0 —— 产物可启动且前端已嵌入"
exit 0
