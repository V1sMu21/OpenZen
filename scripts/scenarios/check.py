#!/usr/bin/env python3
"""Scenario ratchet: catalog and executable tests must agree.

Fails when a shipped scenario disappears from the Rust suite, when the total
drops below the baseline, or when a `manual-pass` scenario has no evidence note.
"""
import json
import re
import sys
from pathlib import Path

root = Path(__file__).resolve().parents[2]
catalog = json.loads((root / "scripts/scenarios/catalog.json").read_text())
source = (root / "crates/oz-steward/tests/scenario_suite.rs").read_text()
tests = set(re.findall(r"async fn ([a-z0-9_]+)\(", source))
errors = []

implemented = [s for s in catalog["scenarios"] if s["status"] == "implemented"]
if len(implemented) < catalog["baseline_implemented"]:
    errors.append(
        f"implemented {len(implemented)} < baseline {catalog['baseline_implemented']}"
    )
for s in implemented:
    if s["id"] not in tests:
        errors.append(f"missing test: {s['id']}")
for s in catalog["scenarios"]:
    if s["status"] in ("manual-pass", "covered") and not s.get("assert"):
        errors.append(f"manual scenario without evidence: {s['id']}")
    if s["status"] not in ("implemented", "manual-pass", "designed", "covered"):
        errors.append(f"bad status: {s['id']} = {s['status']}")

print(f"scenario catalog: {len(catalog['scenarios'])} total, "
      f"{len(implemented)} implemented, {len(tests)} rust tests")
for s in catalog["scenarios"]:
    print(f"  {s['status']:<12} {s['severity']} {s['feature']:<22} {s['id']}")
if errors:
    print("\n".join(f"FAIL {e}" for e in errors), file=sys.stderr)
    sys.exit(1)
print("SCENARIO RATCHET OK")
