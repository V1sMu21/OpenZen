#!/usr/bin/env python3
"""Deterministic OpenAI-compatible mock for GUI E2E.

The desktop app talks to this exactly like a real provider; the scenario is
selected by the last user message so the GUI flow is reproducible and free.

    python3 scripts/e2e/gui_mock.py --port 18777 --log /private/tmp/gui-mock.jsonl
"""

from __future__ import annotations

import argparse
import json
import re
import threading
import time
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer

LOG_PATH = ""
TURNS: dict[str, int] = {}


def bump(key: str) -> int:
    TURNS[key] = TURNS.get(key, 0) + 1
    return TURNS[key]


def sse(obj) -> bytes:
    return b"data: " + json.dumps(obj, ensure_ascii=False).encode() + b"\n\n"


def chunk(delta, finish=None) -> bytes:
    return sse({"choices": [{"index": 0, "delta": delta, "finish_reason": finish}]})


def text_turn(text: str, delay: float = 0.02, pieces: int = 40):
    yield chunk({"role": "assistant"})
    step = max(1, len(text) // pieces)
    for i in range(0, len(text), step):
        yield chunk({"content": text[i : i + step]})
        time.sleep(delay)
    yield chunk({}, "stop")


def tool_turn(tool: str, args: dict):
    yield chunk(
        {
            "tool_calls": [
                {
                    "index": 0,
                    "id": "call_gui_1",
                    "type": "function",
                    "function": {"name": tool, "arguments": ""},
                }
            ]
        }
    )
    yield chunk(
        {
            "tool_calls": [
                {
                    "index": 0,
                    "function": {"arguments": json.dumps(args, ensure_ascii=False)},
                }
            ]
        }
    )
    yield chunk({}, "tool_calls")


class Handler(BaseHTTPRequestHandler):
    protocol_version = "HTTP/1.1"

    def log_message(self, *args):
        pass

    def _json(self, code, obj):
        raw = json.dumps(obj).encode()
        self.send_response(code)
        self.send_header("content-type", "application/json")
        self.send_header("content-length", str(len(raw)))
        self.end_headers()
        self.wfile.write(raw)

    def do_GET(self):
        if self.path.rstrip("/") in ("/v1/models", "/models"):
            return self._json(
                200,
                {
                    "object": "list",
                    "data": [{"id": "mock", "object": "model", "owned_by": "ecf"}],
                },
            )
        return self._json(404, {"error": {"message": "not found"}})

    def do_POST(self):
        length = int(self.headers.get("content-length", 0))
        body = json.loads(self.rfile.read(length) or b"{}")
        messages = body.get("messages", [])

        def flatten(c):
            if isinstance(c, str):
                return c
            if isinstance(c, list):
                return " ".join(
                    str(p.get("text", "")) for p in c if isinstance(p, dict)
                )
            return ""

        user_text = ""
        last_role = ""
        for m in messages:
            last_role = m.get("role", "")
            if m.get("role") == "user":
                user_text = flatten(m.get("content", ""))

        all_text = " ".join(flatten(m.get("content", "")) for m in messages)
        with open(LOG_PATH, "a", encoding="utf-8") as f:
            f.write(
                json.dumps(
                    {"t": time.time(), "last_role": last_role, "user": user_text[:200]},
                    ensure_ascii=False,
                )
                + "\n"
            )

        last_user = ""
        for m in messages:
            if m.get("role") == "user":
                last_user = flatten(m.get("content", ""))
        last = messages[-1] if messages else {}
        content = last.get("content", "")
        has_tool_result = (
            last.get("role") == "tool"
            or "tool_results" in last
            or (
                isinstance(content, list)
                and any(
                    isinstance(p, dict) and p.get("type") in ("tool_result", "tool_use")
                    for p in content
                )
            )
        )
        with open(LOG_PATH, "a", encoding="utf-8") as f:
            f.write(
                json.dumps(
                    {"t": time.time(), "last_role": last.get("role"), "user": last_user[:200]},
                    ensure_ascii=False,
                )
                + "\n"
            )

        def task_events(text: str):
            if "审批" in text:
                return tool_turn(
                    "write",
                    {
                        "file_path": "/private/tmp/ozrg-gui-outside.txt",
                        "content": "approved by gui e2e\n",
                    },
                )
            if "写文件" in text:
                return tool_turn(
                    "write", {"file_path": "gui-note.txt", "content": "hello from gui\n"}
                )
            if "提问" in text:
                return tool_turn(
                    "ask_user",
                    {"question": "请选择 A 还是 B？", "candidates": ["A", "B"]},
                )
            return text_turn("继续完成原始任务。", delay=0.01, pieces=8)

        def marker(text: str) -> str:
            # Own phase is the one on the [工人任务] line; upstream goals also
            # mention earlier phases, so "first match" is wrong.
            m = re.search(r"\[工人任务\]\s*阶段([A-D])", text)
            phase = m.group(1) if m else ""
            if not phase:
                found = re.findall(r"阶段([A-D])", text)
                phase = found[-1] if found else "A"
            return {
                "A": "sum=31",
                "B": "doubled=62",
                "C": "total=124",
                "D": "validated=124",
            }.get(phase, "")

        # Auxiliary summariser: always return a real summary so the quality
        # pipeline does not retry it.
        if "Summarize the conversation" in last_user:
            events = text_turn(
                "## Task\nGUI smoke.\n## Files\n- gui-note.txt\n## Outcome\nCompleted.",
                delay=0.005,
                pieces=10,
            )
        elif "[验收]" in last_user:
            events = text_turn(
                '{"passed": true, "defects": [], "confidence": 0.92}',
                delay=0.005,
                pieces=4,
            )
        elif "[SPEC]" in last_user:
            if bump("spec") == 1:
                events = tool_turn(
                    "write",
                    {
                        "file_path": "task_spec.md",
                        "content": "# Task Spec\n\n## Goals\nGUI smoke.\n\n## Acceptance\nFile exists.\n",
                    },
                )
            else:
                events = text_turn(f"task_spec.md 已写入；{marker(all_text)}", delay=0.005, pieces=6)
        elif "[CHECKLIST]" in last_user:
            if bump("checklist") == 1:
                m = re.search(r"\[(todo_[0-9a-f]+)\]", last_user)
                events = tool_turn(
                    "todoupdate",
                    {
                        "id": m.group(1) if m else "todo_0",
                        "status": "completed",
                        "content": "完成当前阶段",
                    },
                )
            else:
                events = text_turn(f"阶段完成 {marker(all_text)}", delay=0.005, pieces=6)
        elif "[PROTOCOL]" in last_user:
            if bump("protocol") == 1:
                events = tool_turn("todowrite", {"content": "完成当前阶段"})
            else:
                events = text_turn(f"待办已建立；{marker(all_text)}", delay=0.005, pieces=6)
        elif "[CONTINUE]" in last_user:
            if bump("continue") == 1:
                events = task_events(all_text)
            else:
                events = text_turn("已完成。", delay=0.005, pieces=6)
        elif "[工人任务]" in all_text and "生成数据写入 a.json" in all_text:
            events = text_turn("A 失败：无法生成数据。", delay=0.01, pieces=8)
        elif "[工人任务]" in all_text and any(
            x in all_text for x in ("a.json", "b.json", "c.md", "d.txt")
        ):
            m = re.search(r"阶段([A-D])", all_text)
            phase = m.group(1) if m else "A"
            if has_tool_result or bump("xworker:" + phase) > 1:
                events = text_turn(
                    f"{phase} 完成 {marker(all_text)}", delay=0.01, pieces=8
                )
            elif phase == "A":
                events = tool_turn(
                    "write", {"file_path": "a.json", "content": '{"sum":31,"values":[3,1,4,1,5,9,2,6]}'}
                )
            elif phase == "B":
                src = re.findall(r"sum=(\d+)", all_text)
                if not src:
                    events = text_turn("B 失败：上游缺少 sum", delay=0.01, pieces=6)
                else:
                    events = tool_turn(
                        "write",
                        {
                            "file_path": "b.json",
                            "content": '{"source_sum":%s,"doubled":62}' % src[-1],
                        },
                    )
            elif phase == "C":
                src = re.findall(r"doubled=(\d+)", all_text)
                total = int(src[-1]) * 2 if src else 0
                events = tool_turn(
                    "write",
                    {
                        "file_path": "c.md",
                        "content": f"# 阶段C 结论\n\ntotal={total}\n\n"
                        + ("依赖链 A→B→C 的数据一致性说明。\n" * 30),
                    },
                )
            else:
                src = re.findall(r"total=(\d+)", all_text)
                validated = src[-1] if src else "0"
                events = tool_turn(
                    "write",
                    {"file_path": "d.txt", "content": f"validated={validated}\n"},
                )
        elif "[工人任务]" in last_user:
            m = re.search(r"阶段([A-D])", last_user)
            name = f"{m.group(1).lower()}.txt" if m else "butler-note.txt"
            if has_tool_result or bump("worker:" + name) > 1:
                events = text_turn(f"已完成：写入 {name}。", delay=0.01, pieces=10)
            else:
                events = tool_turn(
                    "write",
                    {"file_path": name, "content": f"done by worker: {name}\n"},
                )
        elif "管家 digest" in last_user or "所有任务已结束" in last_user:
            if "极限失败测试" in all_text:
                events = text_turn(
                    "任务失败：阶段A 未产出 a.json，阶段B/C 已阻塞；未验证 0 个。",
                    delay=0.01,
                    pieces=16,
                )
            elif "极限依赖测试" in all_text:
                events = text_turn(
                    "已完成 4 个极限依赖任务（unverified）：a.json → b.json → c.md → d.txt，"
                    "sum=31 / doubled=62 / total=124 / validated=124。",
                    delay=0.01,
                    pieces=20,
                )
            elif "复杂派工测试" in all_text:
                events = text_turn(
                    "已完成 4 个任务（unverified）：a.txt / b.txt / c.txt / d.txt 均已生成；"
                    "证据：文件存在；未决：缺少构建/测试强证据。",
                    delay=0.01,
                    pieces=18,
                )
            else:
                events = text_turn(
                    "已完成 1 个任务（unverified）：butler-note.txt 已生成；"
                    "证据：文件存在；未决：缺少构建/测试强证据。",
                    delay=0.01,
                    pieces=16,
                )
        elif (
            "极限失败测试" in all_text
            and "[SPEC]" not in last_user
            and "[PROTOCOL]" not in last_user
            and "[工人任务]" not in last_user
            and "[验收]" not in last_user
            and "管家 digest" not in last_user
            and "所有任务已结束" not in last_user
        ):
            n = bump("butler:fail")
            goals = [
                ("阶段A 生成数据写入 a.json", ["a.json"]),
                ("阶段B 依赖 A 写入 b.json", ["b.json"]),
                ("阶段C 依赖 B 写入 c.txt", ["c.txt"]),
            ]
            if n <= len(goals):
                goal, deliverables = goals[n - 1]
                prev = re.findall(r'"task_id"\s*:\s*"([^"]+)"', all_text)
                args = {"goal": goal, "detail": goal, "deliverables": deliverables}
                if prev:
                    args["deps"] = [prev[-1]]
                events = tool_turn("task_create", args)
            else:
                events = text_turn("已派工 3 个失败测试任务。", delay=0.01, pieces=10)
        elif (
            "极限依赖测试" in all_text
            and "[SPEC]" not in last_user
            and "[PROTOCOL]" not in last_user
            and "[工人任务]" not in last_user
            and "[验收]" not in last_user
            and "管家 digest" not in last_user
            and "所有任务已结束" not in last_user
        ):
            n = bump("butler:extreme")
            goals = [
                ("阶段A 生成数据集并计算 sum，写入 a.json", ["a.json"]),
                ("阶段B 读取上游 sum 计算 doubled，写入 b.json", ["b.json"]),
                ("阶段C 读取上游 doubled 计算 total，写入 c.md", ["c.md"]),
                ("阶段D 校验上游 total，写入 d.txt", ["d.txt"]),
            ]
            if n <= len(goals):
                goal, deliverables = goals[n - 1]
                prev = re.findall(r'"task_id"\s*:\s*"([^"]+)"', all_text)
                args = {"goal": goal, "detail": goal, "deliverables": deliverables}
                if prev:
                    args["deps"] = [prev[-1]]
                events = tool_turn("task_create", args)
            else:
                events = text_turn("已派工 4 个极限依赖任务。", delay=0.01, pieces=14)
        elif (
            "复杂派工测试" in all_text
            and "管家 digest" not in all_text
            and "所有任务已结束" not in all_text
        ):
            n = bump("butler:complex")
            goals = [
                ("阶段A 写 a.txt", ["a.txt"]),
                ("阶段B 写 b.txt", ["b.txt"]),
                ("阶段C 写 c.txt", ["c.txt"]),
                ("阶段D 写 d.txt", ["d.txt"]),
            ]
            if n <= len(goals):
                goal, deliverables = goals[n - 1]
                prev = re.findall(r'"task_id"\s*:\s*"([^"]+)"', all_text)
                args = {
                    "goal": goal,
                    "detail": f"{goal}；完成后由验收器检查。",
                    "deliverables": deliverables,
                }
                if prev:
                    args["deps"] = [prev[-1]]
                events = tool_turn("task_create", args)
            else:
                events = text_turn("已派工 4 个任务，等待完成。", delay=0.01, pieces=14)
        elif "派工测试" in last_user or "尔雅测试" in last_user:
            if has_tool_result or bump("butler:single") > 1:
                events = text_turn("已派工：写 butler-note.txt，完成后汇报。", delay=0.01, pieces=12)
            else:
                events = tool_turn(
                    "task_create",
                    {
                        "goal": "写 butler-note.txt",
                        "detail": "在工作区写一个 butler-note.txt，内容为 done by worker。",
                        "deliverables": ["butler-note.txt"],
                    },
                )
        elif "慢" in last_user:
            events = text_turn(
                "这是一段慢速流式回复，用于验证停止按钮。" * 40, delay=0.25, pieces=40
            )
        elif "提问" in last_user:
            events = task_events("提问")
        elif "审批" in last_user:
            events = task_events("审批")
        elif "长文" in last_user:
            events = text_turn(
                "长文测试。\n" + ("OpenZen 的流式渲染应该保持顺滑，不抖动，不丢字。\n" * 120),
                delay=0.005,
                pieces=120,
            )
        elif "写文件" in last_user:
            events = task_events("写文件")
        else:
            events = text_turn(
                "你好，我是 OpenZen 的本地 mock 模型。这是一条用于 GUI 验证的确定性回复。",
                delay=0.02,
                pieces=24,
            )

        self.send_response(200)
        self.send_header("content-type", "text/event-stream")
        self.send_header("cache-control", "no-cache")
        self.send_header("connection", "close")
        self.end_headers()
        try:
            for e in events:
                self.wfile.write(e)
                self.wfile.flush()
            self.wfile.write(b"data: [DONE]\n\n")
            self.wfile.flush()
        except (BrokenPipeError, ConnectionResetError):
            pass


def main() -> int:
    global LOG_PATH
    ap = argparse.ArgumentParser()
    ap.add_argument("--port", type=int, default=18777)
    ap.add_argument("--log", default="/private/tmp/ozrg-gui-mock.jsonl")
    args = ap.parse_args()
    LOG_PATH = args.log
    open(LOG_PATH, "w").close()
    srv = ThreadingHTTPServer(("127.0.0.1", args.port), Handler)
    print(f"gui-mock listening on {args.port}", flush=True)
    threading.Thread(target=srv.serve_forever, daemon=True).start()
    try:
        while True:
            time.sleep(1)
    except KeyboardInterrupt:
        srv.shutdown()
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
