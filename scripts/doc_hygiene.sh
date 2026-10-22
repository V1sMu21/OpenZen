#!/usr/bin/env bash
# doc_hygiene.sh — docs/ 入库前检测（可脚本化版本的 git-skill 18 维管道）
#
# 为什么需要它：docs/ 现在分层入库（round5 P1-13①），而文档里天然会写入
# 真机路径、真实 API key 片段、MB 级会话记录。git-skill 的 18 维检测是
# **推送前**的人工流程；本脚本把其中可机械判定的维度抽出来，让"哪些文档
# 能入库"在 `git add` 之前就有客观信号。
#
# 检测维度（对应 git-skill 编号）：
#   1  敏感值子串（来自 mykey.toml 等真实配置，只比对不回显）
#   2  敏感模式 + 高熵串（sk-/ghp_/xoxb/Bearer + Shannon 熵 > 4.0）
#   3  本机绝对路径（/Users/<user>、/home/<user>）
#   5  二进制/数据库文件
#   6  大文件（>10MB 警告，>100MB 阻止）
#   8  构建产物（dist/ target/ *.dmg *.app *.zip）
#   9  截图（*.png/*.jpg/*.gif 等二进制图像）
#   14 GitHub 硬限制（单文件 100MB）
#   18 本地保留清单（zcode 会话记录等）
#
# 用法:
#   bash scripts/doc_hygiene.sh              # 扫描"将要入库的 docs 文件"
#   bash scripts/doc_hygiene.sh --all        # 扫描 docs/ 下全部文件（含本地草稿）
#   bash scripts/doc_hygiene.sh --path docs/adr   # 只扫指定路径
#   bash scripts/doc_hygiene.sh --quiet      # 只输出结论行
#
# 退出码: 0 = 通过（可含警告）；1 = 有阻断项
#
# 设计约束（来自 git-skill 安全规则）：
#   - **绝不回显任何敏感值**，只报文件名 + 命中维度 + 掩码后的前 4 字符
#   - 不修改任何文件；只读扫描
#   - 误报白名单集中在 FAKE_KEY_ALLOWLIST（见下）

set -uo pipefail

cd "$(dirname "$0")/.."   # workspace 根

QUIET=0
SCAN_ALL=0
SCAN_PATH="docs"
while [[ $# -gt 0 ]]; do
  case "$1" in
    --quiet) QUIET=1 ;;
    --all)   SCAN_ALL=1 ;;
    --path)  SCAN_PATH="${2:-docs}"; shift ;;
    -h|--help) sed -n '2,30p' "$0"; exit 0 ;;
    *) echo "unknown arg: $1" >&2; exit 2 ;;
  esac
  shift
done

RED='\033[0;31m'; YELLOW='\033[1;33m'; GREEN='\033[0;32m'; DIM='\033[2m'; NC='\033[0m'
BLOCKERS=0
WARNINGS=0
say()  { [[ $QUIET -eq 1 ]] || echo -e "$*"; }
block(){ echo -e "${RED}✗ BLOCK${NC} $*"; BLOCKERS=$((BLOCKERS+1)); }
warn() { echo -e "${YELLOW}⚠ WARN ${NC} $*"; WARNINGS=$((WARNINGS+1)); }

# 含测试用假 key 的安全文件（git-skill 维度 1/2 白名单，保持同步）
FAKE_KEY_ALLOWLIST=(
  "docs/test-roadmap.md"
)

is_allowlisted() {
  local f="$1"
  for a in "${FAKE_KEY_ALLOWLIST[@]}"; do [[ "$f" == "$a" ]] && return 0; done
  return 1
}

# ── 0. 待扫清单 ────────────────────────────────────────────────────────────
# 默认只扫"会被 git 收进来的"文件：`git add -n` 的 dry-run 结果。这样脚本
# 报的就是真正要入库的东西，本地草稿不会淹没信号。
# 用列表文件而不是 bash 数组：macOS 自带 bash 3.2 没有 mapfile，且
# `set -u` 下空数组的 `"${A[@]}"` 会报 unbound variable。
FILELIST="$(mktemp -t oz-doclist)"
trap 'rm -f "$FILELIST"' EXIT
if [[ $SCAN_ALL -eq 1 ]]; then
  find "$SCAN_PATH" -type f 2>/dev/null | sort > "$FILELIST"
  MODE="全部文件"
