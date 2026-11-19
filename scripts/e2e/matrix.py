#!/usr/bin/env python3
"""OpenZen E2E matrix — 30 scenarios against a real `serve` process.

No real model, no network, no user data:

* a throwaway data root under /private/tmp (never ~/.openzen);
* a local mock model speaking OpenAI chat-completions SSE, scripted per
  scenario and routed by URL prefix;
* an approval bot that answers the approval gate from the server log.

Each scenario is a function that either returns (pass) or raises AssertionError
(fail). Infrastructure problems (server won't start, fixture missing) are
recorded as `error`, never as pass.

Output: target/caselab/e2e-results.jsonl (same schema as oz-caselab).
Exit: 0 all pass, 1 any fail, 3 any infra error.
"""

from __future__ import annotations

import argparse
import atexit
import base64
import glob
import hashlib
import json
import os
import re
import shutil
import signal
import socket
import subprocess
import sys
import threading
import time
import traceback
import urllib.error
import urllib.request
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from pathlib import Path

REPO = Path(__file__).resolve().parents[2]
BIN = REPO / "target/debug/openzen"
WORK = Path(f"/private/tmp/ozecf-e2e-{os.getpid()}")
LOG = WORK / "serve.log"


# ─────────────────────────── infrastructure ───────────────────────────

def free_port() -> int:
    s = socket.socket()
    s.bind(("127.0.0.1", 0))
    port = s.getsockname()[1]
    s.close()
    return port


PORT = free_port()
MOCK_PORT = free_port()


class Mock:
    """Scripted OpenAI-compatible SSE server, one script per URL prefix."""

    def __init__(self):
        self.scripts: dict[str, list[list[tuple]]] = {}
        self.counts: dict[str, int] = {}
        # Recorded protocol bytes (base64 per request) and their replay queue.
        # A recording is authoritative for that scenario; a scenario without
        # one falls back to its script (slow/stop keep their delays).
        self.recorded: dict[str, list[bytes]] = {}
        self.replay: dict[str, list[bytes]] = {}
        self.record_enabled = False
        outer = self

        class Handler(BaseHTTPRequestHandler):
            def log_message(self, *args):
                pass

            def do_POST(self):
                length = int(self.headers.get("content-length", 0))
                self.rfile.read(length)
                name = self.path.split("/")[2] if self.path.startswith("/s/") else "default"
                if name.startswith("http404"):
                    body = b'{"error":{"message":"Not Found"}}'
                    self.send_response(404)
                    self.send_header("content-type", "application/json")
                    self.send_header("content-length", str(len(body)))
                    self.end_headers()
                    self.wfile.write(body)
                    return
                turn = outer.turn(name)
                self.send_response(200)
                self.send_header("content-type", "text/event-stream")
                self.send_header("cache-control", "no-cache")
                self.send_header("connection", "close")
                self.end_headers()
                chunks: list[bytes] = []
                try:
                    if turn and turn[0][0] == "raw":
                        body = turn[0][1]
                        self.wfile.write(body)
                        self.wfile.flush()
                        chunks.append(body)
                    else:
                        for item in turn:
                            kind = item[0]
                            if kind == "sleep":
                                time.sleep(float(item[1]))
                            elif kind == "event":
                                payload = json.dumps(item[1]).encode()
                                chunk = b"data: " + payload + b"\n\n"
                                self.wfile.write(chunk)
                                self.wfile.flush()
                                chunks.append(chunk)
                        if not any(i[0] == "nodone" for i in turn):
                            done = b"data: [DONE]\n\n"
                            self.wfile.write(done)
                            self.wfile.flush()
                            chunks.append(done)
                    if outer.record_enabled and chunks:
                        outer.recorded.setdefault(name, []).append(b"".join(chunks))
                except (BrokenPipeError, ConnectionResetError):
                    pass

        self.httpd = ThreadingHTTPServer(("127.0.0.1", MOCK_PORT), Handler)
        self.thread = threading.Thread(target=self.httpd.serve_forever, daemon=True)
        self.thread.start()

    def turn(self, name: str) -> list[tuple]:
        queue = self.replay.get(name)
        if queue:
            body = queue.pop(0) if len(queue) > 1 else queue[0]
            return [("raw", body)]
        n = self.counts.get(name, 0)
        self.counts[name] = n + 1
        script = self.scripts.get(name) or [[("event", {"choices": [{"delta": {"content": "ok"}}]})]]
        return script[min(n, len(script) - 1)]

    def reset(self):
        self.counts.clear()

    def save_recordings(self, directory: Path) -> int:
        directory.mkdir(parents=True, exist_ok=True)
        saved = 0
        # Delays are part of these two scenarios; replaying them instantly
        # would remove the very race E2E-022/030 test.
        skip = {"mock_slow", "mock_stop"}
        for name, bodies in self.recorded.items():
            if not bodies or name in skip:
                continue
            path = directory / f"{name}.sse.jsonl"
            path.write_text(
                "".join(
                    json.dumps({"body_b64": base64.b64encode(b).decode()}) + "\n"
                    for b in bodies
                )
            )
            saved += 1
        return saved

    def load_recordings(self, directory: Path) -> int:
        loaded = 0
        for path in sorted(directory.glob("*.sse.jsonl")):
            name = path.name[: -len(".sse.jsonl")]
            if name in {"mock_slow", "mock_stop"}:
                continue
            bodies = []
            for line in path.read_text().splitlines():
                if not line.strip():
                    continue
                bodies.append(base64.b64decode(json.loads(line)["body_b64"]))
            if bodies:
                self.replay[name] = bodies
                loaded += 1
        return loaded

    def stop(self):
        self.httpd.shutdown()


