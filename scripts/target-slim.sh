#!/usr/bin/env bash
# scripts/target-slim.sh — 把 target/ 的占用压回可用范围
#
# 为什么 target 会长到好几个 GB：cargo 从不主动删旧产物。
#   1. 每次依赖升级 / rustc 升级 / 改 feature，旧 hash 的 rlib 全部留着（只增不减）
#   2. cargo check / cargo test 会额外生成整棵 dev profile 树（带调试信息，最容易上 GB）
#   3. target/*/incremental 是增量编译缓存，打包安装完全用不到
#   4. target/release/bundle/dmg 是打包中间产物（本仓库用 --bundles app，一般没有）
#
# 关键前提：target/ 里全是**可重建**的缓存。删掉只会让下次构建变慢，
# 不会让构建失败、不会改变产物内容。所以这里的清理是安全的。
#
# 用法：
#   bash scripts/target-slim.sh                    # 只报告，不动文件
#   bash scripts/target-slim.sh --apply            # 清理（上限默认 8GB，保留最近热缓存）
#   bash scripts/target-slim.sh --apply --max 5GB  # 自定义上限
#   bash scripts/target-slim.sh --apply --dev      # 额外删整棵 dev 树（cargo check/test 的产物）
#   bash scripts/target-slim.sh --apply --full     # 清空 target（保留已打好的 .app）
#
# 配套：scripts/install-macos.sh 构建安装成功后会自动跑 `--apply --max ${OZ_SLIM_MAX:-8GB}`，
# 传 --no-slim 可关闭。
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"

TARGET_DIR="${CARGO_TARGET_DIR:-target}"
if [[ "${TARGET_DIR}" != /* ]]; then
  TARGET_DIR="${ROOT}/${TARGET_DIR}"
fi

APPLY=0
DEV=0
FULL=0
MAX="8GB"

GREEN='\033[0;32m'; YELLOW='\033[1;33m'; RED='\033[0;31m'; NC='\033[0m'
log()  { echo -e "${GREEN}==>${NC} $*"; }
warn() { echo -e "${YELLOW}!! ${NC}$*"; }
die()  { echo -e "${RED}!! ${NC}$*" >&2; exit 1; }

usage() {
  sed -n '2,21p' "$0" | sed 's/^# \{0,1\}//'
}

# ── 尺寸工具 ────────────────────────────────────────────────
kb_of() { du -sk "$1" 2>/dev/null | awk '{print $1+0}'; }
gb() {
  awk -v k="$1" 'BEGIN {
    if (k >= 1048576)      printf "%.2f GB", k/1048576;
    else if (k >= 1024)    printf "%.0f MB", k/1024;
    else                   printf "%d KB", k;
  }'
}
kb_from_size() { # "8GB" -> KB；格式不对输出空
  local s rest
  s="$(printf '%s' "$1" | tr '[:lower:]' '[:upper:]')"
  case "${s}" in
    *GB) rest="${s%GB}"; [[ "${rest}" =~ ^[0-9]+$ ]] && echo $(( rest * 1048576 )) ;;
    *MB) rest="${s%MB}"; [[ "${rest}" =~ ^[0-9]+$ ]] && echo $(( rest * 1024 )) ;;
    *KB) rest="${s%KB}"; [[ "${rest}" =~ ^[0-9]+$ ]] && echo $(( rest )) ;;
    *)   [[ "${s}" =~ ^[0-9]+$ ]] && echo $(( s )) ;;
  esac
  return 0
}

while [[ $# -gt 0 ]]; do
  case "$1" in
    --apply)  APPLY=1 ;;
    --dev)    DEV=1 ;;
    --full)   FULL=1 ;;
    --max)    shift; MAX="${1:?--max 需要一个值，例如 8GB}" ;;
    --max=*)  MAX="${1#*=}" ;;
    -h|--help) usage; exit 0 ;;
    *) die "未知参数: ${1}（可用: --apply --dev --full --max 8GB）" ;;
  esac
  shift
done

MAX_KB="$(kb_from_size "${MAX}")"
[[ -n "${MAX_KB}" ]] || die "--max 格式不对: ${MAX}（例: 8GB / 500MB）"

report() {
  local total top
  total="$(kb_of "${TARGET_DIR}")"
  echo
  log "target: ${TARGET_DIR}"
  echo "    总计: $(gb "${total}")"
  top="$(du -sk "${TARGET_DIR}"/* 2>/dev/null | sort -rn | head -5 || true)"
  if [[ -n "${top}" ]]; then
    while read -r k rest; do
      [[ -n "${rest:-}" ]] || continue
      printf "    %9s  %s\n" "$(gb "${k}")" "${rest#"${TARGET_DIR}"/}"
    done <<< "${top}"
  fi
}

# ── 前置检查 ────────────────────────────────────────────────
if [[ ! -d "${TARGET_DIR}" ]]; then
  log "没有 target 目录（${TARGET_DIR}），无需清理。"
  exit 0
fi

[[ "${TARGET_DIR}" != "/" ]] || die "拒绝操作根目录"

if [[ ! -f "${TARGET_DIR}/CACHEDIR.TAG" \
   && ! -d "${TARGET_DIR}/release" \
   && ! -d "${TARGET_DIR}/debug" \
   && -z "$(ls -d "${TARGET_DIR}"/*/release 2>/dev/null || true)" ]]; then
  die "${TARGET_DIR} 不像 cargo target 目录（既无 CACHEDIR.TAG，也无 release/debug），拒绝删除"
fi

BEFORE="$(kb_of "${TARGET_DIR}")"
report

SEEN=""
strip_path() { # <path> <说明>  —— 同一个路径只处理一次
  local p="$1" label="$2" k
  [[ -e "${p}" ]] || return 0
  case " ${SEEN} " in *" ${p} "*) return 0 ;; esac
  SEEN="${SEEN} ${p}"
  k="$(kb_of "${p}")"
  [[ "${k}" -gt 0 ]] || return 0
  if [[ $APPLY -eq 1 ]]; then
    rm -rf "${p}"
    log "删除 ${label}（$(gb "${k}")）"
  else
    log "将删除 ${label}（$(gb "${k}")）"
  fi
}

