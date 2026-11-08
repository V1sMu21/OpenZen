#!/usr/bin/env bash
# Ten deterministic steward scenarios. Runs offline; no model/app required.
set -euo pipefail
cd "$(dirname "$0")/../.."
cargo test -p oz-steward --test steward_fault_injection -- --test-threads=1