def text_turn(text: str) -> list[tuple]:
    return [
        ("event", {"choices": [{"delta": {"role": "assistant"}}]}),
        ("event", {"choices": [{"delta": {"content": text}}]}),
        ("event", {"choices": [{"delta": {}, "finish_reason": "stop"}]}),
    ]


def tool_turn(tool: str, args: str, call_id: str = "call_1") -> list[tuple]:
    return [
        (
            "event",
            {
                "choices": [
                    {
                        "delta": {
                            "tool_calls": [
                                {
                                    "index": 0,
                                    "id": call_id,
                                    "type": "function",
                                    "function": {"name": tool, "arguments": ""},
                                }
                            ]
                        }
                    }
                ]
            },
        ),
        (
            "event",
            {
                "choices": [
                    {
                        "delta": {
                            "tool_calls": [
                                {"index": 0, "function": {"arguments": args}},
                            ]
                        }
                    }
                ]
            },
        ),
        ("event", {"choices": [{"delta": {}, "finish_reason": "tool_calls"}]}),
    ]


class Server:
    def __init__(self):
        self.proc: subprocess.Popen | None = None
        self.token = ""
        self.log = None

    def start(self):
        WORK.mkdir(mode=0o700, parents=True, exist_ok=True)
        try:
            os.chmod(WORK, 0o700)
        except OSError:
            pass
        if self.log is None:
            self.log = open(LOG, "ab", buffering=0)
            try:
                os.chmod(LOG, 0o600)
            except OSError:
                pass
        self.proc = subprocess.Popen(
            [
                str(BIN),
                "--config",
                str(WORK / "mykey.toml"),
                "--dir",
                str(WORK),
                "--assets",
                str(REPO / "assets"),
                "serve",
                "--port",
                str(PORT),
            ],
            cwd=str(REPO),
            stdout=self.log,
            stderr=subprocess.STDOUT,
            start_new_session=True,
        )
        deadline = time.time() + 40
        while time.time() < deadline:
            try:
                status, body = http("GET", "/api/health", token=False, timeout=1)
                if status == 200:
                    self.token = body["auth_token"]
                    return
            except Exception:
                pass
            if self.proc.poll() is not None:
                raise RuntimeError(f"server exited early: {self.tail()}")
            time.sleep(0.15)
        raise RuntimeError(f"server did not become healthy: {self.tail()}")

    def stop(self):
        if self.proc and self.proc.poll() is None:
            try:
                os.killpg(os.getpgid(self.proc.pid), signal.SIGTERM)
            except ProcessLookupError:
                pass
            try:
                self.proc.wait(timeout=10)
            except subprocess.TimeoutExpired:
                try:
                    os.killpg(os.getpgid(self.proc.pid), signal.SIGKILL)
                except ProcessLookupError:
                    pass
                self.proc.wait(timeout=5)
        self.proc = None

    def tail(self, n: int = 8) -> str:
        try:
            return "\n".join(LOG.read_text(errors="replace").splitlines()[-n:])
        except FileNotFoundError:
            return "(no log)"