else
  git add -n "$SCAN_PATH" 2>/dev/null | sed "s/^add '//; s/'$//" > "$FILELIST"
  MODE="将要入库的文件"
fi
FILE_COUNT=$(grep -c . "$FILELIST" 2>/dev/null || echo 0)

say ""
say "── doc_hygiene：${SCAN_PATH}（${MODE}，${FILE_COUNT} 个）"

if [[ "$FILE_COUNT" -eq 0 ]]; then
  say "${DIM}   （没有待入库文件）${NC}"
fi

# ── 1+2. 敏感值扫描 ────────────────────────────────────────────────────────
# 真实值来源：git-skill 维度 1 的数据源。只取前 12 字符做子串匹配，
# 任何情况下都不打印命中的值本身。
SECRET_SOURCES=(
  "$HOME/.openzen/mykey.toml"
  ".openzen/mykey.toml"
  "$HOME/.codex/config.toml"
)
REAL_SECRETS_FILE="$(mktemp -t oz-secrets)"
trap 'rm -f "$REAL_SECRETS_FILE"' EXIT

python3 - "$REAL_SECRETS_FILE" "${SECRET_SOURCES[@]}" <<'PY'
import re, sys, pathlib
out = pathlib.Path(sys.argv[1])
srcs = sys.argv[2:]
seen = set()
for s in srcs:
    p = pathlib.Path(s)
    if not p.is_file():
        continue
    try:
        text = p.read_text(errors="ignore")
    except OSError:
        continue
    # key = "value" / token = "value" / apikey = "value"
    for m in re.finditer(r'(?i)\b(?:api[_-]?key|apikey|token|secret|app[_-]?secret|bot[_-]?token|password)\b\s*[=:]\s*["\']?([A-Za-z0-9_\-\.]{12,})', text):
        v = m.group(1)
        # 占位符/环境变量引用不是真实值
        if v.startswith("${") or v.lower() in {"your_api_key", "changeme", "placeholder"}:
            continue
        seen.add(v[:12])
out.write_text("\n".join(sorted(seen)))
PY
SECRET_COUNT=$(wc -l < "$REAL_SECRETS_FILE" | tr -d ' ')
say "${DIM}   敏感值源: ${SECRET_COUNT} 个真实值前缀（不回显）${NC}"

# 敏感模式（正则）+ 高熵串（Python 做 Shannon 熵，避免 shell 里写复杂逻辑）
PATTERN_HITS="$(mktemp -t oz-pat)"
trap 'rm -f "$REAL_SECRETS_FILE" "$PATTERN_HITS"' EXIT
python3 - "$REAL_SECRETS_FILE" "$PATTERN_HITS" "$FILELIST" <<'PY'
import math, re, sys, pathlib

secrets_file, hits_file, list_file = sys.argv[1], sys.argv[2], sys.argv[3]
files = [l for l in pathlib.Path(list_file).read_text().splitlines() if l.strip()]
real_prefixes = [l.strip() for l in pathlib.Path(secrets_file).read_text().splitlines() if l.strip()]

# 已知 key 形态（git-skill 维度 2）
PATTERNS = [
    ("sk-", re.compile(r"sk-[A-Za-z0-9_\-]{16,}")),
    ("ghp_", re.compile(r"ghp_[A-Za-z0-9]{20,}")),
    ("github_pat_", re.compile(r"github_pat_[A-Za-z0-9_]{20,}")),
    ("xoxb-", re.compile(r"xox[baprs]-[A-Za-z0-9\-]{10,}")),
    ("AKIA", re.compile(r"AKIA[0-9A-Z]{16}")),
    ("Bearer", re.compile(r"Bearer\s+[A-Za-z0-9_\-\.]{20,}")),
    ("AIza", re.compile(r"AIza[0-9A-Za-z_\-]{30,}")),
]
# 高熵兜底：长度 >= 24 的 base64/hex-ish 串，Shannon 熵 > 4.0
ENTROPY_RE = re.compile(r"[A-Za-z0-9+/=_\-]{24,}")

