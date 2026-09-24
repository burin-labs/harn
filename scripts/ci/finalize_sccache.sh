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

  # Subtract the counters this job started from, when they were recorded. The
  # server on a shared runner is host-wide and long-lived, so its totals
  # describe every job that has ever compiled there. Reporting those as this
  # job's result would let a cold job inherit a warm neighbour's hits.
  scope=cumulative
  baseline_path="${SCCACHE_BASELINE_PATH:-${RUNNER_TEMP:-}/sccache-baseline.json}"
  if [[ -n "${RUNNER_TEMP:-}" || -n "${SCCACHE_BASELINE_PATH:-}" ]] \
    && [[ -r "$baseline_path" ]] \
    && base_counters="$(jq -er '
      def counter: type == "number" and . >= 0 and floor == .;
      .stats | select(type == "object") |
      select(.compile_requests | counter) |
      select(.cache_hits.counts | type == "object") |
      select(.cache_misses.counts | type == "object") |
      [.compile_requests, ([.cache_hits.counts[]] | add // 0),
       ([.cache_misses.counts[]] | add // 0)] | @tsv
    ' "$baseline_path" 2>/dev/null)"; then
    IFS=$'\t' read -r base_requests base_hits base_misses <<< "$base_counters"
    # A server restarted mid-job resets its counters, which would make the
    # delta negative. Name that rather than reporting a nonsense number.
    if ((compile_requests >= base_requests && cache_hits >= base_hits && cache_misses >= base_misses)); then
      compile_requests=$((compile_requests - base_requests))
      cache_hits=$((cache_hits - base_hits))
      cache_misses=$((cache_misses - base_misses))
      scope=job
    else
      echo "::warning title=sccache counters reset::The cache server restarted during this job; reporting cumulative host counters."
    fi
  fi

  echo "sccache measured (${scope}): requests=${compile_requests} hits=${cache_hits} misses=${cache_misses}"
  if [[ "$compile_requests" -eq 0 ]]; then
    echo "::notice title=sccache unused::No compile requests were observed."
  elif [[ "$((cache_hits + cache_misses))" -ge 100 && "$cache_hits" -eq 0 ]]; then
    echo "::warning title=sccache is cold::${cache_misses} cacheable compilations produced zero cache hits."
  fi
fi

if [[ "${HARN_RUNNER_TIER:-}" != "self-hosted" && "${HARN_SHARED_SCCACHE:-}" != "on" ]]; then
  "$sccache_bin" --stop-server >/dev/null 2>&1 || true
fi