# app bundle 必须留住：install-macos.sh 安装的就是它
full_clean() {
  local stash kept=0 app
  stash="$(mktemp -d "${TMPDIR:-/tmp}/oz-app-stash.XXXXXX")"
  for app in "${TARGET_DIR}"/release/bundle/macos/*.app; do
    [[ -d "${app}" ]] || continue
    cp -R "${app}" "${stash}/" && kept=1
  done

  if [[ $APPLY -eq 1 ]]; then
    log "删除整棵 target（保留已打好的 .app）"
    rm -rf "${TARGET_DIR}"
    if [[ $kept -eq 1 ]]; then
      mkdir -p "${TARGET_DIR}/release/bundle/macos"
      cp -R "${stash}"/*.app "${TARGET_DIR}/release/bundle/macos/"
      log "已保留 .app；下次构建从零编译"
    fi
  else
    log "将清空 target（已打好的 .app 会保留）"
  fi
  rm -rf "${stash}"
}

sweep_by_size() {
  local cur
  cur="$(kb_of "${TARGET_DIR}")"
  if [[ "${cur}" -le "${MAX_KB}" ]]; then
    log "已在上限内（$(gb "${cur}") ≤ ${MAX}），跳过 cargo sweep"
    return 0
  fi
  if ! command -v cargo-sweep >/dev/null 2>&1; then
    warn "超出上限但未安装 cargo-sweep；可先 cargo install cargo-sweep"
    return 0
  fi
  local -a cmd=(cargo sweep --maxsize "${MAX}")
  [[ $APPLY -eq 1 ]] || cmd+=(--dry-run)
  cmd+=("${ROOT}")
  log "超出上限，按最旧优先裁剪: cargo sweep --maxsize ${MAX}"
  ( cd "${ROOT}" && CARGO_TARGET_DIR="${TARGET_DIR}" "${cmd[@]}" ) \
    || warn "cargo sweep 未完成，跳过（不影响其他步骤）"
}

if [[ $FULL -eq 1 ]]; then
  full_clean
else
  # 1. 打包中间产物：dmg 目录（本仓库用 --bundles app，一般不产生）
  for d in "${TARGET_DIR}"/release/bundle/dmg "${TARGET_DIR}"/*/release/bundle/dmg; do
    strip_path "${d}" "dmg 打包中间产物"
  done

  # 2. 增量编译缓存：纯提速用，打包/安装不需要
  for d in "${TARGET_DIR}"/incremental "${TARGET_DIR}"/*/incremental "${TARGET_DIR}"/*/*/incremental; do
    strip_path "${d}" "incremental 缓存"
  done

  # 3. 可选：整棵 dev 树（cargo check / test / tauri dev 的产物）
  if [[ $DEV -eq 1 ]]; then
    for d in "${TARGET_DIR}"/debug "${TARGET_DIR}"/*/debug; do
      strip_path "${d}" "dev profile 整棵树"
    done
  fi

  # 4. 按上限裁剪旧产物
  sweep_by_size
fi

if [[ $APPLY -eq 1 ]]; then
  AFTER="$(kb_of "${TARGET_DIR}")"
  SAVED=$(( BEFORE - AFTER ))
  echo
  log "完成：$(gb "${BEFORE}") → $(gb "${AFTER}")，释放 $(gb "${SAVED}")"
  report
else
  echo
  log "以上为预演（未删任何文件）。执行: bash scripts/target-slim.sh --apply"
fi