def shannon(s: str) -> float:
    if not s:
        return 0.0
    counts = {}
    for ch in s:
        counts[ch] = counts.get(ch, 0) + 1
    n = len(s)
    return -sum((c / n) * math.log2(c / n) for c in counts.values())

def mask(v: str) -> str:
    return v[:4] + "…" if len(v) > 4 else "…"

BLOCK, WARN = [], []
for f in files:
    p = pathlib.Path(f)
    if not p.is_file():
        continue
    try:
        text = p.read_text(errors="ignore")
    except OSError:
        continue
    # ── 确定性命中（BLOCK）─────────────────────────────────────────────
    for name, rx in PATTERNS:
        for m in rx.finditer(text):
            BLOCK.append(f"{f}\t模式 {name}\t{mask(m.group(0))}")
    for pre in real_prefixes:
        if pre and pre in text:
            BLOCK.append(f"{f}\t真实配置值前缀\t{mask(pre)}")
    # ── 启发式命中（WARN）─────────────────────────────────────────────
    # 高熵串必须同时满足"像随机 token"的四个条件，否则文档里的测试 ID
    # （TAU-IPC-01）、路径（frontends/src/...）、kebab 标识符会把它淹没：
    #   1. 长度 >= 24 且无空白
    #   2. 同时含大写、小写、数字（随机 token 的特征）
    #   3. 不含 '/'（路径）与 ':'（URL/协议）
    #   4. 出现在赋值/键值上下文里（前一个非空白字符是 = : " ' 或 (）
    #   5. Shannon 熵 > 4.0
    for m in ENTROPY_RE.finditer(text):
        cand = m.group(0)
        if len(cand) < 24 or cand.startswith(("http", "data:")):
            continue
        if "/" in cand or ":" in cand:
            continue
        if not (any(c.isupper() for c in cand)
                and any(c.islower() for c in cand)
                and any(c.isdigit() for c in cand)):
            continue
        prev = text[max(0, m.start() - 1):m.start()]
        if prev not in '=:"\'(':
            continue
        ent = shannon(cand)
        if ent > 4.0:
            WARN.append(f"{f}\t高熵串(熵={ent:.2f})\t{mask(cand)}")

pathlib.Path(hits_file).write_text(
    "\n".join("BLOCK\t" + h for h in BLOCK) + ("\n" if BLOCK and WARN else "")
    + "\n".join("WARN\t" + h for h in WARN)
)
PY

if [[ -s "$PATTERN_HITS" ]]; then
  while IFS=$'\t' read -r sev f kind masked; do
    [[ -n "${sev:-}" ]] || continue
    if is_allowlisted "$f"; then
      say "${DIM}   （白名单跳过）${f} — ${kind}${NC}"
    elif [[ "$sev" == "BLOCK" ]]; then
      block "敏感内容：${f} — ${kind} (${masked})"
    else
      warn "疑似随机串（人工判断）：${f} — ${kind} (${masked})"
    fi
  done < "$PATTERN_HITS"
else
  say "${GREEN}   ✓ 无敏感值/高熵串命中${NC}"
fi

# ── 3. 本机绝对路径 ────────────────────────────────────────────────────────
# 约定：文档里的本机路径写成 ~ 或 <repo>。真实证据数值保留，只改路径前缀。
LOCAL_USER="$(whoami)"
PATH_HITS="$(mktemp -t oz-path)"; trap 'rm -f "$REAL_SECRETS_FILE" "$PATTERN_HITS" "$PATH_HITS"' EXIT
# 只对文本类文件做行号扫描（二进制会污染输出）
while IFS= read -r f; do
  [[ -f "$f" ]] || continue
  case "$f" in
    *.png|*.jpg|*.jpeg|*.gif|*.webp|*.ico|*.icns|*.pdf|*.zip|*.gz|*.dmg) continue ;;
  esac
  grep -nE "/Users/($LOCAL_USER|macstu)|/home/[a-z]+/" "$f" 2>/dev/null | sed "s|^|$f:|"
