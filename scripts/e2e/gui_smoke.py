#!/usr/bin/env python3
"""Autonomous GUI smoke for OpenZen (macOS, WKWebView).

Drives the real desktop app with CGEvent (typing/clicking), uses an isolated
data root + deterministic mock model, and asserts on the persisted session
JSON (the UI's source of truth) plus screenshots.

    python3 scripts/e2e/gui_smoke.py --keep

Never touches the user's running OpenZen: it copies the built .app, renames the
executable to `openzen-rg`, and runs with OPENZEN_DATA_DIR under /private/tmp.
"""

from __future__ import annotations

import argparse
import base64
import json
import os
import re
import shutil
import signal
import sqlite3
import socket
import subprocess
import sys
import time
from pathlib import Path

import Quartz
from AppKit import NSScreen
from Foundation import NSURL
import Vision
from PIL import Image

REPO = Path(__file__).resolve().parents[2]
BUNDLE = REPO / "target/release/bundle/macos/OpenZen.app"
SKILL = Path(os.environ.get("TAURI_E2E_SKILLS", str(Path.home() / ".dsh/skills/tauri-e2e/scripts")))
OUT = REPO / "target/gui-smoke"


def free_port() -> int:
    with socket.socket() as s:
        s.bind(("127.0.0.1", 0))
        return s.getsockname()[1]


def sh(cmd, **kw):
    return subprocess.run(cmd, capture_output=True, text=True, **kw)


def wait_until(fn, timeout=20, interval=0.15):
    end = time.time() + timeout
    last = None
    while time.time() < end:
        try:
            last = fn()
            if last:
                return last
        except Exception:  # noqa: BLE001
            pass
        time.sleep(interval)
    return last


def window_bounds(name="OpenZen"):
    wins = Quartz.CGWindowListCopyWindowInfo(
        Quartz.kCGWindowListOptionOnScreenOnly, Quartz.kCGNullWindowID
    )
    best = None
    for w in wins:
        if name.lower() not in w.get("kCGWindowOwnerName", "").lower():
            continue
        b = w.get("kCGWindowBounds", {})
        area = b.get("Width", 0) * b.get("Height", 0)
        if area > 100_000 and (best is None or area > best[0]):
            best = (area, b)
    return best[1] if best else None


def click(x, y, hold=90):
    sh(["python3", str(SKILL / "ozclick.py"), str(int(x)), str(int(y)), str(hold)])


def type_text(text):
    sh(["python3", str(SKILL / "oztype.py"), text])


def enter():
    for down in (True, False):
        Quartz.CGEventPost(
            Quartz.kCGHIDEventTap, Quartz.CGEventCreateKeyboardEvent(None, 36, down)
        )
        time.sleep(0.05)


