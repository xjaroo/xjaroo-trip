#!/usr/bin/env bash
set -euo pipefail
cd "$(dirname "$0")"

if [[ -f .env ]]; then
  set -a
  # shellcheck disable=SC1091
  source .env
  set +a
fi

export TRIP_DB="${TRIP_DB:-$PWD/data/trips.sqlite}"
export TRIP_ADDR="${TRIP_ADDR:-127.0.0.1:3007}"

port="${TRIP_ADDR##*:}"
if [[ ! "$port" =~ ^[0-9]+$ ]]; then
  echo "Invalid TRIP_ADDR port: $TRIP_ADDR" >&2
  exit 1
fi

state_dir="$PWD/data"
pid_file="$state_dir/xjaroo-trip.pid"
log_file="$state_dir/xjaroo-trip.log"
binary="$PWD/target/release/xjaroo-trip"
mkdir -p "$state_dir"

if ! command -v lsof >/dev/null 2>&1; then
  echo "lsof is required to check whether port $port is in use." >&2
  exit 1
fi

listeners() {
  lsof -nP -tiTCP:"$port" -sTCP:LISTEN 2>/dev/null || true
}

stop_listeners() {
  local pids remaining attempts pid
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
      echo "Could not free port $port." >&2
      return 1
    fi
  fi
  rm -f "$pid_file"
}

status() {
  local pids
  pids="$(listeners)"
  if [[ -n "$pids" ]]; then
    echo "Trip planner is running at http://$TRIP_ADDR (PID(s): $(echo "$pids" | tr '\n' ' '))"
    echo "Log: $log_file"
    return 0
  fi
  echo "Trip planner is not running on port $port."
  return 1
}

case "${1:-start}" in
  start|restart)
    if ! command -v cargo >/dev/null 2>&1; then
      echo "cargo is required to build the trip planner." >&2
      exit 1
    fi
    cargo build --release
    stop_listeners
    echo "Starting trip planner in the background..."
    nohup "$binary" >>"$log_file" 2>&1 </dev/null &
    app_pid=$!
    echo "$app_pid" >"$pid_file"

    attempts=0
    while [[ -z "$(listeners)" && "$attempts" -lt 40 ]]; do
      if ! kill -0 "$app_pid" 2>/dev/null; then
        echo "Trip planner exited during startup. Recent log output:" >&2
        tail -n 30 "$log_file" >&2 || true
        rm -f "$pid_file"
        exit 1
      fi
      sleep 0.25
      attempts=$((attempts + 1))
    done

    if [[ -z "$(listeners)" ]]; then
      echo "Trip planner did not start listening on port $port. See $log_file." >&2
      rm -f "$pid_file"
      exit 1
    fi
    echo "Trip planner running at http://$TRIP_ADDR (PID $app_pid)"
    echo "SQLite database: $TRIP_DB"
    echo "Log: $log_file"
    ;;
  stop)
    stop_listeners
    echo "Trip planner stopped."
    ;;
  status)
    status
    ;;
  *)
    echo "Usage: $0 [start|stop|restart|status]" >&2
    exit 2
    ;;
esac