done < "$FILELIST" > "$PATH_HITS" || true
if [[ -s "$PATH_HITS" ]]; then
  # 按文件汇总（同一文件里几十处路径只报一次 + 计数）。
  # 注意：不要用 `... | while` 管道 —— 管道右端是子 shell，warn() 里的
  # 计数器加不回来（verify_gates.sh 踩过同一个坑）。先落盘再读。
  PATH_SUMMARY="$(mktemp -t oz-pathsum)"
  awk -F: '{print $1}' "$PATH_HITS" | sort | uniq -c | sort -rn > "$PATH_SUMMARY"
  while read -r cnt f; do
    [[ -n "${f:-}" ]] || continue
    warn "本机绝对路径：${f}（${cnt} 处）→ 建议改成 ~ 或 <repo>"
  done < "$PATH_SUMMARY"
  rm -f "$PATH_SUMMARY"
else
  say "${GREEN}   ✓ 无本机绝对路径${NC}"
fi

# ── 5/6/8/9/14. 文件形态检查 ───────────────────────────────────────────────
BINARY_EXT_RE='\.(png|jpg|jpeg|gif|webp|ico|icns|pdf|zip|gz|tar|dmg|app|so|dylib|bin|sqlite|sqlite-wal|sqlite-shm|db|mp4|mov|woff2?|ttf|otf)$'
BUILD_RE='(^|/)(dist|target|node_modules)/|\.(dmg|app|zip|tar\.gz)$'

while IFS= read -r f; do
  [[ -f "$f" ]] || continue
  # 单文件大小（macOS stat -f%z / GNU stat -c%s 兼容）
  size=$(stat -f%z "$f" 2>/dev/null || stat -c%s "$f" 2>/dev/null || echo 0)
  mb=$(( size / 1024 / 1024 ))

  if [[ $size -gt $((100 * 1024 * 1024)) ]]; then
    block "超过 GitHub 单文件 100MB 硬限制：${f}（${mb}MB）"
  elif [[ $size -gt $((10 * 1024 * 1024)) ]]; then
    warn "大文件 >10MB：${f}（${mb}MB）→ 确认是否应入库"
  fi

  if [[ "$f" =~ $BUILD_RE ]]; then
    block "构建产物：${f}"
  fi

  if [[ "$f" =~ $BINARY_EXT_RE ]]; then
    # 二进制文件单独判定：图像/压缩包不该出现在 docs 白名单里
    block "二进制/图像文件：${f}（${mb}MB）"
  fi

  # 维度 18：本地保留清单
  case "$f" in
    docs/zcode_*.md) block "本地保留清单（原始会话记录）：${f}" ;;
  esac
done < "$FILELIST"

# ── 15. 白名单自检 ─────────────────────────────────────────────────────────
# 反向确认：本地草稿确实没被收进来。若草稿出现在待入库清单里，说明
# .gitignore 的否定规则被改坏了。
if [[ $SCAN_ALL -eq 0 ]]; then
  for draft in docs/zcode_20260902.md docs/zcode_全面优化.md; do
    if grep -qx "$draft" "$FILELIST"; then
      block "本地草稿被误纳入白名单：${draft}（检查 .gitignore 的 docs/ 分层规则）"
    fi
  done
  say "${GREEN}   ✓ 本地草稿未被纳入（zcode_*.md / 截图）${NC}"
fi

# ── 结论 ───────────────────────────────────────────────────────────────────
TOTAL_BYTES=0
while IFS= read -r f; do
  [[ -f "$f" ]] || continue
  s=$(stat -f%z "$f" 2>/dev/null || stat -c%s "$f" 2>/dev/null || echo 0)
  TOTAL_BYTES=$(( TOTAL_BYTES + s ))
done < "$FILELIST"
say ""
if [[ $BLOCKERS -gt 0 ]]; then
  echo -e "${RED}Summary${NC}  BLOCK=$BLOCKERS WARN=$WARNINGS  待入库 ${FILE_COUNT} 个文件 / $((TOTAL_BYTES/1024)) KB"
  echo -e "${RED}✗ 有阻断项：先修掉再 git add${NC}"
  exit 1
else
  echo -e "${GREEN}Summary${NC}  BLOCK=0 WARN=$WARNINGS  待入库 ${FILE_COUNT} 个文件 / $((TOTAL_BYTES/1024)) KB"
  echo -e "${GREEN}✓ 通过（警告项请逐条判断：路径改占位符、真实数值保留）${NC}"
  exit 0
fi
