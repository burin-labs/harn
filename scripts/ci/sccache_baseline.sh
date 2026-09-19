#!/usr/bin/env bash
# Record the compiler cache counters this job starts from.
#
# On a shared runner the sccache server outlives every job and its counters are
# cumulative for the whole host. A bare `--show-stats` after a build therefore
# reports work other jobs did, and a job that got no hits of its own still
# reads as a warm cache. Capturing a baseline here lets the summary report this
# job's own delta instead of the host's running total.
#
# A missing or unparseable baseline is not an error. The finalize step falls
# back to cumulative counters and says so, because a measurement that silently
# becomes a zero is worse than one that names itself absent.
set -euo pipefail

baseline_path="${SCCACHE_BASELINE_PATH:-${RUNNER_TEMP:?RUNNER_TEMP must name a writable directory}/sccache-baseline.json}"
sccache_bin="${SCCACHE_PATH:-sccache}"

if ! command -v "$sccache_bin" >/dev/null 2>&1; then
  echo "::notice title=sccache unavailable::No baseline recorded; sccache is not installed."
  exit 0
fi

if ! stats="$("$sccache_bin" --show-stats --stats-format=json 2>&1)"; then
  echo "::warning title=sccache baseline unavailable::Stats command failed; the summary will report cumulative host counters."
  exit 0
fi

if ! jq -e '.stats.compile_requests | type == "number"' <<< "$stats" >/dev/null 2>&1; then
  echo "::warning title=sccache baseline unavailable::Stats response lacks counters; the summary will report cumulative host counters."
  exit 0
fi

printf '%s\n' "$stats" > "$baseline_path"
echo "sccache baseline recorded at $baseline_path"
