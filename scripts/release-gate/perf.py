#!/usr/bin/env python3
"""UX budgets for the release gate.

Measures a *fresh* server (isolated data root, random port) — never the user's
~/.openzen.  Emits JSON so the gate report can show measured-vs-budget.

    python3 scripts/release-gate/perf.py --json target/release-gate/perf.json
"""

from __future__ import annotations

import argparse
import json
import os
import shutil
import socket
import statistics
import subprocess
import sys
import time
import urllib.error
import urllib.request
from pathlib import Path

REPO = Path(__file__).resolve().parents[2]
BIN = REPO / "target/debug/openzen"


def free_port() -> int:
    with socket.socket() as s:
        s.bind(("127.0.0.1", 0))
        return s.getsockname()[1]


def http(port: int, token: str, method: str, path: str, body=None, timeout=10):
    headers = {}
    data = None
    if body is not None:
        data = json.dumps(body).encode()
        headers["content-type"] = "application/json"
    if token:
        headers["authorization"] = f"Bearer {token}"
    req = urllib.request.Request(
        f"http://127.0.0.1:{port}{path}", data=data, headers=headers, method=method
    )
    with urllib.request.urlopen(req, timeout=timeout) as resp:
        raw = resp.read()
        if "json" in resp.headers.get("content-type", ""):
            return resp.status, json.loads(raw or b"{}")
        return resp.status, raw


def seed_sessions(root: Path, count: int = 40, msgs: int = 80, chars: int = 3000) -> int:
    """Synthesize a realistic store (~10 MB) so list/load budgets are not
    measured on an empty install.  No user data is read or written."""
    now = "2026-11-18T00:00:00Z"
    store = {}
    body = "x" * chars
    for i in range(count):
        sid = f"rg-big-{i:03}"
        messages = [
            {"role": "user" if j % 2 == 0 else "assistant", "content": f"m{j}-" + body}
            for j in range(msgs)
        ]
        store[sid] = {
            "info": {
                "id": sid,
                "name": f"big-{i}",
                "created_at": now,
                "status": "Idle",
                "message_count": msgs,
                "role": "worker",
            },
            "status": "Idle",
            "messages": messages,
            "created_at": now,
            "project_id": None,
            "working_dir": None,
            "todos": [],
            "revision": 0,
            "role": "worker",
        }
    path = root / "openzen/sessions.json"
    path.write_text(json.dumps(store))
    return path.stat().st_size


def pct(values: list[float], q: float) -> float:
    if not values:
        return 0.0
    ordered = sorted(values)
    idx = min(len(ordered) - 1, max(0, round(q * (len(ordered) - 1))))
    return ordered[idx]


