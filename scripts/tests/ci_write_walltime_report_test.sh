#!/usr/bin/env bash
set -euo pipefail

repo_root=$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)
tmp_root=$(mktemp -d)
trap 'rm -rf "$tmp_root"' EXIT

cat > "$tmp_root/harn" <<'SH'
#!/usr/bin/env bash
set -euo pipefail
[[ "$*" == "run --no-sandbox scripts/ci_walltime_report.harn -- --policy .github/ci-latency.json --limit 50 --json" ]] || exit 2
if [ "${FAKE_HARN_FAIL:-0}" = "1" ]; then
  echo 'report failure' >&2
  exit 1
fi
printf '{"schema_version":1,"wall":{"p90_ms":600000}}\n'
SH
chmod +x "$tmp_root/harn"

HARN_BIN="$tmp_root/harn" \
  GITHUB_STEP_SUMMARY="$tmp_root/summary" \
  "$repo_root/scripts/ci/write_ci_walltime_report.sh" "$tmp_root/report.json"

grep -Fq '"p90_ms":600000' "$tmp_root/report.json"
grep -Fq 'ci-walltime-report' "$tmp_root/summary"
if HARN_BIN="$tmp_root/harn" \
  FAKE_HARN_FAIL=1 \
  GITHUB_STEP_SUMMARY="$tmp_root/fail-summary" \
  "$repo_root/scripts/ci/write_ci_walltime_report.sh" "$tmp_root/fail.json" \
  >/dev/null 2>&1; then
  echo "wall-time writer accepted an empty failed report" >&2
  exit 1
fi
[[ ! -e "$tmp_root/fail.json" ]]

echo "ci_write_walltime_report_test: ok"