def http(method: str, path: str, body=None, token=True, timeout=60):
    url = f"http://127.0.0.1:{PORT}{path}"
    data = None
    headers = {}
    if body is not None:
        data = json.dumps(body).encode()
        headers["content-type"] = "application/json"
    if token:
        headers["authorization"] = f"Bearer {SERVER.token}"
    req = urllib.request.Request(url, data=data, headers=headers, method=method)
    try:
        with urllib.request.urlopen(req, timeout=timeout) as resp:
            raw = resp.read()
            ctype = resp.headers.get("content-type", "")
            if "json" in ctype:
                return resp.status, json.loads(raw or b"{}")
            return resp.status, raw.decode(errors="replace")
    except urllib.error.HTTPError as e:
        raw = e.read()
        try:
            return e.code, json.loads(raw or b"{}")
        except Exception:
            return e.code, raw.decode(errors="replace")


class ApprovalBot(threading.Thread):
    """Answer approval requests by reading the server log (the UI is absent)."""

    def __init__(self):
        super().__init__(daemon=True)
        self.policy = lambda tool: "allow"
        self.seen: set[str] = set()
        self.decisions: list[tuple[str, str]] = []
        self.offset = 0
        self._stop_event = threading.Event()

    def run(self):
        while not self._stop_event.is_set():
            try:
                text = LOG.read_text(errors="replace")
            except FileNotFoundError:
                time.sleep(0.1)
                continue
            chunk = text[self.offset :]
            self.offset = len(text)
            for m in re.finditer(
                r"approval_needed sent: session=(\S+), tool=(.+?), id=([0-9a-f-]{36})", chunk
            ):
                sid, tool, rid = m.groups()
                if rid in self.seen:
                    continue
                self.seen.add(rid)
                decision = self.policy(tool)
                self.decisions.append((tool, decision))
                try:
                    http(
                        "POST",
                        f"/api/sessions/{sid}/approve",
                        {"request_id": rid, "decision": decision},
                    )
                except Exception:
                    pass
            time.sleep(0.08)

    def stop(self):
        self._stop_event.set()


SERVER = Server()
MOCK = Mock()
BOT = ApprovalBot()


# ─────────────────────────── scenarios ───────────────────────────

SCENARIOS: list[dict] = []


def scenario(cid: str, fc: list[str], severity: str, desc: str):
    def deco(fn):
        SCENARIOS.append(
            {"id": cid, "fc": fc, "severity": severity, "desc": desc, "fn": fn}
        )
        return fn

    return deco


def session(cid: str, model: str | None = None, message: str = "hi") -> tuple[str, dict]:
    sid = f"{cid}-{int(time.time() * 1000)}"
    body = {"message": message, "session_id": sid}
    if model:
        body["model_name"] = model
    status, resp = http("POST", "/api/chat", body, timeout=180)
    return sid, {"status": status, "body": resp}


def get_session(sid: str) -> dict:
    status, body = http("GET", f"/api/sessions/{sid}")
    assert status == 200, f"GET session {sid} → {status} {body}"
    return body


def workspace_files() -> list[str]:
    return [p for p in glob.glob(str(WORK / "**" / "*"), recursive=True) if Path(p).is_file()]


