#!/usr/bin/env bash
set -euo pipefail
cd "$(dirname "$0")"
export TRIP_DB="${TRIP_DB:-$PWD/data/trips.sqlite}"
export TRIP_ADDR="${TRIP_ADDR:-127.0.0.1:3000}"

port="${TRIP_ADDR##*:}"
if [[ ! "$port" =~ ^[0-9]+$ ]]; then
  echo "Invalid TRIP_ADDR port: $TRIP_ADDR" >&2
  exit 1
fi

if ! command -v lsof >/dev/null 2>&1; then
  echo "lsof is required to check whether port $port is in use." >&2
  exit 1
fi

listeners() {
  lsof -nP -tiTCP:"$port" -sTCP:LISTEN 2>/dev/null || true
}

pids="$(listeners)"
if [[ -n "$pids" ]]; then
  echo "Stopping process(es) using port $port: $(echo "$pids" | tr '\n' ' ')"
  while IFS= read -r pid; do
    [[ -n "$pid" ]] && kill -TERM "$pid" 2>/dev/null || true
  done <<< "$pids"

  attempts=0
  while [[ -n "$(listeners)" && "$attempts" -lt 20 ]]; do
    sleep 0.25
    attempts=$((attempts + 1))
  done

  remaining="$(listeners)"
  if [[ -n "$remaining" ]]; then
    echo "Process(es) did not stop gracefully; forcing them to stop: $(echo "$remaining" | tr '\n' ' ')"
    while IFS= read -r pid; do
      [[ -n "$pid" ]] && kill -KILL "$pid" 2>/dev/null || true
    done <<< "$remaining"
    sleep 0.25
  fi

  if [[ -n "$(listeners)" ]]; then
    echo "Could not free port $port; refusing to start another instance." >&2
    exit 1
  fi
fi

echo "Local trip planner: http://$TRIP_ADDR"
echo "SQLite database:   $TRIP_DB"
cargo run --release