def main() -> int:
    ap = argparse.ArgumentParser()
    ap.add_argument("--json", default=str(REPO / "target/release-gate/perf.json"))
    ap.add_argument("--bin", default=str(BIN))
    ap.add_argument("--keep", action="store_true")
    ap.add_argument("--seed-mb", type=int, default=0, help="seed a synthetic store of ~N MB")
    args = ap.parse_args()
    bin_path = Path(args.bin)
    if not bin_path.exists():
        print(f"missing {bin_path}; run: cargo build --bin openzen", file=sys.stderr)
        return 3

    root = Path(f"/private/tmp/ozrg-{os.getpid()}")
    port = free_port()
    log = root / "serve.log"
    proc = None
    token = ""
    metrics: dict[str, dict] = {}
    infra_error = ""
    try:
        root.mkdir(mode=0o700, parents=True)
        (root / "openzen").mkdir(exist_ok=True)
        seeded_mb = 0.0
        if args.seed_mb:
            # 40 sessions × 80 messages × 3 KB ≈ 9.6 MB.
            bytes_ = seed_sessions(root)
            seeded_mb = bytes_ / (1024 * 1024)
            print(f"seeded synthetic store: {seeded_mb:.1f} MB", file=sys.stderr)
            metrics["seeded_store_mb"] = {
                "value": round(seeded_mb, 2),
                "budget": 20,
                "unit": "MB",
                "verdict": "pass" if seeded_mb <= 20 else "fail",
                "note": "synthetic 40×80 messages",
            }
        (root / "mykey.toml").write_text(
            '[mock]\napibase = "http://127.0.0.1:9/v1"\napikey = "test"\n'
            'model = "mock"\ncontext_win = 32000\n'
        )
        started = time.time()
        with open(log, "wb") as out:
            proc = subprocess.Popen(
                [
                    str(bin_path),
                    "--config",
                    str(root / "mykey.toml"),
                    "--dir",
                    str(root),
                    "--assets",
                    str(REPO / "assets"),
                    "serve",
                    "--port",
                    str(port),
                ],
                cwd=str(REPO),
                stdout=out,
                stderr=subprocess.STDOUT,
                start_new_session=True,
            )
            deadline = time.time() + 30
            while time.time() < deadline:
                try:
                    with urllib.request.urlopen(
                        f"http://127.0.0.1:{port}/api/health", timeout=1
                    ) as resp:
                        if resp.status == 200:
                            token = json.loads(resp.read())["auth_token"]
                            break
                except Exception:
                    pass
                if proc.poll() is not None:
                    raise RuntimeError("server exited during startup")
                time.sleep(0.05)
            else:
                raise RuntimeError("health did not come up in 30s")
        boot_ms = (time.time() - started) * 1000
        _, listed = http(port, token, "GET", "/api/sessions", timeout=10)
        items = listed.get("sessions", listed) if isinstance(listed, dict) else listed
        loaded = len(items) if isinstance(items, list) else 0
        if args.seed_mb:
            metrics["seeded_sessions_loaded"] = {
                "value": loaded,
                "budget": 40,
                "unit": "sessions",
                "verdict": "pass" if loaded >= 40 else "fail",
                "note": "the synthetic store must actually load, not be renamed corrupt",
            }
        boot_budget = 5000 if args.seed_mb else 3000
        metrics["cold_start_ms"] = {
            "value": round(boot_ms, 1),
            "budget": boot_budget,
            "unit": "ms",
            "verdict": "pass" if boot_ms <= boot_budget else "fail",
            "note": "spawn → /api/health 200"
            + (f" (with {seeded_mb:.1f} MB store)" if args.seed_mb else ""),
        }

        def time_calls(n: int, fn):
            samples = []
            for _ in range(n):
                t0 = time.perf_counter()
                fn()
                samples.append((time.perf_counter() - t0) * 1000)
            return samples

        time_calls(3, lambda: http(port, token, "GET", "/api/health", timeout=5))
        models = time_calls(20, lambda: http(port, token, "GET", "/api/models", timeout=5))
        metrics["models_p95_ms"] = {
            "value": round(pct(models, 0.95), 1),
            "budget": 100,
            "unit": "ms",
            "verdict": "pass" if pct(models, 0.95) <= 100 else "fail",
            "note": "GET /api/models ×20",
        }

        created = []
        create_ms = []
        for i in range(100):
            t0 = time.perf_counter()
            _, body = http(port, token, "POST", "/api/sessions", {"name": f"rg-{i}"})
            create_ms.append((time.perf_counter() - t0) * 1000)
            sid = body.get("id") or body.get("session_id")
            if sid:
                created.append(sid)
        time.sleep(0.4)
        lists = time_calls(20, lambda: http(port, token, "GET", "/api/sessions", timeout=10))
        metrics["sessions_100_p95_ms"] = {
            "value": round(pct(lists, 0.95), 1),
            "budget": 300,
            "unit": "ms",
            "verdict": "pass" if pct(lists, 0.95) <= 300 else "fail",
            "note": "GET /api/sessions with 140 sessions (40 seeded + 100 created) ×20",
        }
        if created:
            detail = time_calls(
                20,
                lambda: http(port, token, "GET", f"/api/sessions/{created[0]}", timeout=10),
            )
            metrics["session_detail_p95_ms"] = {
                "value": round(pct(detail, 0.95), 1),
                "budget": 500,
                "unit": "ms",
                "verdict": "pass" if pct(detail, 0.95) <= 500 else "fail",
                "note": "GET /api/sessions/:id ×20",
            }
        metrics["create_session_p95_ms"] = {
            "value": round(pct(create_ms, 0.95), 1),
            "budget": 150,
            "unit": "ms",
            "verdict": "pass" if pct(create_ms, 0.95) <= 150 else "fail",
            "note": "POST /api/sessions ×100",
        }

        # SSE is a long-lived stream: "first byte" only arrives on an event.
        # The user-perceived cost is the handshake (headers), so measure that.
        sid = created[0] if created else "probe"
        handshake = None
        try:
            req = urllib.request.Request(
                f"http://127.0.0.1:{port}/api/events?session_id={sid}",
                headers={"authorization": f"Bearer {token}"},
            )
            t0 = time.perf_counter()
            resp = urllib.request.urlopen(req, timeout=3)
            handshake = (time.perf_counter() - t0) * 1000
            resp.close()
        except Exception:  # noqa: BLE001 - a failed probe is a skip, not an infra stop
            handshake = None
        metrics["sse_handshake_ms"] = {
            "value": round(handshake, 1) if handshake is not None else None,
            "budget": 500,
            "unit": "ms",
            "verdict": "pass"
            if handshake is not None and handshake <= 500
            else ("skip" if handshake is None else "fail"),
            "note": "GET /api/events response headers",
        }

        chunk_files = sorted(
            (REPO / "frontends/dist/assets").glob("*.js"),
            key=lambda p: p.stat().st_size,
            reverse=True,
        )
        if chunk_files:
            largest = chunk_files[0]
            kb = largest.stat().st_size / 1024
            metrics["largest_js_chunk_kb"] = {
                "value": round(kb, 1),
                "budget": 700,
                "warn": 550,
                "unit": "KB",
                "verdict": "pass" if kb <= 700 else "fail",
                "note": largest.name,
            }

        sessions_json = root / "openzen/sessions.json"
        if sessions_json.exists():
            mb = sessions_json.stat().st_size / (1024 * 1024)
            metrics["sessions_json_mb"] = {
                "value": round(mb, 2),
                "budget": 20,
                "unit": "MB",
                "verdict": "pass" if mb <= 20 else "fail",
                "note": str(sessions_json),
            }
        cp = root / "checkpoints"
        cp_mb = (
            sum(p.stat().st_size for p in cp.rglob("*") if p.is_file()) / (1024 * 1024)
            if cp.exists()
            else 0
        )
        metrics["checkpoints_mb"] = {
            "value": round(cp_mb, 2),
            "budget": 100,
            "unit": "MB",
            "verdict": "pass" if cp_mb <= 100 else "fail",
            "note": str(cp),
        }

        errors = []
        if log.exists():
            for line in log.read_text(errors="replace").splitlines():
                if " ERROR " in line or "panicked at" in line:
                    errors.append(line)
        metrics["startup_log_errors"] = {
            "value": len(errors),
            "budget": 0,
            "unit": "lines",
            "verdict": "pass" if not errors else "fail",
            "note": errors[0][:160] if errors else "",
        }
    except Exception as e:  # noqa: BLE001
        infra_error = str(e)
    finally:
        if proc and proc.poll() is None:
            try:
                os.killpg(os.getpgid(proc.pid), 15)
            except ProcessLookupError:
                pass
            try:
                proc.wait(timeout=10)
            except subprocess.TimeoutExpired:
                try:
                    os.killpg(os.getpgid(proc.pid), 9)
                except ProcessLookupError:
                    pass
        if not args.keep:
            shutil.rmtree(root, ignore_errors=True)

    failed = [k for k, v in metrics.items() if v["verdict"] == "fail"]
    payload = {
        "metrics": metrics,
        "failed": failed,
        "infra_error": infra_error,
        "data_root": str(root),
    }
    out = Path(args.json)
    out.parent.mkdir(parents=True, exist_ok=True)
    out.write_text(json.dumps(payload, indent=2))
    print(f"{'metric':24} {'value':>10} {'budget':>8} verdict")
    for k, v in metrics.items():
        val = "-" if v["value"] is None else v["value"]
        print(f"{k:24} {str(val):>10} {v['budget']:>8} {v['verdict']}")
    if infra_error:
        print(f"infra: {infra_error}", file=sys.stderr)
        return 3
    return 1 if failed else 0


if __name__ == "__main__":
    sys.exit(main())
