#!/usr/bin/env bash
# ─────────────────────────────────────────────────────────────────────────────
# verify_gates.sh — OpenZen 机械验证闸门（供 verify-* skills 调用）
#
# 设计原则：能被脚本判定的，不要靠模型"看"。本脚本只做两类事：
#   1) 契约/一致性硬检查（事件、i18n、CSS 变量、危险默认值、安全边界）
#   2) 与基线的差分（谁把承重默认值改回去了、谁新引入了 unwrap）
#
# 用法：
#   scripts/verify_gates.sh                # 默认：对工作树做全部一致性检查
#   scripts/verify_gates.sh --base HEAD~1  # 只对某个提交之后的 diff 做增量检查
#   scripts/verify_gates.sh --deep         # 额外跑 cargo fmt --check 与数据体积基线
#   scripts/verify_gates.sh --update-baseline
#   scripts/verify_gates.sh --quiet        # 只输出 FAIL/WARN 行
#
# 退出码：0 = 无 FAIL；1 = 有 FAIL（可作 CI / pre-push 门禁）
# 约定：WARN 不阻塞，但必须在报告里逐条解释。
# ─────────────────────────────────────────────────────────────────────────────
set -uo pipefail

REPO_ROOT="$(git rev-parse --show-toplevel 2>/dev/null || pwd)"
cd "$REPO_ROOT" || exit 2

BASE=""
DEEP=0
QUIET=0
UPDATE_BASELINE=0
BASELINE="$REPO_ROOT/scripts/verify-baseline.json"

while [ $# -gt 0 ]; do
  case "$1" in
    --base) BASE="${2:-}"; shift 2 ;;
    --deep) DEEP=1; shift ;;
    --quiet) QUIET=1; shift ;;
    --update-baseline) UPDATE_BASELINE=1; shift ;;
    -h|--help) sed -n '2,20p' "$0"; exit 0 ;;
    *) echo "unknown arg: $1" >&2; exit 2 ;;
  esac
done

GATE_LOG="$(mktemp -t ozselftest)"; trap 'rm -f "$GATE_LOG"' EXIT
red()  { printf '\033[31m%s\033[0m\n' "$*"; }
ylw()  { printf '\033[33m%s\033[0m\n' "$*"; }
grn()  { printf '\033[32m%s\033[0m\n' "$*"; }
hdr()  { [ "$QUIET" = 1 ] || printf '\n\033[1;36m── %s\033[0m\n' "$*"; }
fail() { red "  ✗ FAIL  $*"; echo F >> "$GATE_LOG"; }
warn() { ylw "  ⚠ WARN  $*"; echo W >> "$GATE_LOG"; }
pass() { [ "$QUIET" = 1 ] || grn "  ✓ ok    $*"; echo P >> "$GATE_LOG"; }
# 把一段命令的输出原样打印，并按其中的 FAIL/WARN 行数计账（子 shell 安全）
run_gate() { local out; out="$("$@" 2>&1)"; printf '%s\n' "$out" | sed '/^$/d'
  local f w; f=$(printf '%s\n' "$out" | grep -c 'FAIL' || true); w=$(printf '%s\n' "$out" | grep -c 'WARN' || true)
  [ "${f:-0}" -gt 0 ] && for _ in $(seq 1 "$f"); do echo F >> "$GATE_LOG"; done
  [ "${w:-0}" -gt 0 ] && for _ in $(seq 1 "$w"); do echo W >> "$GATE_LOG"; done
  return 0; }

if [ -n "$BASE" ]; then DIFF_RANGE="$BASE"; else DIFF_RANGE="HEAD"; fi
CHANGED="$(git diff --name-only "$DIFF_RANGE" 2>/dev/null; git diff --name-only 2>/dev/null; git ls-files -o --exclude-standard 2>/dev/null)"
CHANGED="$(printf '%s\n' "$CHANGED" | sort -u | sed '/^$/d')"
is_changed() { printf '%s\n' "$CHANGED" | grep -qx "$1"; }

printf '\033[1mOpenZen verify_gates\033[0m  repo=%s  base=%s  changed=%s files\n' \
  "$REPO_ROOT" "$DIFF_RANGE" "$(printf '%s\n' "$CHANGED" | sed '/^$/d' | wc -l | tr -d ' ')"

