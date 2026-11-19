#!/usr/bin/env python3
"""Score ERME recall relevance from the runtime probe log.

The agent logs one line per recall:
  ERME recall probe q=<first 40 chars> hits=N topk=[0.32@L2:<snippet> | ...]

Given a golden set of (query, expected keywords), this reports hit@3 and
precision@3. It is deliberately log-based: it measures what was actually
injected into the prompt, on the user's real data, without a second store.
"""
import argparse
import glob
import json
import os
import sys

DEFAULT_LOG = os.path.expanduser("~/.openzen/logs/openzen.log")


def load_cases(path):
    with open(path, encoding="utf-8") as f:
        return json.load(f)["cases"]


def load_probes(patterns):
    probes = []
    for pattern in patterns:
        for path in sorted(glob.glob(pattern)):
            try:
                with open(path, encoding="utf-8", errors="replace") as f:
                    for line in f:
                        if "ERME recall probe q=" in line:
                            probes.append(line.rstrip("\n"))
            except OSError:
                continue
    return probes


def parse_probe(line):
    # q=<query> hits=N topk=[...]
    try:
        rest = line.split("ERME recall probe q=", 1)[1]
        query, rest = rest.split(" hits=", 1)
        hits, topk = rest.split(" topk=[", 1)
        topk = topk.rsplit("]", 1)[0]
        return query, int(hits), topk
    except (IndexError, ValueError):
        return None


def score(cases, probes):
    parsed = [p for p in (parse_probe(x) for x in probes) if p]
    rows = []
    for case in cases:
        q = case["query"]
        expect = [e.lower() for e in case["expect"]]
        # The probe stores the first 40 chars; match by prefix either way.
        probe = case.get("probe", q)
        matches = [
            p
            for p in parsed
            if p[0]
            and (
                q.startswith(p[0])
                or p[0].startswith(q[:40])
                or probe.lower() in p[0].lower()
            )
        ]
        if not matches:
            rows.append((q, "no-probe", 0, 0))
            continue
        _, hits, topk = matches[-1]
        low = topk.lower()
        hit = any(e in low for e in expect)
        # Count how many injected snippets matched any expected keyword.
        snippets = [s for s in topk.split(" | ") if s.strip()]
        good = sum(1 for s in snippets if any(e in s.lower() for e in expect))
        precision = good / len(snippets) if snippets else 0.0
        rows.append((q, "hit" if hit else "miss", hits, precision))
    return rows


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--log", default=None, help="log glob (default: openzen.log and rotations)")
    ap.add_argument("--set", default=os.path.join(os.path.dirname(__file__), "..", "docs", "erme-golden-set.json"))
    args = ap.parse_args()
    patterns = [args.log] if args.log else [DEFAULT_LOG, DEFAULT_LOG + ".*"]
    cases = load_cases(args.set)
    probes = load_probes(patterns)
    rows = score(cases, probes)
    print(f"{'query':38} {'result':8} {'hits':4} {'prec@3':6}")
    for q, result, hits, precision in rows:
        print(f"{q[:38]:38} {result:8} {hits:<4} {precision:.2f}")
    tested = [r for r in rows if r[1] != "no-probe"]
    hits = sum(1 for r in tested if r[1] == "hit")
    print()
    print(f"probes found: {len(probes)}; cases with probe: {len(tested)}/{len(rows)}; hit@3 = {hits}/{len(tested) if tested else 0}")
    return 0


if __name__ == "__main__":
    sys.exit(main())
