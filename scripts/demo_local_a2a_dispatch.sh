#!/usr/bin/env bash
set -euo pipefail

ROOT="$(git rev-parse --show-toplevel)"
HARN_BIN="$("$ROOT/scripts/harn_bin.sh" --print)"

TMP_DIR="$(mktemp -d "${TMPDIR:-/tmp}/harn-local-a2a-demo.XXXXXX")"
LOG="$TMP_DIR/receiver.log"
PID=""

cleanup() {
  if [[ -n "$PID" ]]; then
    kill -TERM "$PID" >/dev/null 2>&1 || true
    wait "$PID" >/dev/null 2>&1 || true
  fi
  rm -rf "$TMP_DIR"
}
trap cleanup EXIT

"$HARN_BIN" serve --port 0 "$ROOT/examples/triggers/local-a2a-dispatch/remote-handler.harn" \
  >"$LOG" 2>&1 &
PID="$!"

for _ in {1..400}; do
  if ! kill -0 "$PID" >/dev/null 2>&1; then
    echo "A2A receiver exited before it became ready" >&2
    cat "$LOG" >&2
    exit 1
  fi
  if grep -E 'Harn A2A server listening on ' "$LOG" >/dev/null 2>&1; then
    break
  fi
  sleep 0.05
done

URL="$(sed -nE 's/.*Harn A2A server listening on ([^[:space:]]+).*/\1/p' "$LOG" | tail -n 1)"
if [[ -z "$URL" ]]; then
  echo "A2A receiver did not start" >&2
  cat "$LOG" >&2
  exit 1
fi
PORT="${URL##*:}"

HARN_DEMO_REMOTE_PORT="$PORT" \
  "$HARN_BIN" run "$ROOT/examples/triggers/local-a2a-dispatch/demo.harn"