# ─────────────────────────────────────────────────────────────────────────────
hdr "G1 事件契约：Rust StreamEvent ↔ 前端消费点"
run_gate python3 - "$QUIET" <<'PY'
import re, sys, pathlib
quiet = len(sys.argv) > 1 and sys.argv[1] == "1"
ev = pathlib.Path("crates/oz-core-types/src/event.rs")
if not ev.exists():
    print("  · skip (event.rs not found)"); sys.exit(0)
body = re.search(r"pub enum StreamEvent \{(.*?)\n\}", ev.read_text(), re.S)
if not body:
    print("  ✗ FAIL  cannot parse StreamEvent enum"); sys.exit(1)
rust = set(re.sub(r"(?<!^)(?=[A-Z])", "_", v).lower()
           for v in re.findall(r"^\s{4}([A-Z][A-Za-z0-9]*)", body.group(1), re.M))
src = []
for p in pathlib.Path("frontends/src").rglob("*"):
    if p.suffix in (".ts", ".svelte"):
        src.append(p.read_text(errors="replace"))
blob = "\n".join(src)
# 事件名以外的前端消费形态：union 里的 'x'、switch 里的 case "x"、字符串比较等
missing = sorted(t for t in rust if t not in blob)
# 有意不在桌面端呈现的事件（仍需在报告里人工确认）
# Deliberately not rendered on the desktop: `tool_call_ready` is the
# speculative-parse event the desktop path replaces with the executed
# `tool_input_*` pair (round5 audit P2-12). Listed here so the gate stays
# quiet about a decision that was made on purpose.
allow = {"tool_call_ready"}
hard = [t for t in missing if t not in allow]
soft = [t for t in missing if t in allow]
for t in hard:
    print(f"  ✗ FAIL  Rust 发 `{t}`，前端全仓 0 处引用（事件会被静默丢弃）")
for t in soft:
    print(f"  ·       `{t}` 有意不在桌面端渲染（allow 列表已记录）")
# 反向：前端声明但 Rust 从不发
ts = pathlib.Path("frontends/src/lib/stores/parts.ts")
if ts.exists():
    m = re.search(r"export type ProtocolV1Event =(.*?)\n\n", ts.read_text(), re.S)
    if m:
        dead = sorted(set(re.findall(r"type:\s*'([a-z0-9_]+)'", m.group(1))) - rust)
        for t in dead:
            print(f"  ⚠ WARN  前端声明 `{t}` 但 Rust 从不发送（死分支）")
if not hard and not soft:
    print("  ✓ ok    24/24 事件两端一致")
sys.exit(1 if hard else 0)
PY

# ─────────────────────────────────────────────────────────────────────────────
hdr "G2 i18n 键集（zh ↔ en 双向差集）"
run_gate python3 - "$QUIET" <<'PY'
import json, sys, pathlib
quiet = len(sys.argv) > 1 and sys.argv[1] == "1"
zh_p, en_p = pathlib.Path("frontends/src/lib/i18n/zh.json"), pathlib.Path("frontends/src/lib/i18n/en.json")
if not (zh_p.exists() and en_p.exists()):
    print("  · skip (i18n files not found)"); sys.exit(0)
zh, en = set(json.load(open(zh_p))), set(json.load(open(en_p)))
bad = 0
for k in sorted(zh - en): print(f"  ✗ FAIL  zh 有、en 缺: {k}"); bad += 1
for k in sorted(en - zh): print(f"  ✗ FAIL  en 有、zh 缺: {k}"); bad += 1
if not bad: print(f"  ✓ ok    两侧各 {len(zh)} 键，双向差集为空")
sys.exit(1 if bad else 0)
PY

