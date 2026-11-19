#!/usr/bin/env python3
"""Corpus ratchet — the corpus may grow, never shrink.

A release that deletes cases to get green is a regression, not a cleanup.
`--update` is the only way to lower the baseline and it must be an explicit,
reviewed commit.

Exit: 0 ok · 1 corpus shrank · 4 manifest unreadable.
"""

from __future__ import annotations

import argparse
import json
import sys
from pathlib import Path

REPO = Path(__file__).resolve().parents[2]
MANIFEST = REPO / "cases/manifest.json"
BASELINE = REPO / "cases/ratchet-baseline.json"
KEYS = ("unit_cases", "wire_seeds", "spec_drafts")


def main() -> int:
    ap = argparse.ArgumentParser()
    ap.add_argument("--update", action="store_true")
    args = ap.parse_args()
    try:
        manifest = json.loads(MANIFEST.read_text())
    except Exception as e:  # noqa: BLE001
        print(f"ratchet: cannot read {MANIFEST}: {e}", file=sys.stderr)
        return 4
    current = {k: int(manifest.get(k, 0)) for k in KEYS}
    if args.update or not BASELINE.exists():
        BASELINE.write_text(json.dumps(current, indent=2) + "\n")
        print(f"ratchet: baseline updated {current}")
        return 0
    baseline = json.loads(BASELINE.read_text())
    bad = []
    for k in KEYS:
        if current[k] < int(baseline.get(k, 0)):
            bad.append(f"{k}: {current[k]} < baseline {baseline[k]}")
    if bad:
        for b in bad:
            print(f"ratchet FAIL: {b}", file=sys.stderr)
        return 1
    print(f"ratchet: ok {current} (baseline { {k: baseline.get(k) for k in KEYS} })")
    return 0


if __name__ == "__main__":
    sys.exit(main())
