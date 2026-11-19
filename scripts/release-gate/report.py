#!/usr/bin/env python3
"""Render target/release-gate/report.html from status.tsv + perf.json."""

from __future__ import annotations

import argparse
import json
import re
from pathlib import Path

CSS = """
:root{--bg:#0b0d10;--panel:#11161d;--text:#c3cbd8;--gold:#cbb26a;--green:#5fd3a3;
--red:#e0707a;--amber:#d9a441;--border:#232a35;--mono:ui-monospace,Menlo,monospace}
body{margin:0;background:var(--bg);color:var(--text);font:14px/1.7 -apple-system,"PingFang SC",sans-serif}
.wrap{max-width:1080px;margin:0 auto;padding:40px 28px 80px}
h1{font-size:28px;color:#e9e2cf;margin:6px 0 4px}
h2{font-size:18px;color:var(--gold);margin:32px 0 10px;border-top:1px solid var(--border);padding-top:18px}
.muted{color:#818d9e}.small{font-size:12.5px}
code,pre{font-family:var(--mono);font-size:12px}
code{background:#151b24;border:1px solid #1f2834;border-radius:4px;padding:1px 5px;color:#d9cba6}
pre{background:#0f141b;border:1px solid var(--border);border-left:3px solid var(--gold);padding:10px 12px;overflow:auto}
table{width:100%;border-collapse:collapse;font-size:12.6px;margin:10px 0}
th{background:#141a22;color:var(--gold);text-align:left}
th,td{border:1px solid var(--border);padding:6px 8px;vertical-align:top}
tr:nth-child(even) td{background:#0e1218}
.verdict{border:1px solid var(--border);border-left:4px solid var(--green);background:#0f1714;border-radius:6px;padding:14px 18px;margin:16px 0;font-size:15px}
.bad{border-left-color:var(--red);background:#1a1113}.warn{border-left-color:var(--amber);background:#171410}
.g{color:var(--green)}.r{color:var(--red)}.a{color:var(--amber)}
.pill{display:inline-block;font:11px var(--mono);padding:0 7px;border-radius:20px;border:1px solid var(--border)}
ul{margin:6px 0 6px 20px;padding:0}li{margin:3px 0}
"""