hdr "G2b 组件里绕过 \$t 的硬编码中文文本"
python3 - "$QUIET" <<'PYEOF'
import pathlib, re, sys
quiet = len(sys.argv) > 1 and sys.argv[1] == "1"
roots = [pathlib.Path("frontends/src/lib/components"), pathlib.Path("frontends/src/App.svelte")]
skip = re.compile(r"^\s*(//|/\*|\*|<!--)")
# `i18n-allow` marks a deliberate literal: a quoted UI label that is being
# highlighted inside a translated sentence (bilingual hint text), or a brand
# seal that DESIGN.md declares non-i18n. Everything else must go through $t.
allow = re.compile(r"i18n-allow")
hits = 0
for r in roots:
    files = [r] if r.is_file() else sorted(r.glob("*.svelte"))
    for f in files:
        if allow.search(f.read_text(errors="replace")):
            # Marker must be within 3 lines of the literal; simplest honest
            # rule: a file-level marker suppresses only its own lines.
            pass
        for i, line in enumerate(f.read_text(errors="replace").splitlines(), 1):
            if "$t(" in line or skip.match(line):
                continue
            # Look back up to 8 lines for an allow marker (a multi-line
            # {@html ...} expression needs the marker at its head).
            ctx = f.read_text(errors="replace").splitlines()[max(0, i - 9):i]
            if any(allow.search(c) for c in ctx):
                continue
            for m in re.finditer(r">([^<>{}]*[\u3400-\u9fff][^<>{}]*)<", line):
                text = m.group(1).strip()
                if len(text) < 2:
                    continue
                print(f"  WARN  {f}:{i}  裸文本 {text[:40]!r}")
                hits += 1
if not hits and not quiet:
    print("  ok    组件内未见裸中文文本")
sys.exit(0)
PYEOF

# ─────────────────────────────────────────────────────────────────────────────
hdr "G3 未定义 CSS 变量（var(--x) 无定义）"
run_gate python3 - "$QUIET" <<'PY'
import re, os, sys, collections
quiet = len(sys.argv) > 1 and sys.argv[1] == "1"
root = "frontends/src"
if not os.path.isdir(root):
    print("  · skip"); sys.exit(0)
defined = set()
defs_in = collections.defaultdict(list)
for dp, _, fs in os.walk(root):
    for f in fs:
        p = os.path.join(dp, f)
        if f.endswith((".css", ".svelte")):
            t = open(p, errors="replace").read()
            for m in re.finditer(r"^\s*(--[a-z0-9-]+)\s*:", t, re.M):
                defined.add(m.group(1)); defs_in[m.group(1)].append(p)
            for m in re.finditer(r"style=\"[^\"]*?(--[a-z0-9-]+)\s*:", t):
                defined.add(m.group(1))
used = collections.Counter(); where = {}
for dp, _, fs in os.walk(root):
    for f in fs:
        if f.endswith((".svelte", ".css", ".ts")):
            p = os.path.join(dp, f)
            t = open(p, errors="replace").read()
            for m in re.finditer(r"var\((--[a-z0-9-]+)", t):
                used[m.group(1)] += 1; where.setdefault(m.group(1), []).append(p)
undef = {k: v for k, v in used.items() if k not in defined}
for k in sorted(undef, key=lambda x: -used[x]):
    print(f"  ✗ FAIL  {k} 未定义，被 {used[k]} 处引用 → {', '.join(sorted(set(where[k]))[:3])}")
if not undef:
    print(f"  ✓ ok    {len(defined)} 个 token 全部有定义")
sys.exit(1 if undef else 0)
PY

# ─────────────────────────────────────────────────────────────────────────────
hdr "G4 diff 内新增硬编码色值（应走 token）"
if [ -n "$CHANGED" ]; then
  HEXHITS=$(git diff -U0 "$DIFF_RANGE" -- 'frontends/**/*.svelte' 'frontends/**/*.css' 2>/dev/null \
            | grep -E '^\+' | grep -vE '^\+\+\+' \
            | grep -oE '#[0-9a-fA-F]{6}\b' | sort | uniq -c | sort -rn | head -10)
  if [ -n "$HEXHITS" ]; then
    # NOTE: `while read -r count hex` breaks under bash 3.2 + a multibyte
    # locale (the loop variable name gets mangled → "unbound variable" and the
    # gate reports nothing). Iterate with awk instead.
    printf '%s\n' "$HEXHITS" | awk '{ printf "  WARN  新增硬编码色 %s x%s（除非在 token 表内）\n", $2, $1 }'
    G4HITS=$(printf '%s\n' "$HEXHITS" | wc -l | tr -d ' ')
    warn "diff 内新增 $G4HITS 种硬编码色值（见上）"
  else pass "diff 内无新增硬编码色值"; fi
else pass "no changed files"; fi

# ─────────────────────────────────────────────────────────────────────────────
hdr "G13 docs/ 必须本地保留（永不进远程）"
run_gate bash -c '
set -e
# 用户决定（2026-10-22）：docs/ 含个人画像摘录与真实遥测，公开仓库只放纯净代码。
# `git check-ignore docs` alone returns 1 for a directory pattern —
# probe a path INSIDE docs/ (and the directory form with a trailing slash).
if ! git check-ignore -q docs/ 2>/dev/null && ! git check-ignore -q docs/probe 2>/dev/null; then
  echo "  ✗ FAIL  .gitignore 没有忽略 docs/ —— 开发文档可能被推送"
  exit 1
