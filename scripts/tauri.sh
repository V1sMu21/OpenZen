#!/usr/bin/env bash
# ──────────────────────────────────────────────────────────
# OpenZen Tauri 快速启动（后台模式）
# 用法: bash scripts/tauri.sh
#      OPENZEN_PROFILE=dev bash scripts/tauri.sh       # 隔离数据根（P1-6）
#
# 与 tauri-dev.sh 的区别：
#   - Tauri 在后台运行，不受 shell 超时影响
#   - 输出重定向到 /tmp/openzen-tauri.log
#   - 脚本执行完即返回，app 继续运行
# ──────────────────────────────────────────────────────────
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
cd "$ROOT"

echo "==> OpenZen Tauri Launcher (background)"
echo "    root: $ROOT"

# ── 1. 前端依赖 ──────────────────────────────────────────
if [ ! -d "frontends/node_modules" ]; then
  echo "==> Installing frontend dependencies..."
  cd frontends && npm install && cd "$ROOT"
fi

# ── 2. 清理残留进程 ──────────────────────────────────────
echo "==> Killing stale Vite..."
pkill -f "vite.*5173" 2>/dev/null || true
sleep 1

# ── 3. 启动 Vite dev server（后台） ───────────────────────
echo "==> Starting Vite on port 5173..."
cd frontends
nohup npx vite --port 5173 --host 127.0.0.1 > /tmp/openzen-vite.log 2>&1 &
VITE_PID=$!
cd "$ROOT"

# ── 4. 等待 Vite 就绪 ────────────────────────────────────
echo "==> Waiting for Vite..."
for i in $(seq 1 30); do
  if curl -s http://127.0.0.1:5173 > /dev/null 2>&1; then
    echo "    Vite ready (attempt $i)"
    break
  fi
  sleep 1
done

# ── 5. 启动 Tauri（后台，不受 shell 退出影响） ───────────
echo "==> Launching Tauri in background..."
nohup cargo tauri dev > /tmp/openzen-tauri.log 2>&1 &
TAURI_PID=$!
disown $TAURI_PID

echo "==> Tauri launching (PID $TAURI_PID)"
echo "    Logs: tail -f /tmp/openzen-tauri.log"
echo "    Vite logs: tail -f /tmp/openzen-vite.log"
echo "    To stop: pkill -f openzen-tauri"

# ── 6. 等待 Tauri 编译完成并启动 ─────────────────────────
echo "==> Waiting for Tauri to start..."
for i in $(seq 1 60); do
  if pgrep -f "openzen-tauri" | grep -v grep > /dev/null 2>&1; then
    echo "    Tauri process detected (attempt $i)"
    break
  fi
  sleep 2
done

echo "==> Done. App should be visible on your desktop."
