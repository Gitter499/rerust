#!/usr/bin/env bash
# Fault-tolerant enrichment watchdog: runs exemplar scan + batched backfill in parallel.
set -euo pipefail
cd "$(dirname "$0")/.."

BIN="${RERUST_BIN:-./target/release/rerust}"
DB="${RERUST_DB:-rerust.db}"
EXEMPLARS="${RERUST_EXEMPLARS:-data/exemplars.txt}"
LOG="${RERUST_BACKFILL_LOG:-/tmp/rerust-backfill.log}"
PIDFILE="${RERUST_WATCHDOG_PIDFILE:-/tmp/rerust-watchdog.pid}"
BATCH="${RERUST_BATCH_SIZE:-10}"
MACRO_TIMEOUT="${RERUST_MACRO_TIMEOUT:-900}"
TIMEOUT="${RERUST_TIMEOUT:-300}"
MAX_ROUNDS="${RERUST_MAX_ROUNDS:-500}"

# Ensure GitHub API token for rate limits (user authorized gh auth token).
if [[ -z "${GITHUB_TOKEN:-}" ]]; then
  if command -v gh &>/dev/null; then
    GITHUB_TOKEN="$(gh auth token 2>/dev/null || true)"
    export GITHUB_TOKEN
  fi
fi

# Daemonize so the watchdog survives caller shell exit (unless --foreground).
if [[ "${1:-}" != "--foreground" && -z "${RERUST_WATCHDOG_FOREGROUND:-}" ]]; then
  if [[ -z "${GITHUB_TOKEN:-}" ]]; then
    echo "WARNING: GITHUB_TOKEN not set; rate limits may be hit" >&2
  else
    echo "GITHUB_TOKEN set (len=${#GITHUB_TOKEN})"
  fi
  rm -f "${DB}.backfill.lock" "$PIDFILE"
  script_path="$(cd "$(dirname "$0")" && pwd)/$(basename "$0")"
  repo_root="$(pwd)"
  python3 - "$script_path" "$repo_root" "$LOG" "$PIDFILE" "${GITHUB_TOKEN:-}" <<'PY' &
import os, sys, time
script, repo, log, pidfile, token = sys.argv[1:6]
if os.fork() > 0:
    sys.exit(0)
os.setsid()
if os.fork() > 0:
    sys.exit(0)
os.chdir(repo)
os.environ["RERUST_WATCHDOG_FOREGROUND"] = "1"
if token:
    os.environ["GITHUB_TOKEN"] = token
log_fd = os.open(log, os.O_WRONLY | os.O_CREAT | os.O_APPEND, 0o644)
os.dup2(log_fd, 1)
os.dup2(log_fd, 2)
os.close(log_fd)
os.execv("/bin/bash", ["bash", script, "--foreground"])
PY
  launcher_pid=$!
  for _ in $(seq 1 20); do
    if [[ -f "$PIDFILE" ]]; then
      wp=$(cat "$PIDFILE")
      if kill -0 "$wp" 2>/dev/null; then
        echo "watchdog daemonized pid=$wp"
        exit 0
      fi
    fi
    sleep 0.1
  done
  echo "watchdog launcher pid=$launcher_pid (waiting for pidfile)"
  exit 0
fi

if [[ -z "${GITHUB_TOKEN:-}" ]]; then
  echo "WARNING: GITHUB_TOKEN not set; rate limits may be hit" >&2
else
  echo "GITHUB_TOKEN set (len=${#GITHUB_TOKEN})"
fi
echo $$ >"$PIDFILE"

if [[ ! -x "$BIN" ]]; then
  echo "Building release binary..."
  env -u CARGO_TARGET_DIR cargo build --release -p rerust
  BIN="./target/release/rerust"
fi

echo "$(date -u +%Y-%m-%dT%H:%M:%SZ) watchdog start batch=$BATCH" | tee -a "$LOG"

count_exemplars_pending() {
  local pending=0 slug hist_status
  while IFS= read -r slug || [[ -n "$slug" ]]; do
    slug="${slug%%#*}"
    slug="${slug// /}"
    [[ -z "$slug" ]] && continue
    hist_status=$(sqlite3 "$DB" "SELECT history_status FROM projects WHERE repo_url='https://github.com/$slug' LIMIT 1;")
    if [[ "$hist_status" != "ok" ]]; then
      pending=$((pending + 1))
    fi
  done < "$EXEMPLARS"
  echo "$pending"
}

# Start scan-exemplars in background; never block backfill on slow macro scans.
SCAN_PID=""
start_scan_exemplars_bg() {
  local pending
  pending=$(count_exemplars_pending)
  if [[ "$pending" -eq 0 ]]; then
    echo "$(date -u +%H:%M:%S) scan-exemplars skipped (all exemplars history_ok)" >>"$LOG"
    return 0
  fi
  if [[ -n "$SCAN_PID" ]] && kill -0 "$SCAN_PID" 2>/dev/null; then
    echo "$(date -u +%H:%M:%S) scan-exemplars still running pid=$SCAN_PID pending=$pending" >>"$LOG"
    return 0
  fi
  echo "$(date -u +%H:%M:%S) scan-exemplars pending=$pending (background pid pending)" >>"$LOG"
  "$BIN" scan-exemplars --db "$DB" --exemplars-file "$EXEMPLARS" \
    --macro-timeout-secs "$MACRO_TIMEOUT" >>"$LOG" 2>&1 &
  SCAN_PID=$!
  echo "$(date -u +%H:%M:%S) scan-exemplars started pid=$SCAN_PID" >>"$LOG"
}

# Kick off exemplar scan immediately in background; backfill starts without waiting.
start_scan_exemplars_bg

for round in $(seq 1 "$MAX_ROUNDS"); do
  pending=$(sqlite3 "$DB" "SELECT COUNT(*) FROM projects WHERE history_status IS NULL OR (history_status='failed' AND COALESCE(history_attempts,0) < 3) OR (history_status='ok' AND unsafe_percentage IS NULL AND (original_language IS NULL OR rust_percentage >= 50.0));")
  echo "$(date -u +%H:%M:%S) round=$round pending≈$pending scan_pid=${SCAN_PID:-none}" >>"$LOG"
  if [[ "${pending:-0}" -eq 0 ]]; then
    if [[ -n "$SCAN_PID" ]] && kill -0 "$SCAN_PID" 2>/dev/null; then
      echo "$(date -u +%H:%M:%S) waiting for scan-exemplars pid=$SCAN_PID before finish" >>"$LOG"
      wait "$SCAN_PID" 2>/dev/null || true
      SCAN_PID=""
    fi
    echo "$(date -u +%H:%M:%S) watchdog done" >>"$LOG"
    "$BIN" reclassify --db "$DB" >>"$LOG" 2>&1
    "$BIN" build-site --db "$DB" --out docs >>"$LOG" 2>&1
    exit 0
  fi
  rm -f "${DB}.backfill.lock"
  "$BIN" backfill-history --db "$DB" \
    --max-stars 0 \
    --batch-size "$BATCH" \
    --timeout-secs "$TIMEOUT" \
    --macro-timeout-secs "$MACRO_TIMEOUT" \
    --exemplars-file "$EXEMPLARS" \
    --retry-failed \
    >>"$LOG" 2>&1 || echo "$(date -u +%H:%M:%S) backfill exited code=$?" >>"$LOG"

  # Re-launch exemplar scan if prior run finished and exemplars still pending.
  start_scan_exemplars_bg

  sleep 2
done

echo "$(date -u +%H:%M:%S) watchdog max rounds reached" >>"$LOG"
exit 1