class App:
    def __init__(self, root: Path, mock_port: int):
        self.root = root
        self.mock_port = mock_port
        self.app = Path("/private/tmp/OpenZenRG.app")
        self.proc = None
        self.mock = None
        self.bounds = None
        self.shots = 0

    def start_mock(self):
        self.root.mkdir(mode=0o700, parents=True, exist_ok=True)
        log = self.root / "mock.jsonl"
        self.mock = subprocess.Popen(
            [
                sys.executable,
                str(REPO / "scripts/e2e/gui_mock.py"),
                "--port",
                str(self.mock_port),
                "--log",
                str(log),
            ],
            cwd=str(REPO),
            stdout=subprocess.DEVNULL,
            stderr=subprocess.DEVNULL,
        )
        ok = wait_until(
            lambda: sh(["curl", "-s", "-m", "1", f"http://127.0.0.1:{self.mock_port}/v1/models"]).returncode
            == 0,
            10,
        )
        if not ok or self.mock.poll() is not None:
            raise RuntimeError(f"mock failed to start on {self.mock_port}")

    def prepare(self):
        shutil.rmtree(self.app, ignore_errors=True)
        shutil.copytree(BUNDLE, self.app)
        binary = self.app / "Contents/MacOS/openzen-tauri"
        binary.rename(binary.with_name("openzen-rg"))
        self.root.mkdir(mode=0o700, parents=True, exist_ok=True)
        (self.root / "mykey.toml").write_text(
            f'[e2e-mock]\napibase = "http://127.0.0.1:{self.mock_port}/v1"\n'
            'apikey = "test"\nmodel = "mock"\ncontext_win = 32000\n'
        )

    def find_text(self, *needles):
        """OCR the screen and return (x, y) for the first matching text."""
        shot = Path("/private/tmp/ozrg-ocr.png")
        subprocess.run(["screencapture", "-x", str(shot)], check=True)
        req = Vision.VNRecognizeTextRequest.alloc().init()
        req.setRecognitionLevel_(1)
        handler = Vision.VNImageRequestHandler.alloc().initWithURL_options_(
            NSURL.fileURLWithPath_(str(shot)), {}
        )
        handler.performRequests_error_([req], None)
        frame = NSScreen.mainScreen().frame()
        sw, sh = frame.size.width, frame.size.height
        needles = tuple(n.lower() for n in needles)
        for obs in req.results() or []:
            text = obs.topCandidates_(1)[0].string().lower()
            if any(n in text for n in needles):
                b = obs.boundingBox()
                x = (b.origin.x + b.size.width / 2) * sw
                y = (1 - (b.origin.y + b.size.height / 2)) * sh
                return int(x), int(y)
        return None

    def activate(self):
        try:
            from AppKit import NSWorkspace, NSApplicationActivateIgnoringOtherApps

            if not self.proc:
                return
            for a in NSWorkspace.sharedWorkspace().runningApplications():
                if a.processIdentifier() == self.proc.pid:
                    a.activateWithOptions_(NSApplicationActivateIgnoringOtherApps)
                    time.sleep(0.25)
                    return
        except Exception:  # noqa: BLE001
            pass

    def launch(self):
        log = open(self.root / "app.log", "ab")
        env = dict(os.environ, OPENZEN_DATA_DIR=str(self.root))
        self.proc = subprocess.Popen(
            [str(self.app / "Contents/MacOS/openzen-rg")],
            cwd=str(REPO),
            env=env,
            stdout=log,
            stderr=subprocess.STDOUT,
            start_new_session=True,
        )
        self.bounds = wait_until(window_bounds, 20)
        if not self.bounds:
            raise RuntimeError("app window did not appear")
        self.activate()
        time.sleep(0.8)

    def kill(self):
        if self.proc and self.proc.poll() is None:
            try:
                os.killpg(os.getpgid(self.proc.pid), signal.SIGTERM)
            except ProcessLookupError:
                pass
            try:
                self.proc.wait(timeout=8)
            except subprocess.TimeoutExpired:
                try:
                    os.killpg(os.getpgid(self.proc.pid), signal.SIGKILL)
                except ProcessLookupError:
                    pass
        self.proc = None

    def stop(self):
        self.kill()
        if self.mock and self.mock.poll() is None:
            self.mock.terminate()
            try:
                self.mock.wait(timeout=5)
            except subprocess.TimeoutExpired:
                self.mock.kill()

    def shot(self, name: str) -> Path:
        self.activate()
        self.shots += 1
        path = OUT / f"{self.shots:02d}-{name}.png"
        full = Path(f"/private/tmp/ozrg-gui-shot-{os.getpid()}.png")
        sh(["screencapture", "-x", str(full)])
        b = self.bounds
        img = Image.open(full)
        img.crop(
            (int(b["X"]), int(b["Y"]), int(b["X"] + b["Width"]), int(b["Y"] + b["Height"]))
        ).save(path)
        full.unlink(missing_ok=True)
        return path

    def click_rel(self, rx, ry, hold=90):
        self.activate()
        b = self.bounds
        click(b["X"] + rx, b["Y"] + ry, hold)

    def settle(self, timeout=30):
        """Wait until every session is Idle (and no ask_user) for 0.8s."""
        end = time.time() + timeout
        stable = 0
        while time.time() < end:
            d = self.sessions()
            quiet = bool(d) and all(
                e.get("status") == "Idle" and not e.get("pending_ask_user")
                for e in d.values()
            )
            stable = stable + 1 if quiet else 0
            if stable >= 6:
                return True
            time.sleep(0.25)
        return False

    def handoff(self, text: str):
        # `/erya <task>` is the keyboard path to the same Tauri command; the
        # button's x-position moves with the toolbar layout, the command does not.
        self.quiesce()
        self.click_rel(600, 676)
        time.sleep(0.15)
        for down in (True, False):
            e = Quartz.CGEventCreateKeyboardEvent(None, 0, down)  # A
            Quartz.CGEventSetFlags(e, Quartz.kCGEventFlagMaskCommand)
            Quartz.CGEventPost(Quartz.kCGHIDEventTap, e)
            time.sleep(0.04)
        for down in (True, False):
            e = Quartz.CGEventCreateKeyboardEvent(None, 51, down)  # Backspace
            Quartz.CGEventPost(Quartz.kCGHIDEventTap, e)
            time.sleep(0.04)
        type_text(text)
        time.sleep(0.35)
        point = self.find_text("hand to erya", "to erya", "交给尔雅")
        if point:
            click(*point, 100)
            time.sleep(0.5)
        else:
            self.click_rel(540, 723, 100)

    def fresh_chat(self):
        before = len(self.sessions())
        self.click_rel(52, 61, 100)
        wait_until(lambda: len(self.sessions()) == before + 1, 10)
        self.settle()

    def quiesce(self, quiet=2.0, timeout=45):
        """Idle sessions AND no mock traffic for `quiet` seconds.

        Post-run spec/summary calls keep the app busy without changing the
        session status; sending during them turns the next message into an
        intervention instead of a new run.
        """
        end = time.time() + timeout
        last = -1
        stable = 0.0
        while time.time() < end:
            d = self.sessions()
            idle = bool(d) and all(
                e.get("status") == "Idle" and not e.get("pending_ask_user")
                for e in d.values()
            )
            log = self.root / "mock.jsonl"
            size = log.stat().st_size if log.exists() else 0
            if idle and size == last:
                stable += 0.25
                if stable >= quiet:
                    return True
            else:
                stable = 0.0
            last = size
            time.sleep(0.25)
        return False

    def send(self, text: str):
        self.quiesce()
        self.activate()
        self.click_rel(600, 676)
        time.sleep(0.2)
        type_text(text)
        time.sleep(0.25)
        enter()

    def sessions(self):
        path = self.root / "openzen/sessions.json"
        if not path.exists():
            return {}
        try:
            return json.loads(path.read_text())
        except Exception:  # noqa: BLE001
            return {}

    def wait_reply(self, text: str, timeout=30):
        def check():
            for sid, e in self.sessions().items():
                msgs = e.get("messages", [])
                for i, m in enumerate(msgs):
                    if m.get("role") == "user" and text in str(m.get("content", "")):
                        after = [x for x in msgs[i + 1 :] if x.get("role") == "assistant"]
                        if after and e.get("status") == "Idle":
                            return (sid, e)
            return None

        return wait_until(check, timeout)

    def wait_idle(self, timeout=20):
        def check():
            d = self.sessions()
            return d if d and all(e.get("status") == "Idle" for e in d.values()) else None

        return wait_until(check, timeout)