fi
TRACKED=$(git ls-files docs | wc -l | tr -d " ")
if [ "$TRACKED" -gt 0 ]; then
  echo "  ✗ FAIL  有 $TRACKED 个 docs/ 文件已被跟踪（应全部本地保留）"
  git ls-files docs | head -5 | sed "s/^/      /"
  exit 1
fi
echo "  ok    docs/ 已忽略且 0 个文件被跟踪"
'

# ─────────────────────────────────────────────────────────────────────────────
hdr "G4b 死回退：新增行里 var(--token, #hex) 而 token 已定义"
if [ -n "$CHANGED" ]; then
  run_gate python3 - "$DIFF_RANGE" <<'PY'
import pathlib, re, subprocess, sys
rng = sys.argv[1]
css = pathlib.Path("frontends/src/app.css")
if not css.exists():
    sys.exit(0)
defined = set(re.findall(r"--([a-z0-9-]+):", css.read_text()))
pat = re.compile(r"var\(--([a-z0-9-]+),\s*(#[0-9a-fA-F]{6})\)")
diff = subprocess.run(
    ["git", "diff", "-U0", rng, "--", "frontends/**/*.svelte"],
    capture_output=True, text=True).stdout
hits = []
for line in diff.splitlines():
    if not line.startswith("+") or line.startswith("+++"):
        continue
    for m in pat.finditer(line):
        if m.group(1) in defined:
            hits.append(f"--{m.group(1)} 死回退 {m.group(2)}  |  {line[1:].strip()[:90]}")
# A literal fallback on a DEFINED token is dead code, and for a two-theme token
# it is necessarily wrong in one of the two themes. 102 such values (an older
# palette: #81b5c7 / #8a8a90 / #f0b850 …) were removed in round5.
# Legacy sites in untouched files are recorded in the verify-design skill's
# known-drift table rather than re-reported here.
for h in hits[:10]:
    print(f"  WARN  新增死回退: {h}")
if not hits:
    print("  ok    新增行内无死回退")
sys.exit(0)
PY
else pass "no changed files"; fi

# ─────────────────────────────────────────────────────────────────────────────
hdr "G5 测试可信度（静默跳过 / 新增测试）"
# round5 P1-13 ③: `docs_only!()` no longer returns silently — it PANICS
# unless OZ_TESTS_ALLOW_DOCS_SKIP=1 is set explicitly, so a missing docs/ tree
# is a hard failure instead of a green no-op. Only flag the old shape.
SKIP=$(grep -c "docs_only!();" crates/oz-tests/src/lib.rs 2>/dev/null | tr -d ' ')
if grep -q "OZ_TESTS_ALLOW_DOCS_SKIP" crates/oz-tests/src/lib.rs 2>/dev/null; then
  pass "docs 缺失时默认 panic（显式 OZ_TESTS_ALLOW_DOCS_SKIP=1 才跳过并打 SKIP 标记）"
  SKIP=0
fi
SKIP2=$(grep -rn "if !dir.exists() { return; }\|if !dir.exists()" crates/oz-tools/src/registry.rs 2>/dev/null | wc -l | tr -d ' ')
IGN=$(grep -rn "#\[ignore" crates/ src-tauri/ --include=*.rs 2>/dev/null | wc -l | tr -d ' ')
if [ "${SKIP:-0}" -gt 0 ]; then
  warn "oz-tests 里 $SKIP 个测试在缺 docs/ 时静默 return（CI 上 = 通过，零信息量）"
else pass "无 docs_only! 静默跳过"; fi
[ "${SKIP2:-0}" -gt 0 ] && warn "registry.rs 有 $SKIP2 处 fixture-missing 静默 return"
NEWTESTS=$(git diff -U0 "$DIFF_RANGE" -- '*.rs' 2>/dev/null | grep -cE '^\+.*#\[(tokio::)?test\]')
NEWFIX=$(git diff -U0 "$DIFF_RANGE" -- '*.rs' 2>/dev/null | grep -cE '^\+.*fn .*(regress|repro|guard|_fails)')
if [ "${NEWTESTS:-0}" -gt 0 ]; then
  pass "diff 新增 $NEWTESTS 个测试（$NEWFIX 个疑似回归测试）"
