#!/usr/bin/env bash
# Produce one non-empty rolling CI report. The Harn boundary owns bounded API retries.
set -euo pipefail

report_path="${1:?usage: $0 REPORT_PATH}"
harn_bin="${HARN_BIN:?HARN_BIN must name the verified CLI artifact}"
tmp_path="${report_path}.tmp"
rm -f "$tmp_path"
trap 'rm -f "$tmp_path"' EXIT

# The report shells out to GitHub's API. Harn's default network ceiling is
# correct for normal scripts, so this explicitly network-enabled CI audit uses
# the narrow, visible CLI escape hatch.
"$harn_bin" run --no-sandbox scripts/ci_walltime_report.harn -- \
  --policy .github/ci-latency.json --limit 50 --json > "$tmp_path"
[ -s "$tmp_path" ]
mv "$tmp_path" "$report_path"

if [ -n "${GITHUB_STEP_SUMMARY:-}" ]; then
  {
    echo "### Rolling merge-group wall time"
    echo
    echo "The structured report is attached as the \`ci-walltime-report\` artifact."
  } >> "$GITHUB_STEP_SUMMARY"
fi
