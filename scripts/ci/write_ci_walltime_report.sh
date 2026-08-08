#!/usr/bin/env bash
# Produce a non-empty rolling CI report despite transient GitHub API failures.
set -euo pipefail

report_path="${1:?usage: $0 REPORT_PATH}"
harn_bin="${HARN_BIN:?HARN_BIN must name the verified CLI artifact}"
attempts="${HARN_WALLTIME_REPORT_ATTEMPTS:-3}"
interval_seconds="${HARN_WALLTIME_REPORT_INTERVAL_SECONDS:-2}"

case "$attempts" in
  ''|*[!0-9]*|0) echo "HARN_WALLTIME_REPORT_ATTEMPTS must be a positive integer" >&2; exit 2 ;;
esac
case "$interval_seconds" in
  ''|*[!0-9]*) echo "HARN_WALLTIME_REPORT_INTERVAL_SECONDS must be a non-negative integer" >&2; exit 2 ;;
esac

tmp_path="${report_path}.tmp"
rm -f "$tmp_path"
trap 'rm -f "$tmp_path"' EXIT

run_with_retry() {
  local output_path="$1"
  shift
  local attempt
  for ((attempt = 1; attempt <= attempts; attempt++)); do
    # The report shells out to GitHub's API. Harn's default network ceiling is
    # correct for normal scripts, so this explicitly network-enabled CI audit
    # uses the narrow, visible CLI escape hatch.
    if "$harn_bin" run --no-sandbox scripts/ci_walltime_report.harn -- \
      --limit 50 "$@" > "$output_path" \
      && [ -s "$output_path" ]; then
      return 0
    fi
    if [ "$attempt" -eq "$attempts" ]; then
      return 1
    fi
    sleep "$interval_seconds"
  done
}

run_with_retry "$tmp_path" --json
mv "$tmp_path" "$report_path"

if [ -n "${GITHUB_STEP_SUMMARY:-}" ]; then
  {
    echo "### Rolling merge-group wall time"
    echo
    echo "The structured report is attached as the \`ci-walltime-report\` artifact."
  } >> "$GITHUB_STEP_SUMMARY"
fi
