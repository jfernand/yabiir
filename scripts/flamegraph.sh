#!/usr/bin/env bash
# Capture a flamegraph of src/bin/loadtest.rs under a mixed get/put/delete
# workload with merge running concurrently. See src/bin/loadtest.rs's own
# doc comment for the full flag list.
#
# Usage: scripts/flamegraph.sh [extra loadtest args...]
# Requires: cargo-flamegraph (cargo install flamegraph), perf, and
# kernel.perf_event_paranoid <= 1 (sudo sysctl -w kernel.perf_event_paranoid=1).

set -euo pipefail

DATA_DIR="${LOADTEST_DIR:-/tmp/loadtest-data}"
OUT="${FLAMEGRAPH_OUT:-/tmp/flamegraph.svg}"
CSV="${LOADTEST_CSV:-/tmp/loadtest.csv}"

cargo flamegraph --profile profiling --bin loadtest -o "$OUT" -- \
  "$DATA_DIR" \
  --duration-secs 20 \
  --warmup-secs 3 \
  --threads 4 \
  --keys 50000 \
  --value-size 256 \
  --merge-interval-secs 3 \
  --report-interval-secs 2 \
  --csv "$CSV" \
  "$@"

echo "flamegraph: $OUT"
echo "raw samples: $CSV"