elif [ -n "$CHANGED" ] && printf '%s\n' "$CHANGED" | grep -q '\.rs$'; then
  warn "本次 Rust diff 没有新增任何 #[test] —— 修复必须带一条『修前会失败』的回归测试"
fi

# ─────────────────────────────────────────────────────────────────────────────
hdr "G6 承重默认值（与基线比对 + 已知反模式）"
run_gate python3 - "$BASELINE" "$UPDATE_BASELINE" <<'PYEOF'
import json, re, sys, pathlib
baseline_p, update = pathlib.Path(sys.argv[1]), sys.argv[2] == "1"
# name: (file, regex, 说明, 危险判定)
def lt(n):
    return lambda v: (v.lstrip("0123456789")[:1].isdigit() or v.isdigit()) and v.isdigit() and int(v) < n
WATCH = {
    "tool_timeout_secs":    ("crates/oz-core/src/handler.rs", r"tool_timeout_secs:\s*(\d+)",
                             "工具默认超时（round5 P0-7：应 >=300）", lt(120)),
    "max_turns":            ("crates/oz-core/src/handler.rs", r"max_turns:\s*(u32::MAX|\d+)",
                             "单 run 轮次上限", lambda v: v == "u32::MAX"),
    "max_wal_records":      ("crates/oz-core/src/checkpoint.rs", r"MAX_WAL_RECORDS:\s*usize\s*=\s*(\d+)",
                             "WAL 保留全量快照数（round5 P0-2：应 <=1）", lambda v: v.isdigit() and int(v) > 1),
    "checkpoint_interval":  ("src-tauri/src/runner.rs", r"checkpoint_interval\s*=\s*(\d+)",
                             "检查点间隔（越大越省 IO）", lambda v: False),
    "max_concurrent_tools": ("src-tauri/src/runner.rs", r"max_concurrent_tools\s*=\s*(\d+)",
                             "每会话工具并发", lambda v: False),
    "global_tool_sem":      ("crates/oz-core/src/agent_loop.rs", r"Semaphore::new\((\d+)\)",
                             "进程级工具并发（P1-16 需要 acquire 超时）", lambda v: False),
    "summary_wait_secs":    ("crates/oz-core/src/compress.rs", r"summary_wait_secs:\s*(\d+)",
                             "压缩摘要内联等待（P1-17：越小越好）", lambda v: v.isdigit() and int(v) > 300),
    "processing_timeout":   ("frontends/src/lib/stores/chat.ts", r"PROCESSING_TIMEOUT_MS\s*=\s*([0-9*\s]+);",
                             "前端看门狗", lambda v: False),
}
cur, notes = {}, []
for name, (f, rx, desc, bad) in WATCH.items():
    p = pathlib.Path(f)
    if not p.exists():
        continue
    m = re.search(rx, p.read_text(errors="replace"))
    if m:
        v = m.group(1).strip()
        cur[name] = v
        notes.append((name, v, desc, bad(v)))
if update or not baseline_p.exists():
    baseline_p.write_text(json.dumps(cur, indent=2, ensure_ascii=False) + "\n")
    print(f"  ok    基线已写入 {baseline_p}（{len(cur)} 项）")
    sys.exit(0)
old = json.loads(baseline_p.read_text())
drift = bad_now = 0
for n, v, d, is_bad in notes:
    mark = ""
    if n in old and old[n] != v:
        mark = f"  [{old[n]} -> {v}]"; drift += 1
    if is_bad:
        print(f"  WARN  {n} = {v}{mark}   {d}")
        bad_now += 1
    elif mark:
        print(f"  WARN  {n} 漂移{mark}   {d}")
    else:
        print(f"  ·     {n} = {v}   {d}")
if not drift and not bad_now:
    print("  ok    承重默认值未漂移，且无已知反模式")
PYEOF

