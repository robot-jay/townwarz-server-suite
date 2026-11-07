#!/usr/bin/env bash
set -euo pipefail

# Run from the workspace root. All endpoints belong to this CI job's services.
: "${TEST_DATABASE_URL:?Set TEST_DATABASE_URL to an isolated PostgreSQL database}"
: "${TEST_NATS_URL:?Set TEST_NATS_URL to an isolated NATS server}"
: "${TEST_CONDUCTOR_URL:?Set TEST_CONDUCTOR_URL to the local conductor URL}"
export TEST_SERVICE_BIN_DIR="${TEST_SERVICE_BIN_DIR:-$PWD/target/debug}"
export DATABASE_URL="$TEST_DATABASE_URL"
mkdir -p target/ci-logs
pids=()
cleanup() {
  for pid in "${pids[@]}"; do
    kill "$pid" 2>/dev/null || true
  done
  for pid in "${pids[@]}"; do
    wait "$pid" 2>/dev/null || true
  done
}
trap cleanup EXIT

# NATS has no HTTP health endpoint enabled in its default container image.
python3 - <<'PY'
import os
import socket
import time
from urllib.parse import urlparse

endpoint = urlparse(os.environ['TEST_NATS_URL'])
for attempt in range(60):
    try:
        with socket.create_connection((endpoint.hostname, endpoint.port or 4222), timeout=1):
            break
    except OSError:
        time.sleep(1)
else:
    raise SystemExit('NATS did not become ready')
PY

# Mock NATS responders must finish before the real conductor subscribes to the
# same subjects. Live-service fixtures run after the real services are ready.
cargo test --workspace --locked -- \
  --ignored --test-threads=1 \
  --skip phase3_acceptance \
  --skip authenticated_entry_live_cells_geometry_resume_and_terminal_end \
  --skip authenticated_construction_live_durable_purchase_replay_demolition_and_resume

# These fixtures own and restart their conductor/cell children. Run them before
# the long-lived fixture so no competing NATS responders can service a request.
cargo test --workspace --locked phase3_acceptance -- --ignored --test-threads=1 --nocapture

wait_for_http() {
  local url="$1"
  local pid="$2"
  local expected_status="${3:-200}"
  for ((attempt = 0; attempt < 60; attempt++)); do
    if ! kill -0 "$pid" 2>/dev/null; then
      echo "Service exited before becoming ready: $url" >&2
      return 1
    fi
    if [[ "$(curl --silent --output /dev/null --write-out '%{http_code}' --max-time 2 "$url" || true)" == "$expected_status" ]]; then
      return 0
    fi
    sleep 1
  done
  echo "Service did not become ready: $url" >&2
  return 1
}

target/debug/twz-conductor --bind 127.0.0.1:7000 --nats "$TEST_NATS_URL" \
  >target/ci-logs/conductor.log 2>&1 &
pids+=("$!")
# Routing reports no servers (503) until the first cell has registered.
wait_for_http "$TEST_CONDUCTOR_URL/route/cell/0/0" "${pids[0]}" 503

for cell in A B; do
  if [[ "$cell" == A ]]; then port=3001; else port=3002; fi
  target/debug/twz-cellserver --id "$cell" \
    --bind "127.0.0.1:$port" --advertise "127.0.0.1:$port" \
    --conductor "$TEST_CONDUCTOR_URL" --nats "$TEST_NATS_URL" \
    >"target/ci-logs/cell-$cell.log" 2>&1 &
  pids+=("$!")
  wait_for_http "$TEST_CONDUCTOR_URL/server/$cell" "${pids[-1]}"
  wait_for_http "http://127.0.0.1:$port/health" "${pids[-1]}"
done

# The fixture creates its own gateway with local Auth0 signing keys; no secrets
# or real Auth0 tenant are needed. Both real cells and persistence are exercised.
cargo test --workspace --locked \
  authenticated_ -- \
  --ignored --test-threads=1 --nocapture \
  --skip authenticated_entry_verifies_user_creates_named_town_and_ends_on_disconnect