@scenario("E2E-001", ["FC-G1"], "P2", "health is public and reports ok")
def _(ctx):
    status, body = http("GET", "/api/health", token=False)
    assert status == 200 and body.get("status") == "ok", (status, body)


@scenario("E2E-002", ["FC-G1"], "P2", "models require a bearer token")
def _(ctx):
    status, _ = http("GET", "/api/models", token=False)
    assert status == 401, f"expected 401, got {status}"


@scenario("E2E-003", ["FC-B2"], "P2", "models list contains the configured mock")
def _(ctx):
    status, body = http("GET", "/api/models")
    assert status == 200 and any(m["name"] == "mock_text" for m in body), body


@scenario("E2E-004", ["FC-A5"], "P1", "create session returns a usable id")
def _(ctx):
    status, body = http("POST", "/api/sessions", {"name": "matrix-create"})
    assert status == 200 and body.get("session_id"), (status, body)
    status, got = http("GET", f"/api/sessions/{body['session_id']}")
    assert status == 200 and got.get("name") == "matrix-create", got


@scenario("E2E-005", ["FC-A5"], "P2", "list sessions includes the created one")
def _(ctx):
    _, created = http("POST", "/api/sessions", {"name": "matrix-list"})
    status, body = http("GET", "/api/sessions")
    ids = [s.get("id") for s in body.get("sessions", [])]
    assert status == 200 and created["session_id"] in ids, ids


@scenario("E2E-006", ["FC-A5"], "P2", "a fresh session is empty and idle")
def _(ctx):
    _, created = http("POST", "/api/sessions", {"name": "matrix-fresh"})
    got = get_session(created["session_id"])
    assert got["total_messages"] == 0 and got["status"] == "idle", got


@scenario("E2E-007", ["FC-A5"], "P2", "delete removes the session")
def _(ctx):
    _, created = http("POST", "/api/sessions", {"name": "matrix-del"})
    sid = created["session_id"]
    status, _ = http("DELETE", f"/api/sessions/{sid}")
    assert status == 200, status
    status, _ = http("GET", f"/api/sessions/{sid}")
    assert status == 404, f"deleted session still readable: {status}"


@scenario("E2E-008", ["FC-G1"], "P2", "unknown API route is a 404, not a panic")
def _(ctx):
    status, _ = http("GET", "/api/definitely-not-a-route")
    assert status == 404, status


@scenario("E2E-009", ["FC-F3"], "P2", "approving an unknown request id is a 404")
def _(ctx):
    status, _ = http(
        "POST",
        "/api/sessions/whatever/approve",
        {"request_id": "00000000-0000-0000-0000-000000000000", "decision": "allow"},
    )
    assert status == 404, status


@scenario("E2E-010", ["FC-F3"], "P2", "an invalid approval decision is a 400")
def _(ctx):
    status, _ = http(
        "POST",
        "/api/sessions/whatever/approve",
        {"request_id": "x", "decision": "allow-everything"},
    )
    assert status == 400, status


@scenario("E2E-011", ["FC-D5"], "P2", "stopping an idle session is a 404, not a hang")
def _(ctx):
    _, created = http("POST", "/api/sessions", {"name": "matrix-stop-idle"})
    status, _ = http("POST", f"/api/sessions/{created['session_id']}/stop")
    assert status == 404, status


@scenario("E2E-012", ["FC-A1"], "P2", "compressing an empty session is handled")
def _(ctx):
    _, created = http("POST", "/api/sessions", {"name": "matrix-compress"})
    status, _ = http("POST", f"/api/sessions/{created['session_id']}/compress")
    assert status in (200, 204), f"compress route answered {status}"


@scenario("E2E-013", ["FC-A5"], "P2", "resuming without a checkpoint is a 404")
def _(ctx):
    _, created = http("POST", "/api/sessions", {"name": "matrix-resume"})
    status, body = http("POST", f"/api/sessions/{created['session_id']}/resume", {})
    # No checkpoint for an idle session: a 404 is the documented answer.
    assert status == 404, f"resume of a checkpoint-less session answered {status}: {body}"


