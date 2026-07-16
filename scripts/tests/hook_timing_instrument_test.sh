#!/usr/bin/env bash
set -euo pipefail

repo_root=$(cd "$(dirname "$0")/../.." && pwd)
tmp=$(mktemp -d)
trap 'rm -rf "$tmp"' EXIT

emit_timing() {
  profile=$1
  if [ "$profile" = "full" ]; then
    HARN_HOOKS_FULL_LOCAL=1 HOOK_TIMING_LOG_DIR="$tmp" sh -c '
      . "$1/.githooks/lib.sh"
      hook_timing_start pre-push
      hook_timing_finish 0
    ' shell "$repo_root"
  else
    HOOK_TIMING_LOG_DIR="$tmp" sh -c '
      . "$1/.githooks/lib.sh"
      hook_timing_start pre-commit
      hook_timing_finish 0
    ' shell "$repo_root"
  fi
}

emit_timing fast
emit_timing full

[ "$(wc -l < "$tmp/hook-timings.ndjson" | tr -d ' ')" = "2" ]
grep -Fq '"repository":"burin-labs/harn"' "$tmp/hook-timings.ndjson"
grep -Fq '"hook":"pre-commit","profile":"fast"' "$tmp/hook-timings.ndjson"
grep -Fq '"hook":"pre-push","profile":"full"' "$tmp/hook-timings.ndjson"

echo "hook_timing_instrument_test: ok"
