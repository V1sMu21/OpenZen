#!/usr/bin/env bash
# Start the official StartLux-Decision MLX server.
#
# StartLux is NOT an oMLX chat model: Instant uses bidirectional prompt
# attention and a letter read-out. Serving it through a causal chat endpoint
# silently produces different answers. This script only starts the official
# `startlux-decide` process.
set -euo pipefail

MODEL="${1:-${STARTLUX_MODEL:-}}"
PORT="${STARTLUX_PORT:-8001}"

if [[ -z "${MODEL}" ]]; then
  echo "usage: $0 <path-to-StartLux-...-MLX-4bit>  (or STARTLUX_MODEL=...)" >&2
  echo "download: hf download startlux-models/StartLux-Decision-v1.1-4B-Instant-MLX-4bit --local-dir <path>" >&2
  exit 2
fi
if [[ "${STARTLUX_LICENSE_ACK:-}" != "1" ]]; then
  echo "StartLux weights are CC BY-NC 4.0 (code is Apache-2.0)." >&2
  echo "Set STARTLUX_LICENSE_ACK=1 after reading the license; commercial use needs a StartLux license." >&2
  exit 2
fi
if [[ "${PORT}" == "8000" ]]; then
  echo "port 8000 is reserved for oMLX; use 8001" >&2
  exit 2
fi
if ! command -v startlux-decide >/dev/null 2>&1; then
  echo "startlux-decide not found. Install with: pip install 'startlux-decide[mlx]'" >&2
  exit 2
fi

echo "[startlux] model=${MODEL} backend=mlx port=${PORT}" >&2
echo "[startlux] after start: curl -s http://127.0.0.1:${PORT}/health" >&2
exec startlux-decide serve --model "${MODEL}" --backend mlx --port "${PORT}"