@scenario("E2E-014", ["FC-A2"], "P2", "checkpoint listing answers with JSON")
def _(ctx):
    status, body = http("GET", "/api/checkpoints")
    assert status == 200 and isinstance(body, (list, dict)), (status, body)


@scenario("E2E-015", ["FC-F3"], "P1", "session id traversal escapes nothing")
def _(ctx):
    status, _ = http("GET", "/api/sessions/..%2F..%2Fevil")
    assert status in (400, 404), status
    assert not (WORK / "evil").exists(), "traversal created a file"


@scenario("E2E-016", ["FC-G1"], "P1", "oversized chat body is rejected, server survives")
def _(ctx):
    big = "x" * (3 * 1024 * 1024)
    status, _ = http(
        "POST", "/api/chat", {"message": big, "session_id": "oversize"}, timeout=60
    )
    assert status in (400, 413, 422), f"expected 4xx, got {status}"
    status, _ = http("GET", "/api/health", token=False)
    assert status == 200, "server died after an oversized body"


@scenario("E2E-017", ["FC-A5"], "P2", "20 concurrent creates produce 20 unique ids")
def _(ctx):
    ids: list[str] = []
    lock = threading.Lock()

    def make(i):
        st, body = http("POST", "/api/sessions", {"name": f"conc-{i}"})
        if st == 200:
            with lock:
                ids.append(body["session_id"])

    threads = [threading.Thread(target=make, args=(i,)) for i in range(20)]
    [t.start() for t in threads]
    [t.join() for t in threads]
    assert len(ids) == 20 and len(set(ids)) == 20, f"{len(ids)} ids, {len(set(ids))} unique"


@scenario("E2E-018", ["FC-A5"], "P1", "messages survive a server restart")
def _(ctx):
    MOCK.scripts["mock_text"] = [text_turn("persist-me")]
    MOCK.reset()
    sid, res = session("e2e-018", "mock_text")
    assert res["status"] == 200, res
    SERVER.stop()
    SERVER.start()
    got = get_session(sid)
    assert "persist-me" in json.dumps(got["messages"], ensure_ascii=False), got


@scenario("E2E-019", ["FC-A5", "FC-G1"], "P0", "a corrupt sessions.json is preserved, not erased")
def _(ctx):
    SERVER.stop()
    path = WORK / "openzen/sessions.json"
    path.write_bytes(b"{ this is not json")
    SERVER.start()
    backups = glob.glob(str(WORK / "openzen/sessions.corrupt-*"))
    assert backups, "no backup created for the corrupt store"
    assert b"this is not json" in Path(backups[0]).read_bytes(), "backup content lost"
    status, _ = http("GET", "/api/sessions")
    assert status == 200, status


@scenario("E2E-020", ["FC-A5", "FC-I3"], "P1", "invalid UTF-8 sessions.json is preserved")
def _(ctx):
    SERVER.stop()
    path = WORK / "openzen/sessions.json"
    before = set(glob.glob(str(WORK / "openzen/sessions.corrupt-*")))
    payload = b"\xff\xfe{\"x\":1}"
    path.write_bytes(payload)
    SERVER.start()
    new = set(glob.glob(str(WORK / "openzen/sessions.corrupt-*"))) - before
    assert len(new) == 1, f"expected one new backup, got {sorted(new)}"
    assert Path(new.pop()).read_bytes() == payload, "backup content lost"


@scenario("E2E-021", ["FC-C2"], "P1", "a second server on the same port fails fast")
def _(ctx):
    second = subprocess.Popen(
        [
            str(BIN),
            "--config",
            str(WORK / "mykey.toml"),
            "--dir",
            str(WORK),
            "--assets",
            str(REPO / "assets"),
            "serve",
            "--port",
            str(PORT),
        ],
        cwd=str(REPO),
        stdout=subprocess.PIPE,
        stderr=subprocess.STDOUT,
        text=True,
    )
    try:
        out, _ = second.communicate(timeout=20)
    except subprocess.TimeoutExpired:
        second.kill()
        raise AssertionError("second server did not exit; it may have stolen the port")
    assert second.returncode != 0, f"second server exited 0: {out[-400:]}"
    assert re.search(r"address already in use|bind|AddrInUse", out, re.I), out[-400:]


