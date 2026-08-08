#!/usr/bin/env bash
# ──────────────────────────────────────────────────────────
# OpenZen Tauri 开发模式启动脚本
# 用法: bash scripts/tauri-dev.sh
#
# 步骤:
#   1. 安装前端依赖（如未安装）
#   2. 清理 Vite 残留进程
#   3. 启动 Vite dev server（后台）
#   4. 等待 Vite 就绪
#   5. 构建并启动 Tauri（使用 devUrl）
# ──────────────────────────────────────────────────────────
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
cd "$ROOT"

echo "==> OpenZen Tauri Dev Launcher"
echo "    root: $ROOT"

# ── 1. 前端依赖 ──────────────────────────────────────────
if [ ! -d "frontends/node_modules" ]; then
  echo "==> Installing frontend dependencies..."
  cd frontends && npm install && cd "$ROOT"
fi

# ── 2. 清理 Vite 残留 ────────────────────────────────────
echo "==> Killing stale Vite processes..."
pkill -f "vite.*5173" 2>/dev/null || true
sleep 1

# ── 3. 启动 Vite dev server ──────────────────────────────
echo "==> Starting Vite dev server (port 5173)..."
cd frontends
npx vite --port 5173 --host 127.0.0.1 > /tmp/openzen-vite.log 2>&1 &
VITE_PID=$!
cd "$ROOT"

# ── 4. 等待 Vite 就绪 ────────────────────────────────────
echo "==> Waiting for Vite (PID $VITE_PID)..."
for i in $(seq 1 30); do
  if curl -s http://127.0.0.1:5173 > /dev/null 2>&1; then
    echo "    Vite ready (attempt $i)"
    break
  fi
  sleep 1
done

# ── 5. 构建并启动 Tauri ─────────────────────────────────
echo "==> Building & launching Tauri (dev mode)..."
cargo tauri dev

# ── 清理 ─────────────────────────────────────────────────
echo "==> Tauri exited. Cleaning up Vite (PID $VITE_PID)..."
kill "$VITE_PID" 2>/dev/null || true
echo "==> Done."
