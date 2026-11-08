#!/usr/bin/env python3
"""Validate the steward decision dataset format and report its distribution.

This validates shape/coverage, not label truth. A synthetic set can pass this
validator and still be unsuitable for calibration; see docs/bench/README.md.
"""
import argparse, collections, json, sys
from pathlib import Path

REQUIRED = {"id", "language", "goal", "acceptance", "steps", "expected_verdict"}
VERDICTS = {"on_track", "at_risk", "off_track", "stalled", "blocked"}

def main() -> int:
    ap = argparse.ArgumentParser()
    ap.add_argument("path", nargs="?", default="docs/bench/steward-decisions.jsonl")
    ap.add_argument("--min", type=int, default=100)
    args = ap.parse_args()
    rows = []
    for i, line in enumerate(Path(args.path).read_text(encoding="utf-8").splitlines(), 1):
        if not line.strip():
            continue
        try:
            row = json.loads(line)
        except json.JSONDecodeError as e:
            print(f"{args.path}:{i}: invalid JSON: {e}", file=sys.stderr)
            return 1
        missing = REQUIRED - row.keys()
        if missing:
            print(f"{args.path}:{i}: missing keys {sorted(missing)}", file=sys.stderr)
            return 1
        if row["expected_verdict"] not in VERDICTS:
            print(f"{args.path}:{i}: bad verdict {row['expected_verdict']}", file=sys.stderr)
            return 1
        if not isinstance(row["steps"], list):
            print(f"{args.path}:{i}: steps must be a list", file=sys.stderr)
            return 1
        rows.append(row)
    if len(rows) < args.min:
        print(f"{len(rows)} rows < minimum {args.min}; set --min only for a smoke set", file=sys.stderr)
        return 1
    print(f"rows={len(rows)} languages={dict(collections.Counter(r['language'] for r in rows))}")
    print(f"verdicts={dict(collections.Counter(r['expected_verdict'] for r in rows))}")
    print(f"sources={dict(collections.Counter(r.get('source','unknown') for r in rows))}")
    return 0

if __name__ == "__main__":
    raise SystemExit(main())
