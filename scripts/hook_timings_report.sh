#!/usr/bin/env bash
set -euo pipefail

# Print p50/p95/max hook duration per (repo, hook) from the NDJSON log that
# .githooks/lib.sh's hook_timing_start/hook_timing_finish append to on every
# pre-commit / pre-push invocation (see that file for the wrapper contract,
# and .githooks/pre-commit + .githooks/pre-push for how it's wired in). This
# is the read side of the hook-timing instrument: a pre-push hook that starts
# running a full `make check-generated-registry` rust build should show up
# here as a p95 spike instead of only as anecdotal "git push feels slow" pain.
#
# Usage:
#   scripts/hook_timings_report.sh
#   scripts/hook_timings_report.sh --file /path/to/hook-timings.ndjson
#   scripts/hook_timings_report.sh --json

log_file="${HOME}/.burin/hook-timings.ndjson"
json_output=0

while [ $# -gt 0 ]; do
  case "$1" in
    --file)
      log_file=$2
      shift 2
      ;;
    --json)
      json_output=1
      shift
      ;;
    *)
      echo "hook_timings_report.sh: unknown argument: $1" >&2
      exit 1
      ;;
  esac
done

if [ ! -f "$log_file" ]; then
  echo "Hook timing report (source: $log_file)"
  echo "No hook timing records found."
  exit 0
fi

# Parse the NDJSON with a small, dependency-free field extractor (grep -o on
# a fixed key order — the log lines are written by hook_timing_finish's own
# printf, so the key order and lack of embedded quotes/newlines in values is
# guaranteed) rather than requiring jq, which isn't in the zero-dep contract
# this instrument promises.
awk_field() {
  # $1 = json line, $2 = key
  printf '%s' "$1" | grep -o "\"$2\":\"\{0,1\}[^,\"}]*\"\{0,1\}" | head -1 | sed -E "s/\"$2\":\"?([^\"]*)\"?/\1/"
}

if [ "$json_output" -eq 1 ]; then
  printf '['
  first=1
  while IFS= read -r line; do
    [ -z "$line" ] && continue
    [ "$first" -eq 1 ] || printf ','
    first=0
    printf '%s' "$line"
  done < "$log_file"
  printf ']\n'
  exit 0
fi

echo "Hook timing report (source: $log_file)"

# Group by (repo, hook), collecting durations and failure counts, then
# compute p50/p95/max per group with awk (sort -n per group + index by
# fraction — POSIX awk, no external stats dependency).
awk -F'"' '
  {
    repo = ""; hook = ""; duration = ""; exit_code = ""
    for (i = 1; i <= NF; i += 2) {
      key = $i
      if (key ~ /repo$/)   { repo = $(i + 1) }
      if (key ~ /hook$/)   { hook = $(i + 1) }
    }
    # duration_ms and exit_code are bare numbers, not quoted, so pull them
    # with a plain field split on the numeric-key markers instead.
    line = $0
    if (match(line, /"duration_ms":[0-9]+/)) {
      duration = substr(line, RSTART + 14, RLENGTH - 14)
    }
    if (match(line, /"exit_code":-?[0-9]+/)) {
      exit_code = substr(line, RSTART + 12, RLENGTH - 12)
    }
    if (repo == "" || hook == "" || duration == "") { next }
    key = repo SUBSEP hook
    count[key] += 1
    durations[key] = durations[key] " " duration
    if (exit_code != 0) { failures[key] += 1 }
  }
  END {
    printf "%-28s %-12s %-6s %-8s %-8s %-8s %-8s\n", "repo", "hook", "count", "p50_ms", "p95_ms", "max_ms", "failures"
    printf "%-28s %-12s %-6s %-8s %-8s %-8s %-8s\n", "----------------------------", "------------", "------", "--------", "--------", "--------", "--------"
    for (key in count) {
      split(key, parts, SUBSEP)
      n = split(durations[key], vals, " ")
      # insertion sort — hook counts per machine are small, so O(n^2) is fine.
      for (i = 2; i <= n; i++) {
        v = vals[i]
        j = i - 1
        while (j >= 1 && vals[j] + 0 > v + 0) {
          vals[j + 1] = vals[j]
          j--
        }
        vals[j + 1] = v
      }
      p50_idx = int(0.5 * n); if (p50_idx < 1) p50_idx = 1; if (p50_idx > n) p50_idx = n
      p95_idx = int(0.95 * n); if (p95_idx < 1) p95_idx = 1; if (p95_idx > n) p95_idx = n
      printf "%-28s %-12s %-6d %-8d %-8d %-8d %-8d\n", parts[1], parts[2], count[key], vals[p50_idx], vals[p95_idx], vals[n], failures[key] + 0
    }
  }
' "$log_file"
