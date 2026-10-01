#!/usr/bin/env bash
# Run a 3-node Raft cluster on localhost (no Docker needed).
#
#   node       api     cluster   data
#   timika-0   :8200   :8201     data/raft/timika-0
#   timika-1   :8210   :8211     data/raft/timika-1
#   timika-2   :8220   :8221     data/raft/timika-2
#
# Initialize ONE node (e.g. the UI on :5174 talks to timika-0), unseal it, then
# unseal the other two with the same keys — they join automatically.
# Ctrl+C stops all three. `RESET=1` wipes their data first.
# TLS: export TLS_CERT_FILE / TLS_KEY_FILE / TLS_CA_FILE (cert valid for 127.0.0.1).
set -euo pipefail
cd "$(dirname "$0")/../backend"

[[ "${RESET:-0}" == "1" ]] && rm -rf data/raft
cargo build --quiet
BIN=target/debug/anveesa-timika-backend
SCHEME=http; [[ -n "${TLS_CERT_FILE:-}" ]] && SCHEME=https
JOIN="$SCHEME://127.0.0.1:8200,$SCHEME://127.0.0.1:8210,$SCHEME://127.0.0.1:8220"
mkdir -p data/raft

trap 'kill 0' EXIT INT TERM
for i in 0 1 2; do
  api=$((8200 + i * 10)); cluster=$((api + 1))
  STORAGE=raft RAFT_NODE_ID="timika-$i" RAFT_PATH="data/raft/timika-$i" \
  BIND_ADDR="127.0.0.1:$api" CLUSTER_BIND_ADDR="127.0.0.1:$cluster" \
  API_ADDR="$SCHEME://127.0.0.1:$api" CLUSTER_ADDR="$SCHEME://127.0.0.1:$cluster" \
  RAFT_RETRY_JOIN="$JOIN" RAFT_SNAPSHOT_THRESHOLD="${RAFT_SNAPSHOT_THRESHOLD:-8192}" RAFT_TRAILING_LOGS="${RAFT_TRAILING_LOGS:-10000}" \
    "$BIN" 2>&1 | sed -u "s/^/[timika-$i] /" &
done
wait
