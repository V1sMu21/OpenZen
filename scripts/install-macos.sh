#!/usr/bin/env bash
# OpenZen macOS 构建 + 安装脚本
#
# 用法:
#   bash scripts/install-macos.sh              # 只构建+校验；OpenZen 运行中则跳过安装（默认）
#   bash scripts/install-macos.sh --restart    # 允许退出→安装→重启（需用户显式同意）
#
# ⛔ 硬性规则（见 AGENTS.md）：绝不关闭用户正在使用的 OpenZen。
#   所以默认模式检测到 OpenZen 在运行就只构建、不安装、不重启。
#
# 为什么必须走这个脚本而不是手动拷二进制：
#   1. 必须 `cargo tauri build`，beforeBuildCommand 会把 frontends/dist 嵌进二进制；
#      裸 cargo build 后只拷二进制会白屏（项目 skill 硬性规则）。
#   2. 必须整体替换 .app，签名才覆盖整个 bundle。
#   3. 本项目用稳定的自签名身份 "OpenZen Local Dev" 签名，避免重建后 macOS TCC
#      授权（屏幕录制/辅助功能）失效。详见 scripts/setup-signing.sh。
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
cd "$ROOT"

APP_NAME="OpenZen"
DEST="/Applications/${APP_NAME}.app"
BUILT="target/release/bundle/macos/${APP_NAME}.app"
SIGN_ID="OpenZen Local Dev"
PROC_PATTERN="/Applications/${APP_NAME}.app/Contents/MacOS/"

RESTART=0
if [[ "${1:-}" == "--restart" ]]; then RESTART=1; fi

GREEN='\033[0;32m'; YELLOW='\033[1;33m'; RED='\033[0;31m'; NC='\033[0m'
log()  { echo -e "${GREEN}==>${NC} $*"; }
warn() { echo -e "${YELLOW}!! ${NC}$*"; }
die()  { echo -e "${RED}!! ${NC}$*" >&2; exit 1; }

# ── 0. 前置：签名身份必须存在 ───────────────────────────────
security find-identity -v -p codesigning 2>/dev/null | grep -q "$SIGN_ID" \
  || die "钥匙串中找不到签名身份 '${SIGN_ID}'。请先运行: bash scripts/setup-signing.sh"
log "签名身份就绪: ${SIGN_ID}"

# ── 1. 记录 OpenZen 是否在运行（不主动关闭它） ──────────────
RUNNING_PIDS="$(pgrep -f "$PROC_PATTERN" | tr '\n' ' ' || true)"
RUNNING=0
if [[ -n "${RUNNING_PIDS// /}" ]]; then RUNNING=1; fi

# ── 2. 构建（本地注入签名身份，不写进 tauri.conf.json） ─────
# 为什么不写进 tauri.conf.json：CI（.github/workflows/release.yml 的
# macos-latest runner）没有这个本地证书，硬编码会让发布构建失败。
# 用 APPLE_SIGNING_IDENTITY 环境变量只影响本机构建，CI 行为保持不变。
log "构建: cargo tauri build --bundles app（签名身份: ${SIGN_ID}）"
APPLE_SIGNING_IDENTITY="$SIGN_ID" cargo tauri build --bundles app \
  --config '{"bundle":{"macOS":{"hardenedRuntime":false}}}'
[[ -d "$BUILT" ]] || die "未找到构建产物: $BUILT"

# ── 3. 校验签名（身份 + DR 必须基于证书，否则 TCC 授权还会失效） ──
log "校验签名"
codesign --verify --deep --strict "$BUILT" || die "签名校验失败"
DR="$(codesign -d -r- "$BUILT" 2>&1 | grep 'designated' || true)"
echo "    ${DR:-<无 designated requirement>}"
echo "$DR" | grep -q 'certificate root' \
  || die "DR 不是基于证书身份（签名未生效？），TCC 授权会随重建失效。请检查签名配置。"

# ── 4. 安装决策 ─────────────────────────────────────────────
if [[ $RUNNING -eq 1 && $RESTART -eq 0 ]]; then
  warn "检测到 OpenZen 正在运行（PID: ${RUNNING_PIDS}）—— 按硬性规则不会关闭它，已跳过安装。"
  echo "    构建产物已就绪（已签名、已校验）："
  echo "      $BUILT"
  echo "    待你方便时二选一："
  echo "      1) 自己退出 OpenZen 后重新运行本脚本 → 自动安装并启动"
  echo "      2) 运行: bash scripts/install-macos.sh --restart  → 允许脚本退出并重启它"
  exit 0
fi

if [[ $RUNNING -eq 1 ]]; then
  log "收到 --restart：退出 OpenZen（PID: ${RUNNING_PIDS}）"
  osascript -e "quit app \"${APP_NAME}\"" >/dev/null 2>&1 || true
  for _ in $(seq 1 20); do
    pgrep -f "$PROC_PATTERN" >/dev/null 2>&1 || break
    sleep 0.5
  done
  pkill -f "$PROC_PATTERN" >/dev/null 2>&1 || true
fi

# ── 5. 安装：整体替换，绝不只换二进制 ───────────────────────
log "安装到 ${DEST}"
rm -rf "$DEST"
cp -R "$BUILT" "$DEST"
xattr -dr com.apple.quarantine "$DEST" >/dev/null 2>&1 || true

# ── 6. 启动 ─────────────────────────────────────────────────
log "启动 ${APP_NAME}"
open "$DEST"
log "完成 ✓"