def main() -> int:
    ap = argparse.ArgumentParser()
    ap.add_argument("--keep", action="store_true")
    ap.add_argument("--only", default="")
    ap.add_argument("--port", type=int, default=0, help="0 = pick a free port")
    ap.add_argument("--root", default=f"/private/tmp/ozrg-gui-suite-{os.getpid()}")
    args = ap.parse_args()
    root = Path(args.root)
    shutil.rmtree(root, ignore_errors=True)
    OUT.mkdir(parents=True, exist_ok=True)
    for old in OUT.glob("*.png"):
        old.unlink()
    results = []

    def case(cid, name, fn):
        if args.only and cid not in args.only.split(","):
            return
        t0 = time.time()
        try:
            detail, shot = fn()
            results.append(
                {"id": cid, "name": name, "status": "pass", "ms": int((time.time() - t0) * 1000),
                 "detail": detail, "shot": str(shot) if shot else ""}
            )
            print(f"PASS {cid} {name} — {detail}")
        except Exception as e:  # noqa: BLE001
            shot = app.shot(f"fail-{cid}") if app and app.bounds else None
            results.append(
                {"id": cid, "name": name, "status": "fail", "ms": int((time.time() - t0) * 1000),
                 "detail": f"{type(e).__name__}: {e}", "shot": str(shot) if shot else ""}
            )
            print(f"FAIL {cid} {name} — {e}")

    app = App(root, args.port or free_port())
    try:
        app.start_mock()
        app.prepare()
        app.launch()
        app.shot("start")

        def t_text():
            app.send("你好，请用一句话介绍你自己")
            got = app.wait_reply("请用一句话介绍你自己", 30)
            assert got, "no assistant reply"
            sid, e = got
            text = str(e["messages"][-1].get("content", ""))
            assert "mock" in text, text[:80]
            return f"reply {len(text)} chars", app.shot("text")

        case("GUI-01", "中文输入 + 发送 + 流式回复", t_text)
        app.quiesce()

        def t_long():
            app.send("长文测试")
            got = app.wait_reply("长文测试", 30)
            assert got, "no long reply"
            sid, e = got
            text = str(e["messages"][-1].get("content", ""))
            assert len(text) > 2000, f"only {len(text)} chars"
            app.click_rel(600, 400)
            for _ in range(6):
                e = Quartz.CGEventCreateScrollWheelEvent(None, Quartz.kCGScrollEventUnitLine, 1, -3)
                Quartz.CGEventPost(Quartz.kCGHIDEventTap, e)
                time.sleep(0.05)
            return f"{len(text)} chars, scrolled", app.shot("long")

        app.quiesce()
        case("GUI-02", "长回复渲染与滚动", t_long)

        def t_write():
            app.send("写文件")
            got = app.wait_reply("写文件", 30)
            assert got, "no reply"
            path = root / "workspace/gui-note.txt"
            assert path.exists(), f"missing {path}"
            return f"wrote {path.name}", app.shot("write")

        app.quiesce()
        case("GUI-03", "工具调用：工作区内写文件", t_write)

        def t_ask():
            app.send("提问")
            pending = wait_until(
                lambda: next(
                    (e for e in app.sessions().values() if e.get("pending_ask_user")), None
                ),
                15,
            )
            assert pending, "no pending ask_user"
            app.shot("ask")
            app.click_rel(600, 490)
            type_text("A")
            enter()
            done = wait_until(
                lambda: all(
                    not e.get("pending_ask_user") and e.get("status") == "Idle"
                    for e in app.sessions().values()
                ),
                20,
            )
            assert done, "ask_user did not resume"
            return "dialog answered, run resumed", app.shot("ask-done")

        def t_extreme_chain():
            app.handoff("极限依赖测试：A到D强依赖")
            db = root / "butler.db"
            assert wait_until(lambda: db.exists(), 15), "butler.db not created"

            def rows():
                try:
                    con = sqlite3.connect(db)
                    got = con.execute(
                        "select id,status,goal,deps_json from task order by rowid"
                    ).fetchall()
                    con.close()
                    return got
                except Exception:
                    return []

            terminal = {"verified", "unverified", "failed", "cancelled"}
            started = wait_until(
                lambda: len([r for r in rows() if r[1] in terminal]) >= 2,
                120,
            )
            assert started, f"extreme chain did not start: {rows()}"
            app.shot("extreme-before-restart")
            app.kill()
            time.sleep(1)
            app.launch()
            final = wait_until(
                lambda: (rows() if len(rows()) == 4 and all(r[1] in terminal for r in rows()) else None),
                180,
            )
            assert final, f"extreme chain did not finish after restart: {rows()}"
            by_goal = {r[2][:4]: r for r in final}
            assert {"阶段A", "阶段B", "阶段C", "阶段D"} <= set(by_goal), [r[2] for r in final]
            for prev, cur in (("阶段A", "阶段B"), ("阶段B", "阶段C"), ("阶段C", "阶段D")):
                dep_ids = json.loads(by_goal[cur][3])
                assert by_goal[prev][0] in dep_ids, f"{cur} does not depend on {prev}"
            ws = root / "workspace"
            a = json.loads((ws / "a.json").read_text())
            b = json.loads((ws / "b.json").read_text())
            c = (ws / "c.md").read_text()
            d = (ws / "d.txt").read_text()
            assert a.get("sum") == 31, a
            assert b.get("source_sum") == 31 and b.get("doubled") == 62, b
            assert "total=124" in c, c[:120]
            assert "validated=124" in d, d
            digest = wait_until(
                lambda: next(
                    (
                        str(m.get("content", ""))
                        for e in app.sessions().values()
                        if e.get("role") == "butler"
                        for m in e["messages"]
                        if "极限依赖任务" in str(m.get("content", ""))
                    ),
                    None,
                ),
                30,
            )
            assert digest, "no extreme-chain digest"
            app.click_rel(140, 771, 100)
            time.sleep(0.6)
            app.click_rel(455, 229, 100)
            time.sleep(0.6)
            shot = app.shot("extreme-panel")
            app.click_rel(1170, 68, 80)
            return "A(31) → B(62) → C(124) → D(124), survived restart", shot

        app.quiesce()
        case("GUI-04", "极限依赖链 A→B→C→D + 内容传递 + 中途重启", t_extreme_chain)


        def t_allow():
            outside = Path("/private/tmp/ozrg-gui-outside.txt")
            outside.unlink(missing_ok=True)
            app.send("审批")
            log = root / "logs/openzen.log"
            wait_until(
                lambda: log.exists() and "approval_needed" in log.read_text(errors="replace"),
                15,
            )
            app.shot("approval")
            app.click_rel(708, 442, 100)
            ok = wait_until(lambda: outside.exists(), 10)
            assert ok, "approved write did not land"
            return "approved; outside file written", app.shot("allow")

        app.quiesce()

        def t_deny():
            outside = Path("/private/tmp/ozrg-gui-outside.txt")
            outside.unlink(missing_ok=True)
            app.send("审批")
            log = root / "logs/openzen.log"
            before = len(re.findall("approval_needed", log.read_text(errors="replace"))) if log.exists() else 0
            wait_until(
                lambda: log.exists()
                and len(re.findall("approval_needed", log.read_text(errors="replace"))) > before,
                15,
            )
            app.shot("approval-deny")
            app.click_rel(508, 442, 100)
            time.sleep(2)
            assert not outside.exists(), "denied write created the file"
            return "denied; no file", app.shot("deny")

        app.quiesce()

        def t_stop():
            app.send("慢")
            time.sleep(2)
            app.click_rel(1140, 723, 100)
            done = app.wait_idle(10)
            assert done, "did not stop"
            slow = next(
                e for e in done.values()
                if any("慢" in str(m.get("content", "")) for m in e["messages"])
            )
            text = str(slow["messages"][-1].get("content", ""))
            reason = slow["messages"][-1].get("exitReason")
            assert reason == "stopped_by_user", f"exit reason {reason!r}"
            assert len(text) < 1200, f"stop did not truncate ({len(text)})"
            return f"stopped at {len(text)} chars", app.shot("stop")

        app.quiesce()

        def t_newchat():
            before = len(app.sessions())
            old_ids = set(app.sessions())
            app.click_rel(52, 61, 100)
            wait_until(lambda: len(app.sessions()) == before + 1, 10)
            assert len(app.sessions()) == before + 1, "new chat not created"
            app.send("新会话测试")
            got = wait_until(
                lambda: next(
                    (
                        (sid, e)
                        for sid, e in app.sessions().items()
                        if sid not in old_ids
                        and any("新会话测试" in str(m.get("content", "")) for m in e["messages"])
                        and any(m.get("role") == "assistant" for m in e["messages"])
                        and e.get("status") == "Idle"
                    ),
                    None,
                ),
                30,
            )
            assert got, "new chat reply missing"
            return f"{before + 1} sessions", app.shot("newchat")


        def t_settings():
            app.click_rel(28, 771, 100)
            time.sleep(0.8)
            assert app.proc.poll() is None, "app crashed opening settings"
            return "settings opened", app.shot("settings")

        def t_failure_propagation():
            app.handoff("极限失败测试：A失败阻断B和C")
            db = root / "butler.db"
            assert wait_until(lambda: db.exists(), 15), "butler.db not created"

            def rows():
                try:
                    con = sqlite3.connect(db)
                    got = con.execute(
                        "select id,status,goal from task order by rowid"
                    ).fetchall()
                    con.close()
                    return got
                except Exception:
                    return []

            started = wait_until(lambda: len(rows()) == 3, 30)
            assert started, f"tasks not created: {rows()}"
            settled = wait_until(
                lambda: (
                    rows()
                    if len(rows()) == 3
                    and all(r[1] in ("failed", "cancelled", "blocked", "queued") for r in rows())
                    and rows()[0][1] in ("failed", "cancelled")
                    and rows()[1][1] in ("failed", "cancelled", "blocked")
                    else None
                ),
                90,
            )
            assert settled, f"failure did not propagate: {rows()}"
            statuses = {r[2][:4]: r[1] for r in settled}
            assert statuses["阶段A"] in ("failed", "cancelled"), statuses
            assert statuses["阶段B"] in ("failed", "cancelled", "blocked"), statuses
            assert statuses["阶段C"] != "verified", statuses
            ws = root / "workspace"
            assert not (ws / "b.json").exists(), "B ran despite A failing"
            assert not (ws / "c.txt").exists(), "C ran despite B failing"
            digest = wait_until(
                lambda: next(
                    (
                        str(m.get("content", ""))
                        for e in app.sessions().values()
                        if e.get("role") == "butler"
                        for m in e["messages"]
                        if "失败" in str(m.get("content", "")) or "阻塞" in str(m.get("content", ""))
                    ),
                    None,
                ),
                30,
            )
            assert digest, "no failure digest"
            return f"A={statuses['阶段A']}, B={statuses['阶段B']}, C={statuses['阶段C']}", app.shot("failure")

        app.quiesce()
        case("GUI-05", "负向边界：A 失败必须阻断 B/C", t_failure_propagation)

        app.quiesce()
        case("GUI-06", "设置面板打开", t_settings)

        def t_restart():
            counts = {sid: len(e["messages"]) for sid, e in app.sessions().items()}
            app.kill()
            time.sleep(1)
            app.launch()
            after = {sid: len(e["messages"]) for sid, e in app.sessions().items()}
            assert after == counts, f"persistence mismatch {counts} != {after}"
            return f"{len(after)} sessions restored", app.shot("restart")

        app.quiesce()
        def t_butler():
            app.fresh_chat()
            app.handoff("派工测试：写一个 butler-note.txt")
            db = root / "butler.db"
            assert wait_until(lambda: db.exists(), 15), "butler.db not created"

            def terminal():
                try:
                    con = sqlite3.connect(db)
                    row = con.execute(
                        "select id,status,goal from task order by rowid desc limit 1"
                    ).fetchone()
                    con.close()
                    return row if row and row[1] in (
                        "verified", "unverified", "failed", "cancelled"
                    ) else None
                except Exception:
                    return None

            row = wait_until(terminal, 90)
            assert row, "task never reached a terminal status"
            tid, status, goal = row
            assert "butler-note.txt" in goal
            note = root / "workspace/butler-note.txt"
            assert note.exists(), "worker did not produce the deliverable"
            roles = [e.get("role") for e in app.sessions().values()]
            assert "butler" in roles and "worker" in roles, roles
            digest = wait_until(
                lambda: next(
                    (
                        str(m.get("content", ""))
                        for e in app.sessions().values()
                        if e.get("role") == "butler"
                        for m in e["messages"]
                        if "已完成" in str(m.get("content", ""))
                    ),
                    None,
                ),
                20,
            )
            assert digest, "no digest collected from the worker"
            return f"task {status}, worker produced {note.name}", app.shot("butler")

        case("GUI-08", "尔雅单任务派工 + 工人结果回收", t_butler)

        case("GUI-07", "重启后会话与消息恢复", t_restart)
    finally:
        app.stop()
        if not args.keep:
            shutil.rmtree(root, ignore_errors=True)

    passed = sum(1 for r in results if r["status"] == "pass")
    report = OUT / "report.html"
    cards = ""
    for r in results:
        img = ""
        if r["shot"] and Path(r["shot"]).exists():
            b64 = base64.b64encode(Path(r["shot"]).read_bytes()).decode()
            img = f'<img src="data:image/png;base64,{b64}" alt="{r["id"]}">'
        cards += f"""<section class="case {r['status']}"><h3>{r['id']} · {r['name']}
<span>{r['status'].upper()} · {r['ms']} ms</span></h3>
<p>{r['detail']}</p>{img}</section>"""
    report.write_text(
        f"""<!DOCTYPE html><html lang="zh-CN"><head><meta charset="utf-8">
<title>OpenZen GUI Smoke</title><style>
body{{background:#0b0d10;color:#c3cbd8;font:14px/1.6 -apple-system,"PingFang SC",sans-serif;margin:0}}
.wrap{{max-width:1100px;margin:0 auto;padding:36px 28px 80px}}
h1{{color:#e9e2cf;font-size:28px}}h2{{color:#cbb26a;border-top:1px solid #232a35;padding-top:18px}}
.case{{border:1px solid #232a35;border-radius:8px;padding:14px 16px;margin:14px 0;background:#11161d}}
.case h3{{margin:0 0 4px;color:#ded6c0;font-size:15px}}.case h3 span{{float:right;font:12px ui-monospace;color:#818d9e}}
.case.pass{{border-left:4px solid #5fd3a3}}.case.fail{{border-left:4px solid #e0707a}}
img{{max-width:100%;border:1px solid #232a35;border-radius:6px;margin-top:10px}}
code{{background:#151b24;padding:1px 5px;border-radius:4px;color:#d9cba6}}
</style></head><body><div class="wrap">
<h1>OpenZen 桌面端 GUI Smoke</h1>
<p>真实 CGEvent 点击/输入 · 隔离数据根 · 确定性 mock 模型 · 以 sessions.json 为真值。
<strong>{passed}/{len(results)} 通过</strong></p>
{cards}
</div></body></html>"""
    )
    print(f"\n{passed}/{len(results)} passed · report {report}")
    return 0 if passed == len(results) else 1


if __name__ == "__main__":
    raise SystemExit(main())
