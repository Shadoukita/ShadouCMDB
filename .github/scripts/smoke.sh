#!/usr/bin/env bash
# Start a ShadouCMDB server, check that it answers, then stop it.
#
#   smoke.sh PORT COMMAND...        e.g. smoke.sh 3000 ./shadoucmdb serve
#
# The server must listen on 127.0.0.1:PORT and its database must already be
# migrated. Checks: /healthz is ok, /readyz reports every migration applied,
# and / serves the embedded web UI. SIGTERM must stop it within 30 s.
set -euo pipefail

port=$1
shift
base="http://127.0.0.1:$port"
log=$(mktemp)

"$@" >"$log" 2>&1 &
pid=$!
trap 'kill "$pid" 2>/dev/null || true; echo "--- server log"; tail -20 "$log"' EXIT

for _ in $(seq 1 60); do
  curl -sf "$base/healthz" >/dev/null && break
  kill -0 "$pid" 2>/dev/null || { echo "server exited early" >&2; exit 1; }
  sleep 1
done

echo "GET /healthz -> $(curl -sf "$base/healthz")"
readyz=$(curl -sf "$base/readyz")
echo "GET /readyz  -> $readyz"
grep -q '"upToDate":true' <<<"$readyz"
index=$(curl -sf "$base/")
grep -qi '<!doctype html>' <<<"$index"
echo "GET /        -> embedded web UI (index.html, ${#index} bytes)"

kill -TERM "$pid"
for _ in $(seq 1 30); do kill -0 "$pid" 2>/dev/null || break; sleep 1; done
if kill -0 "$pid" 2>/dev/null; then echo "server did not stop on SIGTERM" >&2; exit 1; fi
trap - EXIT
tail -3 "$log"
