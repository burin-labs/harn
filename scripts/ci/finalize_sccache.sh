#!/usr/bin/env bash
# Emit the live server's statistics before stopping a job-owned daemon. A
# host-owned shared daemon must outlive every individual runner job.
set -euo pipefail

sccache_bin="${SCCACHE_PATH:-sccache}"
if ! command -v "$sccache_bin" >/dev/null 2>&1; then
  exit 0
fi

stats="$($sccache_bin --show-stats 2>&1 || true)"
if [[ -n "${GITHUB_STEP_SUMMARY:-}" ]]; then
  {
    echo "### sccache"
    echo
    echo '```text'
    printf '%s\n' "$stats"
    echo '```'
  } >> "$GITHUB_STEP_SUMMARY"
fi
printf '%s\n' "$stats"

compile_requests=$(awk '/^Compile requests[[:space:]]+[0-9]+$/ {print $3; exit}' <<< "$stats")
cache_hits=$(awk '/^Cache hits[[:space:]]+[0-9]+$/ {print $3; exit}' <<< "$stats")
if [[ "${compile_requests:-0}" -ge 100 && "${cache_hits:-0}" -eq 0 ]]; then
  echo "::warning title=sccache is cold::${compile_requests} compile requests produced zero cache hits."
fi

if [[ "${HARN_SHARED_SCCACHE:-}" != "on" ]]; then
  "$sccache_bin" --stop-server >/dev/null 2>&1 || true
fi
