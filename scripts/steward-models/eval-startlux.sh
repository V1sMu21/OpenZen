#!/usr/bin/env bash
# Evaluate StartLux on OpenZen's labeled decisions.
#
# Input JSONL must contain the request/response pairs used by
# `startlux-decide eval`; STARTLUX_DATA defaults to the local benchmark path.
set -euo pipefail

MODEL="${STARTLUX_MODEL:-${1:-}}"
DATA="${STARTLUX_DATA:-docs/bench/steward-decisions.jsonl}"
OUT="${STARTLUX_OUT:-docs/verify/startlux-eval.json}"

if [[ -z "${MODEL}" ]]; then
  echo "usage: STARTLUX_MODEL=<mlx-model-dir> $0 [model]" >&2
  exit 2
fi
if [[ "${STARTLUX_LICENSE_ACK:-}" != "1" ]]; then
  echo "StartLux weights are CC BY-NC 4.0; set STARTLUX_LICENSE_ACK=1 after reading the license." >&2
  exit 2
fi
if [[ ! -f "${DATA}" ]]; then
  echo "missing labeled set: ${DATA}" >&2
  echo "create 50-100 OpenZen decisions first; do not claim calibration without it" >&2
  exit 2
fi
if ! command -v startlux-decide >/dev/null 2>&1; then
  echo "startlux-decide not found; pip install 'startlux-decide[mlx]'" >&2
  exit 2
fi

mkdir -p "$(dirname "${OUT}")"
startlux-decide eval --model "${MODEL}" --backend mlx --data "${DATA}" --out "${OUT}"
echo "[startlux] eval written to ${OUT}" >&2
