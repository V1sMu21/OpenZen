#!/usr/bin/env python3
"""Minimal mutation probe — proves the detectors can actually fail.

Three hand-picked mutations, each followed by the focused test that must kill
it. The source file is backed up byte-for-byte and restored in a `finally`;
the probe refuses to run if the backup hash does not match afterwards.

This is deliberately smaller than `cargo-mutants` (not installed here): it
answers one question — "would the new ECF tests notice if this logic were
broken?" — and records the answer.

Exit: 0 all mutations killed · 1 a mutant survived · 3 infrastructure error.
"""

from __future__ import annotations

import argparse
import atexit
import hashlib
import json
import os
import signal
import subprocess
import sys
import tempfile
import time
from pathlib import Path

REPO = Path(__file__).resolve().parents[2]

# Crash safety: the original bytes live on disk (`.mutbak`) and in memory.
# SIGKILL cannot be caught, but the next run restores any leftover backup
# before touching the source again.
_CURRENT: tuple[Path, bytes] | None = None


def _restore_current() -> None:
    global _CURRENT
    if _CURRENT is None:
        return
    path, data = _CURRENT
    try:
        path.write_bytes(data)
    except Exception:  # noqa: BLE001 - best effort on interpreter shutdown
        pass
    bak = path.with_suffix(path.suffix + ".mutbak")
    try:
        bak.unlink()
    except FileNotFoundError:
        pass
    _CURRENT = None


atexit.register(_restore_current)
signal.signal(signal.SIGTERM, lambda *_: sys.exit(3))

# (id, file, find, replace, test command)
MUTATIONS = [
    (
        "M1-alias-pass-disabled",
        "crates/oz-core-types/src/tool_args.rs",
        "        if out.contains_key(*canon) {\n            continue;\n        }",
        "        if false {\n            continue;\n        }",
        ["cargo", "test", "-p", "oz-testkit", "--test", "cases", "every_unit_case_passes"],
    ),
    (
        "M2-relative-path-cwd",
        "crates/oz-tools/src/file_ops.rs",
        "    if p.is_absolute() {\n        p.to_path_buf()\n    } else {\n        std::path::Path::new(&ctx.working_dir).join(p)\n    }",
        "    let _ = ctx;\n    p.to_path_buf()",
        ["cargo", "test", "-p", "oz-tools", "--lib", "relative_write_lands"],
    ),
    (
        "M3-eof-accepts-any-args",
        "crates/oz-llm/src/stream.rs",
        "    serde_json::from_str::<serde_json::Value>(raw.trim()).ok()\n}",
        "    let _ = raw;\n    Some(serde_json::Value::String(\"accepted\".into()))\n}",
        ["cargo", "test", "-p", "oz-llm", "--lib", "eof_flush_refuses"],
    ),
    (
        "M4-boundary-always-outside",
        "crates/oz-core-types/src/path_trust.rs",
        "    if resolved.starts_with(&root) {\n        None\n    } else {\n        Some(resolved)\n    }",
        "    let _ = root;\n    Some(resolved)",
        ["cargo", "test", "-p", "oz-core-types", "--test", "path_boundary"],
    ),
    (
        "M6-pattern-first-token-only",
        "crates/oz-safety/src/patterns.rs",
        "            let normalized = code.split_whitespace().collect::<Vec<_>>().join(\" \");",
        "            let normalized = code.split_whitespace().next().unwrap_or(\"\").to_string();",
        ["cargo", "test", "-p", "oz-testkit", "--test", "cases", "every_unit_case_passes"],
    ),
    (
        "M5-repair-always-refuses",
        "crates/oz-llm/src/stream.rs",
        "pub fn repair_parameter_style_args(raw: &str, tool: Option<&str>) -> Option<serde_json::Value> {",
        "pub fn repair_parameter_style_args(raw: &str, tool: Option<&str>) -> Option<serde_json::Value> {\n    if true {\n        return None;\n    }",
        ["cargo", "test", "-p", "oz-testkit", "--test", "cases", "wire_corpus_properties_hold"],
    ),
]


def sha(data: bytes) -> str:
    return hashlib.sha256(data).hexdigest()


def main() -> int:
    global _CURRENT
    ap = argparse.ArgumentParser()
    ap.add_argument("--only", action="append", default=[])
    ap.add_argument("--dry-run", action="store_true")
    ap.add_argument("--out", default=str(REPO / "target/caselab/mutation-results.jsonl"))
    args = ap.parse_args()

    chosen = [m for m in MUTATIONS if not args.only or m[0] in args.only]
    if not chosen:
        print("no mutations selected", file=sys.stderr)
        return 3
    if args.dry_run:
        for mid, path, _, _, cmd in chosen:
            print(f"{mid}: {path} → {' '.join(cmd)}")
        return 0

    results = []
    for mid, rel, find, repl, cmd in chosen:
        path = REPO / rel
        bak = path.with_suffix(path.suffix + ".mutbak")
        if bak.exists():
            path.write_bytes(bak.read_bytes())
            bak.unlink()
            print(f"  recovered {rel} from a leftover mutation backup", file=sys.stderr)
        original = path.read_bytes()
        before = sha(original)
        started = time.time()
        status = "survived"
        detail = ""
        try:
            text = original.decode()
            if text.count(find) != 1:
                raise RuntimeError(f"mutation anchor found {text.count(find)} times in {rel}")
            bak.write_bytes(original)
            _CURRENT = (path, original)  # type: ignore[assignment]
            path.write_text(text.replace(find, repl, 1))
            proc = subprocess.run(
                cmd,
                cwd=REPO,
                capture_output=True,
                text=True,
                timeout=600,
            )
            output = (proc.stdout or "") + (proc.stderr or "")
            if proc.returncode != 0:
                if "error[E" in output or "could not compile" in output:
                    status = "unviable"
                    detail = "mutation did not compile"
                elif "test result: FAILED" in output or "panicked at" in output:
                    status = "killed"
                    detail = f"test failed (exit {proc.returncode})"
                else:
                    # A process killed by a signal still proves the test did
                    # not pass; keep it as a kill but name the exit.
                    status = "killed"
                    detail = f"test exited {proc.returncode} without a result line"
            else:
                status = "survived"
                detail = "focused test still passed"
        except subprocess.TimeoutExpired:
            status = "error"
            detail = "test timed out — treat as infrastructure, not a kill"
        except Exception as e:  # noqa: BLE001 - probe must restore and report
            status = "error"
            detail = f"{type(e).__name__}: {e}"
        finally:
            _restore_current()
            if sha(path.read_bytes()) != before:
                print(f"FATAL: could not restore {rel}", file=sys.stderr)
                return 3
        ms = int((time.time() - started) * 1000)
        results.append(
            {"id": mid, "file": rel, "status": status, "detail": detail, "duration_ms": ms}
        )
        print(f"  {status.upper():8} {mid}  ({ms} ms)  {detail}")

    out = Path(args.out)
    out.parent.mkdir(parents=True, exist_ok=True)
    out.write_text("".join(json.dumps(r) + "\n" for r in results))
    killed = sum(r["status"] == "killed" for r in results)
    print(f"mutation probe: {killed}/{len(results)} killed → {out}")
    if any(r["status"] in ("error", "unviable") for r in results):
        return 3
    return 0 if killed == len(results) else 1


if __name__ == "__main__":
    sys.exit(main())