@scenario("E2E-022", ["FC-D5"], "P1", "a second chat on the same session is a 409")
def _(ctx):
    MOCK.scripts["mock_slow"] = [
        sum(
            (
                [
                    ("event", {"choices": [{"delta": {"content": c}}]}),
                    ("sleep", 0.35),
                ]
                for c in "abcdef"
            ),
            [],
        )
        + [("event", {"choices": [{"delta": {}, "finish_reason": "stop"}]})]
    ]
    MOCK.reset()
    sid = "concurrent-1"
    done = {}

    def run():
        done["res"] = http(
            "POST", "/api/chat", {"message": "hi", "session_id": sid, "model_name": "mock_slow"},
            timeout=60,
        )

    t = threading.Thread(target=run)
    t.start()
    time.sleep(0.6)
    status, _ = http(
        "POST", "/api/chat", {"message": "again", "session_id": sid, "model_name": "mock_slow"},
        timeout=10,
    )
    assert status == 409, f"expected 409, got {status}"
    http("POST", f"/api/sessions/{sid}/stop")
    t.join(timeout=20)


@scenario("E2E-023", ["FC-G1"], "P2", "chat without a token is a 401")
def _(ctx):
    status, _ = http("POST", "/api/chat", {"message": "hi", "session_id": "noauth"}, token=False)
    assert status == 401, status


@scenario("E2E-024", ["FC-B1"], "P2", "SSE endpoint is reachable and typed")
def _(ctx):
    req = urllib.request.Request(f"http://127.0.0.1:{PORT}/api/events?session_id=none")
    try:
        with urllib.request.urlopen(req, timeout=3) as resp:
            assert resp.status == 200, resp.status
            assert "text/event-stream" in resp.headers.get("content-type", ""), resp.headers
    except urllib.error.HTTPError as e:
        raise AssertionError(f"GET /api/events → {e.code}")


@scenario("E2E-025", ["FC-B1"], "P1", "a text-only model turn reaches the user")
def _(ctx):
    MOCK.scripts["mock_text"] = [text_turn("e2e-ok")]
    MOCK.reset()
    _, res = session("e2e-025", "mock_text")
    assert res["status"] == 200, res
    assert res["body"].get("response") == "e2e-ok", res["body"]


@scenario("E2E-026", ["FC-B2"], "P1", "an in-workspace write executes and is persisted")
def _(ctx):
    MOCK.scripts["mock_write"] = [
        tool_turn("write", '{"file_path": "e2e_in.txt", "content": "hi"}'),
        text_turn("written"),
    ]
    MOCK.reset()
    _, res = session("e2e-026", "mock_write")
    assert res["status"] == 200, res
    hits = [p for p in workspace_files() if p.endswith("e2e_in.txt")]
    assert hits, f"e2e_in.txt not created under {WORK}: {workspace_files()[:10]}"


@scenario("E2E-027", ["FC-F3"], "P0", "an out-of-workspace write is denied when the user says no")
def _(ctx):
    target = f"/private/tmp/ozecf-outside-{os.getpid()}.txt"
    if os.path.exists(target):
        os.unlink(target)
    MOCK.scripts["mock_outside"] = [
        tool_turn("write", json.dumps({"file_path": target, "content": "no"})),
        text_turn("denied"),
    ]
    MOCK.reset()
    BOT.policy = lambda tool: "deny" if "outside workspace" in tool else "allow"
    BOT.decisions.clear()
    try:
        _, res = session("e2e-027", "mock_outside")
    finally:
        BOT.policy = lambda tool: "allow"
    assert res["status"] == 200, res
    assert not os.path.exists(target), "denied write still created the file"
    assert any(d == "deny" for _, d in BOT.decisions), f"no deny was issued: {BOT.decisions}"