# ─────────────────────────────────────────────────────────────────────────────
hdr "G7 安全边界（免审批写面 / 敏感路径）"
run_gate python3 - <<'PY'
import re, pathlib
g = pathlib.Path("crates/oz-safety/src/guard.rs")
f = pathlib.Path("crates/oz-tools/src/file_ops.rs")
bad = 0
if g.exists():
    m = re.search(r"safe_tools:\s*vec!\[(.*?)\]", g.read_text(), re.S)
    tools = re.findall(r'"([a-z_]+)"', m.group(1)) if m else []
    risky = {"write", "edit", "patch", "code_run", "terminal", "web_js"}
    # `write`/`edit`/`patch` are allowed to stay unapproved ONLY because the
    # path fence constrains them (working dir + runtime-state rejection).
    # Verify that fence exists; without it they are a real finding.
    fence = "is_in_working_dir" in f.read_text() if f.exists() else False
    for t in sorted(set(tools) & risky):
        if t in {"write", "edit", "patch"} and fence:
            print(f"  ·       `{t}` 免审批，但写面受 is_in_working_dir + 运行态拒写约束")
        else:
            print(f"  ⚠ WARN  `{t}` 仍在免审批 safe_tools（写面必须限制目标路径）")
            bad += 1
    for must in ("read", "grep", "glob", "ls"):
        if must not in tools:
            print(f"  ·       `{must}` 已不在 safe_tools（确认是有意收紧）")
if f.exists():
    m = re.search(r"SENSITIVE_ALWAYS:\s*&\[&str\]\s*=\s*&\[(.*?)\];", f.read_text(), re.S)
    pats = re.findall(r'"([^"]+)"', m.group(1)) if m else []
    required = {".ssh/", ".aws/", "mykey.toml"}
    missing = required - set(pats)
    for r in sorted(missing):
        print(f"  ✗ FAIL  敏感路径名单缺少 `{r}`")
    # Injection surfaces are guarded by a SEPARATE, path-relative mechanism
    # (`is_protected_runtime_path`), not by the substring list: the substring
    # form cannot be used here because the project's own checkout path
    # contains "/openzen/", so a substring rule would block every file in the
    # repo. Verify the real mechanism instead.
    src = f.read_text()
    m2 = re.search(r"PROTECTED_DIRS:\s*&\[&str\]\s*=\s*&\[(.*?)\];", src, re.S)
    dirs = set(re.findall(r'"([^"]+)"', m2.group(1))) if m2 else set()
    want = {"openzen", "memory", ".openzen"}
    absent = want - dirs
    if not m2:
        print("  ⚠ WARN  未找到运行态拒写名单（is_protected_runtime_path）")
        bad += 1
    elif absent:
        print(f"  ⚠ WARN  注入面未纳入拒写名单: {', '.join(sorted(absent))}")
        bad += 1
    else:
        print(f"  ✓ ok    运行态注入面已拒写: {', '.join(sorted(dirs))}")
    if "PROTECTED_FILES" in src and "servers.toml" in src:
        print("  ✓ ok    servers.toml / trust.json 已纳入拒写")
    else:
        print("  ⚠ WARN  servers.toml / trust.json 未纳入拒写")
        bad += 1
if bad == 0:
    print("  ✓ ok    免审批写面与敏感名单未见回退")
PY

# ─────────────────────────────────────────────────────────────────────────────
hdr "G8 diff 新增的 Rust 风险模式"
if printf '%s\n' "$CHANGED" | grep -q '\.rs$'; then
  for pat in 'unwrap\(\)' 'expect\(' 'panic!\(' 'unsafe ' 'eprintln!' 'let _ = ' ; do
    n=$(git diff -U0 "$DIFF_RANGE" -- '*.rs' 2>/dev/null | grep -E '^\+' | grep -vE '^\+\+\+' | grep -cE "$pat")
    [ "${n:-0}" -gt 0 ] && warn "新增 ${n} 处 ${pat}"
  done
  docs=$(git diff -U0 "$DIFF_RANGE" -- '*.rs' 2>/dev/null | grep -E '^\+' | grep -cE '^\+\s*pub (fn|struct|enum) ')
  docc=$(git diff -U0 "$DIFF_RANGE" -- '*.rs' 2>/dev/null | grep -E '^\+' | grep -cE '^\+\s*(///|//!)')
  pass "新增 pub 项 $docs 个 / 文档行 $docc 条"
else pass "no rust diff"; fi

