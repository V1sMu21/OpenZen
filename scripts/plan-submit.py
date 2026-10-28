#!/usr/bin/env python3
"""把一份方案投进 OpenZen 的方案审查收件箱。

OpenZen 每 2 秒扫一次 `<working_dir>/.openzen/plans/inbox/`，把投进来的 JSON
变成一份方案，并在右侧栏自动开一个「方案」页签等用户批准。

用法
----
    python3 scripts/plan-submit.py --title "把会话列表改一行" --file plan.md \
        --est "6–8 分钟" --option "稳=只改样式" --option "快=重写组件"

    # 不给 --workdir 时按 OPENZEN_WORKDIR → ~/.openzen/workspace 的顺序找
    OPENZEN_WORKDIR=/path/to/project python3 scripts/plan-submit.py ...

退出码：0 投递成功；1 参数/写入失败。
"""
from __future__ import annotations

import argparse
import json
import os
import sys
from pathlib import Path


def resolve_workdir(explicit: str | None) -> Path:
    if explicit:
        return Path(explicit).expanduser()
    env = os.environ.get("OPENZEN_WORKDIR")
    if env:
        return Path(env).expanduser()
    return Path.home() / ".openzen" / "workspace"


def main() -> int:
    ap = argparse.ArgumentParser(description="投递一份待审方案")
    ap.add_argument("--title", required=True)
    ap.add_argument("--file", default="", help="方案正文（markdown 文件）")
    ap.add_argument("--md", default="", help="方案正文（直接给字符串）")
    ap.add_argument("--summary", default="", help="一句话摘要")
    ap.add_argument("--est", default="", help="预计耗时，例如 '6–8 分钟'")
    ap.add_argument(
        "--option",
        action="append",
        default=[],
        help="候选档位，格式 label=说明，可重复",
    )
    ap.add_argument("--workdir", default=None, help="OpenZen 工作目录（默认见文档）")
    ap.add_argument("--dry-run", action="store_true")
    args = ap.parse_args()

    if args.file:
        markdown = Path(args.file).read_text(encoding="utf-8")
    elif args.md:
        markdown = args.md
    else:
        markdown = sys.stdin.read()

    options = []
    for raw in args.option:
        label, _, detail = raw.partition("=")
        options.append({"label": label.strip(), "detail": detail.strip()})

    payload = {
        "title": args.title,
        "summary": args.summary,
        "markdown": markdown,
        "options": options,
        "est_cost": args.est,
    }

    workdir = resolve_workdir(args.workdir)
    inbox = workdir / ".openzen" / "plans" / "inbox"
    if args.dry_run:
        print(json.dumps({"inbox": str(inbox), "payload": payload}, ensure_ascii=False, indent=2))
        return 0

    try:
        inbox.mkdir(parents=True, exist_ok=True)
        stamp = __import__("time").strftime("%Y%m%d-%H%M%S")
        target = inbox / f"plan-{stamp}.json"
        target.write_text(json.dumps(payload, ensure_ascii=False, indent=2), encoding="utf-8")
    except OSError as e:
        print(f"[error] 写入失败：{e}", file=sys.stderr)
        return 1

    print(f"[plan] 已投递：{target}")
    print("[plan] OpenZen 会在 2 秒内把它变成右侧栏的待审方案。")
    return 0


if __name__ == "__main__":
    sys.exit(main())