@scenario("E2E-028", ["FC-B2", "FC-G1"], "P0", "truncated tool arguments never execute a half call")
def _(ctx):
    # A real truncation: the stream dies mid-arguments, before finish_reason.
    MOCK.scripts["mock_trunc"] = [
        tool_turn("write", '{"file_path": "trunc.txt", "content": "x')[:2]
        + [("nodone",)],
        text_turn("done"),
    ]
    MOCK.reset()
    sid, res = session("e2e-028", "mock_trunc")
    assert res["status"] == 200, res
    assert not any(p.endswith("trunc.txt") for p in workspace_files()), "half call was executed"
    status, _ = http("GET", "/api/health", token=False)
    assert status == 200, "server died on a truncated stream"


@scenario("E2E-029", ["FC-I1", "FC-C2"], "P1", "an upstream 404 ends the run with llm_error")
def _(ctx):
    MOCK.reset()
    _, res = session("e2e-029", "mock_404")
    body = res["body"]
    assert res["status"] == 200, res
    assert body.get("status") == "error", body
    assert "404" in json.dumps(body), f"404 is not visible in the failure: {body}"


@scenario("E2E-030", ["FC-D1"], "P0", "Stop interrupts a streaming turn")
def _(ctx):
    MOCK.scripts["mock_stop"] = [
        sum(
            (
                [
                    ("event", {"choices": [{"delta": {"content": c}}]}),
                    ("sleep", 0.25),
                ]
                for c in "abcdefghij"
            ),
            [],
        )
        + [("event", {"choices": [{"delta": {}, "finish_reason": "stop"}]})]
    ]
    MOCK.reset()
    sid = "stop-1"
    done = {}

    def run():
        done["res"] = http(
            "POST", "/api/chat", {"message": "long", "session_id": sid, "model_name": "mock_stop"},
            timeout=60,
        )

    t = threading.Thread(target=run)
    t.start()
    time.sleep(1.0)
    t0 = time.time()
    stop_status, _ = http("POST", f"/api/sessions/{sid}/stop")
    assert stop_status == 200, f"/stop answered {stop_status}; the run was not registered"
    t.join(timeout=10)
    elapsed = time.time() - t0
    assert not t.is_alive(), "chat did not return after Stop"
    assert elapsed < 5, f"Stop took {elapsed:.1f}s"
    status, _ = http("GET", f"/api/sessions/{sid}")
    assert status == 200, status
    body = done.get("res", (0, {}))[1]
    text = (body.get("response") or "") + json.dumps(body)
    assert "j" not in text, f"stream was not cut short: {body}"


# ─────────────────────────── runner ───────────────────────────


def write_config():
    (WORK / "workspace").mkdir(parents=True, exist_ok=True)
    (WORK / "openzen").mkdir(parents=True, exist_ok=True)
    models = {
        "mock_text": "mock_text",
        "mock_write": "mock_write",
        "mock_outside": "mock_outside",
        "mock_trunc": "mock_trunc",
        "mock_404": "http404",
        "mock_slow": "mock_slow",
        "mock_stop": "mock_stop",
    }
    lines = []
    for name, prefix in models.items():
        lines += [
            f"[{name}]",
            f'apibase = "http://127.0.0.1:{MOCK_PORT}/s/{prefix}/v1"',
            'apikey = "test"',
            'model = "mock"',
            "context_win = 32000",
            "",
        ]
    (WORK / "mykey.toml").write_text("\n".join(lines))


def outcome(cid: str, fc: list[str], sev: str, ok: bool, kind: str, msg: str, ms: int) -> dict:
    sig = hashlib.sha1(f"{cid.split('-')[0]}|{kind}|{msg[:160]}".encode()).hexdigest()[:16]
    return {
        "case_id": cid,
        "layer": "e2e",
        "fc": fc,
        "severity": sev,
        "status": "pass" if ok else ("fail" if kind == "assertion" else "error"),
        "duration_ms": ms,
        "error": None if ok else {"kind": kind, "message": msg[:2000]},
        "signature": "" if ok else sig,
        "repro": f"bash scripts/e2e/matrix.sh --only {cid}",
        "known": False,
    }