# ─────────────────────────────────────────────────────────────────────────────
hdr "G9 版本与产物一致性"
V1=$(python3 -c "import json;print(json.load(open('src-tauri/tauri.conf.json'))['version'])" 2>/dev/null || echo "?")
V2=$(python3 -c "import json;print(json.load(open('frontends/package.json'))['version'])" 2>/dev/null || echo "?")
[ "$V1" = "$V2" ] && pass "tauri.conf.json 与 package.json 版本一致 ($V1)" || fail "版本不一致: $V1 vs $V2"
DIRTY=$(git status --porcelain | wc -l | tr -d ' ')
if [ "$DIRTY" -gt 0 ]; then warn "工作树有 $DIRTY 项未提交（release.sh 会拒绝发布）"; else pass "工作树干净"; fi

# ─────────────────────────────────────────────────────────────────────────────
if [ "$DEEP" = 1 ]; then
  hdr "G10 数据体积基线（真实运行数据）"
  D="$HOME/.openzen"
  if [ -d "$D" ]; then
    SZ=$(du -sm "$D/openzen/sessions.json" 2>/dev/null | cut -f1)
    CP=$(du -sm "$D/workspace/openzen/checkpoints" 2>/dev/null | cut -f1)
    WAL=$(du -sm "$D"/workspace/openzen/checkpoints/wal_*.mmap 2>/dev/null | awk '{s+=$1} END{print s+0}')
    printf '  · sessions.json %s MB · checkpoints %s MB · WAL %s MB\n' "${SZ:-?}" "${CP:-?}" "${WAL:-?}"
    [ "${SZ:-0}" -gt 40 ] && warn "sessions.json > 40 MB（见 round5 P0-1）"
    [ "${CP:-0}" -gt 300 ] && warn "checkpoints > 300 MB（见 round5 P0-2）"
    [ -n "$CHANGED" ] && pass "体积已采样"
  else pass "no data root"; fi
  hdr "G11 文档契约抽查"
  grep -q "持久化指纹\|entry_fingerprint" crates/oz-server/src/webui/sessions.rs && pass "fingerprint 仍在 sessions.rs" || warn "fingerprint 位置变了，检查文档引用"
fi

# ─────────────────────────────────────────────────────────────────────────────
hdr "G12 设计 token 单源（DESIGN.md ↔ app.css @theme）"
run_gate python3 - <<'PYEOF'
import pathlib, re, sys
d, c = pathlib.Path("frontends/DESIGN.md"), pathlib.Path("frontends/src/app.css")
if not (d.exists() and c.exists()):
    print("  · skip (missing DESIGN.md or app.css)"); sys.exit(0)
ctext = c.read_text()
# 只取 @theme 块（默认/深色主题）；后面的 .light 覆盖块不算漂移
m = re.search(r"@theme\s*\{(.*?)\n\}", ctext, re.S)
scope = m.group(1) if m else ctext
def first_tokens(t):
    out = {}
    for mm in re.finditer(r"(--[a-z0-9-]+)\s*:\s*([^;/\n]+)", t):
        out.setdefault(mm.group(1), mm.group(2).strip().lower())
    return out
dt = first_tokens(d.read_text()); ct = first_tokens(scope)
common = sorted(set(dt) & set(ct))
drift = [(k, dt[k], ct[k]) for k in common if dt[k] != ct[k]]
for k, a, b in drift:
    print(f"  WARN  {k}: DESIGN.md={a}  app.css={b}")
if not drift:
    print(f"  ok    {len(common)} 个共同 token 取值一致")
undoc = sorted(k for k in set(ct) - set(dt) if k.startswith("--color-"))
if undoc:
    print(f"  WARN  {len(undoc)} 个 --color-* token 未登记进 DESIGN.md: {', '.join(undoc[:8])}{' …' if len(undoc) > 8 else ''}")
sys.exit(0)
PYEOF

# ─────────────────────────────────────────────────────────────────────────────
FAIL=$(grep -c F "$GATE_LOG" || true); WARN=$(grep -c W "$GATE_LOG" || true); PASS=$(grep -c P "$GATE_LOG" || true)
FAIL=${FAIL:-0}; WARN=${WARN:-0}; PASS=${PASS:-0}
printf '\n\033[1mSummary\033[0m  PASS=%d  WARN=%d  FAIL=%d\n' "$PASS" "$WARN" "$FAIL"
if [ "$FAIL" -gt 0 ]; then red "GATES FAILED — 先修 FAIL 再谈提交"; exit 1; fi
[ "$WARN" -gt 0 ] && ylw "GATES PASSED with $WARN warning(s) — 每条 WARN 都要在报告里给出解释或修复计划"
exit 0
