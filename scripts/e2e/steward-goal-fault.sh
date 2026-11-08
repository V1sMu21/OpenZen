#!/usr/bin/env bash
# Eight Goal Mode fault scenarios (G4). Offline; no model/app required.
set -euo pipefail
cd "$(dirname "$0")/../.."
cargo test -p oz-steward --test goal_fault_injection -- --test-threads=1