def main() -> int:
    ap = argparse.ArgumentParser()
    ap.add_argument("--only", action="append", default=[])
    ap.add_argument("--keep", action="store_true")
    ap.add_argument("--list", action="store_true")
    ap.add_argument("--record", action="store_true", help="record SSE bytes to corpus/sse/")
    ap.add_argument("--replay", action="store_true", help="serve recorded SSE bytes when present")
    args = ap.parse_args()

    if args.list:
        for s in SCENARIOS:
            print(s["id"], s["desc"])
        return 0
    record_dir = REPO / "corpus/sse"
    if args.replay:
        n = MOCK.load_recordings(record_dir)
        if n == 0:
            print(f"no recordings under {record_dir}; run with --record first", file=sys.stderr)
            return 3
        print(f"replay: {n} recorded scenario(s)")
    if args.record:
        MOCK.record_enabled = True
    if not BIN.exists():
        print(f"missing {BIN}; run: cargo build --bin openzen", file=sys.stderr)
        return 3
    # A crashed/killed previous run must not leak a serve process or a root
    # that later assertions mistake for their own.
    for old in glob.glob("/private/tmp/ozecf-e2e-*"):
        p = Path(old)
        if p == WORK:
            continue
        log = p / "serve.log"
        # A live matrix run keeps its log fresh; a dead one does not.
        stale = log.exists() and time.time() - log.stat().st_mtime > 900
        if stale or time.time() - p.stat().st_mtime > 7200:
            shutil.rmtree(p, ignore_errors=True)
    WORK.mkdir(mode=0o700, parents=True, exist_ok=True)
    write_config()
    if SERVER.log is not None:
        SERVER.log.close()
    SERVER.log = open(LOG, "wb", buffering=0)
    os.chmod(LOG, 0o600)
    BOT.start()
    results: list[dict] = []
    try:
        SERVER.start()
        for s in SCENARIOS:
            if args.only and s["id"] not in args.only:
                continue
            MOCK.reset()
            BOT.policy = lambda tool: "allow"
            started = time.time()
            try:
                s["fn"](None)
                results.append(outcome(s["id"], s["fc"], s["severity"], True, "assertion", "", int((time.time() - started) * 1000)))
                print(f"  PASS {s['id']}  {s['desc']}")
            except AssertionError as e:
                msg = str(e) or "assertion failed"
                results.append(outcome(s["id"], s["fc"], s["severity"], False, "assertion", msg, int((time.time() - started) * 1000)))
                print(f"  FAIL {s['id']}  {msg}")
            except Exception as e:
                msg = f"{type(e).__name__}: {e}"
                results.append(outcome(s["id"], s["fc"], s["severity"], False, "infra", msg, int((time.time() - started) * 1000)))
                print(f"  ERROR {s['id']}  {msg}")
                traceback.print_exc()
    finally:
        if args.record:
            saved = MOCK.save_recordings(record_dir)
            print(f"recorded: {saved} scenario(s) → {record_dir}")
        BOT.stop()
        BOT.join(timeout=1)
        SERVER.stop()
        MOCK.stop()
        if not args.keep:
            shutil.rmtree(WORK, ignore_errors=True)

    out = REPO / "target/caselab/e2e-results.jsonl"
    out.parent.mkdir(parents=True, exist_ok=True)
    if args.only and not results:
        print(f"no scenario matched {args.only}", file=sys.stderr)
        return 4
    out.write_text("".join(json.dumps(r) + "\n" for r in results))
    passed = sum(r["status"] == "pass" for r in results)
    failed = sum(r["status"] == "fail" for r in results)
    errored = sum(r["status"] == "error" for r in results)
    print(f"e2e: {len(results)} scenarios, pass={passed} fail={failed} error={errored}")
    print(f"jsonl: {out}")
    if errored:
        return 3
    return 1 if failed else 0


if __name__ == "__main__":
    sys.exit(main())