def main() -> int:
    ap = argparse.ArgumentParser()
    ap.add_argument("--dir", default="target/release-gate")
    ap.add_argument("--version", default="?")
    ap.add_argument("--sha", default="?")
    ap.add_argument("--branch", default="?")
    ap.add_argument("--dirty", default="0")
    ap.add_argument("--env-limited", default="")
    args = ap.parse_args()

    out = Path(args.dir)
    meta = json.loads((out / "meta.json").read_text()) if (out / "meta.json").exists() else {}
    perf = json.loads((out / "perf.json").read_text()) if (out / "perf.json").exists() else {}
    rows = [line.split("\t") for line in (out / "status.tsv").read_text().splitlines() if line.strip()]
    rows = [r + [""] * (5 - len(r)) for r in rows]
    fails = [r for r in rows if r[2] == "FAIL"]
    skips = [r for r in rows if r[2] == "SKIP"]
    ux_fail = perf.get("failed") or []
    dirty = int(args.dirty or 0)
    env = args.env_limited

    if fails or ux_fail:
        verdict, cls = "❌ 阻塞发布：存在未关闭的 P0/P1", "bad"
    elif skips:
        verdict, cls = "⚠️ 不完整：有阶段被跳过，不能签核", "warn"
    elif env and dirty:
        verdict, cls = (
            f"⚠️ 可发布，但工作树含 {dirty} 项未提交改动 + 环境豁免：{env}（需提交并由 CI 补跑）",
            "warn",
        )
    elif env:
        verdict, cls = f"⚠️ 可发布但带环境豁免：{env}（需 CI 补跑）", "warn"
    elif dirty:
        verdict, cls = f"⚠️ 可发布但工作树含 {dirty} 项未提交改动（需显式接受）", "warn"
    else:
        verdict, cls = "✅ 可发布：全部门禁与体验预算通过", ""

    def esc(s: str) -> str:
        s = re.sub(r"\x1b\[[0-9;]*m", "", s)
        return s.replace("&", "&amp;").replace("<", "&lt;").replace(">", "&gt;")

    def detail_for(r) -> str:
        if r[1] != "workspace-tests":
            return r[4]
        log = out / f"{r[0]}-{r[1]}.log"
        if not log.exists():
            return r[4]
        text = log.read_text(errors="replace")
        passed = failed = ignored = suites = 0
        for m in re.finditer(
            r"test result: \w+\. (\d+) passed; (\d+) failed; (\d+) ignored", text
        ):
            suites += 1
            passed += int(m.group(1))
            failed += int(m.group(2))
            ignored += int(m.group(3))
        extra = " · 1 env-limited skip" if env else ""
        return f"{suites} suites · {passed} passed / {failed} failed / {ignored} ignored{extra}"

    phase_rows = "\n".join(
        f"<tr><td>{esc(r[0])}</td><td>{esc(r[1])}</td>"
        f"<td class=\"{'g' if r[2]=='PASS' else ('a' if r[2]=='SKIP' else 'r')}\">{r[2]}</td>"
        f"<td>{r[3]}s</td><td class=small>{esc(detail_for(r))}</td></tr>"
        for r in rows
    )
    ux_rows = ""
    for k, v in (perf.get("metrics") or {}).items():
        val = "-" if v.get("value") is None else v.get("value")
        ux_rows += (
            f"<tr><td><code>{esc(k)}</code></td><td>{val} {v.get('unit','')}</td>"
            f"<td>{v.get('budget','')}</td>"
            f"<td class=\"{'g' if v.get('verdict')=='pass' else 'r'}\">{v.get('verdict')}</td>"
            f"<td class=small>{esc(v.get('note',''))}</td></tr>"
        )

    checks = [
        ("R1 静态（fmt/check/clippy/gates）", all(r[2] == "PASS" for r in rows if r[0] == "R1")),
        ("R2 可信测试（ECF/全测/变异/E2E）", all(r[2] == "PASS" for r in rows if r[0] == "R2")),
        ("R3 体验预算", all(r[2] == "PASS" for r in rows if r[0] == "R3")),
        ("R4 Tauri bundle", any(r[1] == "bundle" and r[2] == "PASS" for r in rows)),
        ("版本三处一致", any(r[1] == "version-sync" and r[2] == "PASS" for r in rows)),
        ("工作树干净或已显式接受", dirty == 0),
        ("无业务 skip / 无未关闭 P0/P1", not skips and not fails and not ux_fail),
        ("无环境豁免（或已由 CI 补跑）", not env),
    ]
    check_rows = "".join(
        f"<tr><td>{esc(name)}</td><td class=\"{'g' if ok else 'r'}\">{'✓' if ok else '✗'}</td></tr>"
        for name, ok in checks
    )

    html = f"""<!DOCTYPE html><html lang="zh-CN"><head><meta charset="utf-8">
<title>Release Gate · v{esc(args.version)}</title><style>{CSS}</style></head>
<body><div class="wrap">
<div class="muted small">OpenZen release gate · v{esc(args.version)} · {esc(args.sha)} ({esc(args.branch)}) · {esc(meta.get('started_at',''))}</div>
<h1>发布前最后一关</h1>
<div class="verdict {cls}"><strong>{verdict}</strong><br>
<span class="small">dirty={dirty} · 阶段 {len(rows)} · FAIL {len(fails)} · SKIP {len(skips)} · UX fail {len(ux_fail)}
{' · <b>env-limited: ' + esc(env) + '</b>' if env else ''}</span></div>

<h2>1. 五段结果</h2>
<table><tr><th>段</th><th>检查</th><th>结果</th><th>耗时</th><th>摘要</th></tr>{phase_rows}</table>

<h2>2. 真实体验预算</h2>
<table><tr><th>指标</th><th>实测</th><th>预算</th><th>结论</th><th>说明</th></tr>{ux_rows}</table>
<p class="small muted">测量使用一次性数据根 + 随机端口 + 合成 10 MB 会话库；不读取、不启动用户的 OpenZen。</p>

<h2>3. 签核清单</h2>
<table><tr><th>条件</th><th>状态</th></tr>{check_rows}</table>
<h2>4. Tauri 真机手感（必须人工过一遍）</h2>
<ul class="small">
<li>□ 冷启动窗口 ≤ 2 s，无白屏</li>
<li>□ 发送一条消息，首 token ≤ 2 s，滚动不抖</li>
<li>□ 长回复（&gt; 10 KB）滚动 60 fps</li>
<li>□ 切会话 3 次，无重复/丢失消息</li>
<li>□ 审批弹窗出现 / 回答 / 消失</li>
<li>□ 关闭重开，会话与草稿还在</li>
<li>□ 5 分钟内存不单调增长，控制台无 <code>each_key_duplicate</code></li>
</ul>

<h2>4. 复跑</h2>
<pre><code>bash scripts/release-gate.sh --version {esc(args.version)}</code></pre>
<p class="small muted">修复循环见 <code>.opencode/skills/release-gate/SKILL.md</code> §5：只修 P0/P1，每条带修前会红的回归，最多 5 轮。</p>
</div></body></html>"""
    (out / "report.html").write_text(html)
    print(f"report: {out / 'report.html'}")
    print(f"verdict: {verdict}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
