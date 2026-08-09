#!/usr/bin/env bash
# OpenZen 发布脚本（方案 A：git-cliff 版本推导）
#
# 流程:
#   1. 前置检查（工作树干净 / git-cliff 存在）
#   2. 测试闸门（cargo test --workspace 全绿才继续 —— 本地预检，CI 是最终闸门）
#   3. git-cliff --bump 推导下一个版本号（feat→minor, fix→patch, breaking→major）
#   4. 同步版本号到 tauri.conf.json + frontends/package.json
#   5. 更新 CHANGELOG.md（git-cliff 自动生成新段）
#   6. cargo tauri build 产出带版本号的 dmg
#   7. git commit + tag vX.Y.Z
#   8. 提示 push（push 后 CI 的 release job 自动构建并挂到 GitHub Release）
#
# 用法:
#   scripts/release.sh            # 正常发布（推 minor/patch 取决于 commit）
#   scripts/release.sh --dry-run  # 只推导版本号 + 预览 CHANGELOG，不改任何文件
set -euo pipefail

cd "$(dirname "$0")/.."   # workspace 根

DRY_RUN=0
if [[ "${1:-}" == "--dry-run" ]]; then DRY_RUN=1; fi

GREEN='\033[0;32m'; YELLOW='\033[1;33m'; RED='\033[0;31m'; NC='\033[0m'
log()  { echo -e "${GREEN}==>${NC} $*"; }
warn() { echo -e "${YELLOW}!! ${NC} $*"; }
die()  { echo -e "${RED}!! ${NC} $*" >&2; exit 1; }

# ---------- 1. 前置检查 ----------
log "前置检查..."
command -v git-cliff >/dev/null || die "git-cliff 未安装，请先: brew install git-cliff"

if [[ $DRY_RUN -eq 0 ]]; then
  if [[ -n "$(git status --porcelain)" ]]; then
    warn "工作树有未提交变更："
    git status --short
    die "请先提交或 stash 全部变更再发布（release.sh 会自己生成 commit）"
  fi
  [[ "$(git branch --show-current)" == "main" ]] || die "发布必须在 main 分支（当前: $(git branch --show-current)）"
fi

# ---------- 2. 测试闸门 ----------
log "运行测试闸门: cargo test --workspace -- --test-threads=1"
if ! cargo test --workspace -- --test-threads=1 2>&1 | tail -5; then
  die "测试未全绿，发布中止。修复后再试。"
fi

# ---------- 3. 推导版本号 ----------
log "推导下一个版本号..."
NEW_VERSION=$(git-cliff --bumped-version 2>/dev/null || true)
if [[ -z "$NEW_VERSION" ]]; then
  warn "git-cliff 无法从 commit 推导版本（可能自上次 tag 后无 conventional commits）"
  die "请先使用 conventional commit 风格提交（feat:/fix:/refactor:/docs: 等前缀）"
fi
# 统一为 v 前缀（git-cliff 可能输出 "0.1.0" 或 "v0.1.0"）
NEW_VERSION="v${NEW_VERSION#v}"
CURRENT_TAG=$(git describe --tags --abbrev=0 2>/dev/null || echo "(无 tag)")
log "当前版本: ${CURRENT_TAG} → 下一个版本: ${NEW_VERSION}"
VERSION_NO_V="${NEW_VERSION#v}"   # v0.2.0 → 0.2.0

# ---------- 4. 同步版本号 ----------
log "同步版本号: tauri.conf.json + frontends/package.json → ${VERSION_NO_V}"
TAURI_CONF="src-tauri/tauri.conf.json"
PKG_JSON="frontends/package.json"
if ! grep -qE '"version"\s*:\s*"[0-9]+\.[0-9]+\.[0-9]+"' "$TAURI_CONF"; then
  die "tauri.conf.json 未找到 version 字段，请人工检查"
fi

if [[ $DRY_RUN -eq 0 ]]; then
  sed -i '' -E "s/\"version\"[[:space:]]*:[[:space:]]*\"[0-9]+\.[0-9]+\.[0-9]+\"/\"version\": \"${VERSION_NO_V}\"/" "$TAURI_CONF"
  sed -i '' -E "s/\"version\"[[:space:]]*:[[:space:]]*\"[0-9]+\.[0-9]+\.[0-9]+\"/\"version\": \"${VERSION_NO_V}\"/" "$PKG_JSON"
  log "已同步: $(grep -o '"version": "[^"]*"' "$TAURI_CONF" | head -1) / $(grep -o '"version": "[^"]*"' "$PKG_JSON" | head -1)"
else
  log "[dry-run] 将同步 $TAURI_CONF 与 $PKG_JSON → ${VERSION_NO_V}"
fi

# ---------- 5. 更新 CHANGELOG ----------
log "更新 CHANGELOG.md..."
if [[ $DRY_RUN -eq 0 ]]; then
  # 用 git-cliff 全量生成（含历史 tag 段），保留我们手写的 v0.1.0 前导说明
  git-cliff --output CHANGELOG.md 2>/dev/null || die "git-cliff 生成 CHANGELOG 失败"
  log "CHANGELOG.md 已更新（顶部为新版本段）"
else
  log "[dry-run] 将执行: git-cliff --output CHANGELOG.md"
  echo "---- 新版本段预览 ----"
  git-cliff --unreleased 2>/dev/null || true
  echo "---- 预览结束 ----"
fi

# ---------- 6. 构建 ----------
if [[ $DRY_RUN -eq 0 ]]; then
  log "构建 release 产物: cargo tauri build"
  cargo tauri build 2>&1 | tail -15
  DMG="target/release/bundle/dmg/OpenZen_${VERSION_NO_V}_aarch64.dmg"
  [[ -f "$DMG" ]] && log "产物: $DMG" || warn "未找到预期 dmg: $DMG（手动检查 bundle 目录）"
fi

# ---------- 7. commit + tag ----------
if [[ $DRY_RUN -eq 0 ]]; then
  log "创建发布 commit + tag: ${NEW_VERSION}"
  git add CHANGELOG.md "$TAURI_CONF" "$PKG_JSON" Cargo.lock
  git commit -m "chore(release): prepare for ${NEW_VERSION}"
  git tag "$NEW_VERSION"
  log "已创建 commit + tag ${NEW_VERSION}"
  log "推送: git push origin main --tags"
  log "推送后 CI 的 release job 会自动构建并挂载 GitHub Release（见 .github/workflows/release.yml）"
else
  log "[dry-run] 将创建 commit 'chore(release): prepare for ${NEW_VERSION}' + tag ${NEW_VERSION}"
fi

log "完成 ✓"
