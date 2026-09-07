#!/usr/bin/env bash
# Record the live compiler-cache result, then stop the job-owned daemon so the
# runner's orphan-process cleanup cannot turn a successful proof into a flake.
set -euo pipefail

sccache_bin="${SCCACHE_PATH:-sccache}"
if ! command -v "$sccache_bin" >/dev/null 2>&1; then
  echo "::notice title=sccache unavailable::Compiler-cache activity was not measured; sccache is not installed."
  exit 0
fi

stats_status=0
stats="$("$sccache_bin" --show-stats --stats-format=json 2>&1)" || stats_status=$?
if [[ -n "${GITHUB_STEP_SUMMARY:-}" ]]; then
  {
    echo "### sccache"
    echo
    echo '```json'
    printf '%s\n' "$stats"
    echo '```'
  } >> "$GITHUB_STEP_SUMMARY"
fi
printf '%s\n' "$stats"

# Missing measurements must never become measured zeroes. An empty language
# count map is a valid zero only inside a complete, successful stats response.
if [[ "$stats_status" -ne 0 ]]; then
  echo "::warning title=sccache measurement unavailable::Stats command failed with exit ${stats_status}; cache activity is unknown."
elif ! counters="$(jq -er '
  def counter: type == "number" and . >= 0 and floor == .;
  .stats | select(type == "object") |
  select(.compile_requests | counter) |
  select(.cache_hits.counts | type == "object") |
  select(.cache_misses.counts | type == "object") |
  select(.cache_hits.counts | all(.[]; counter)) |
  select(.cache_misses.counts | all(.[]; counter)) |
  [.compile_requests, ([.cache_hits.counts[]] | add // 0),
   ([.cache_misses.counts[]] | add // 0)] | @tsv
' <<< "$stats" 2>/dev/null)"; then
  echo "::warning title=sccache measurement unavailable::Stats response lacks valid compiler-cache counters; cache activity is unknown."
else
  IFS=$'\t' read -r compile_requests cache_hits cache_misses <<< "$counters"
  echo "sccache measured: requests=${compile_requests} hits=${cache_hits} misses=${cache_misses}"
  if [[ "$compile_requests" -eq 0 ]]; then
    echo "::notice title=sccache unused::No compile requests were observed."
  elif [[ "$((cache_hits + cache_misses))" -ge 100 && "$cache_hits" -eq 0 ]]; then
    echo "::warning title=sccache is cold::${cache_misses} cacheable compilations produced zero cache hits."
  fi
fi

if [[ "${HARN_RUNNER_TIER:-}" != "self-hosted" && "${HARN_SHARED_SCCACHE:-}" != "on" ]]; then
  "$sccache_bin" --stop-server >/dev/null 2>&1 || true
fi
