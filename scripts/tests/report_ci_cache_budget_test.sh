#!/usr/bin/env bash
set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
tmp="$(mktemp -d)"
trap 'rm -rf "$tmp"' EXIT

mkdir -p "$tmp/bin"
cat >"$tmp/bin/gh" <<'MOCK'
#!/usr/bin/env bash
set -euo pipefail
args="$*"
if [[ "$args" == *'/actions/cache/storage-limit'* ]]; then
  printf '{"max_cache_size_gb":10}\n'
elif [[ "$args" == *'/actions/cache/usage'* ]]; then
  printf '{"full_name":"burin-labs/harn","active_caches_size_in_bytes":%s,"active_caches_count":2}\n' "${MOCK_USAGE_BYTES:-3000}"
elif [[ "$args" == *'/actions/caches?per_page=100'* ]]; then
  printf '[{"total_count":2,"actions_caches":[{"id":1,"ref":"refs/heads/main","key":"v0-rust-release-linux","size_in_bytes":2000},{"id":2,"ref":"refs/pull/9/merge","key":"sccache/a/b/c","size_in_bytes":1000}]}]\n'
else
  echo "unexpected gh arguments: $args" >&2
  exit 64
fi
MOCK
chmod +x "$tmp/bin/gh"

PATH="$tmp/bin:$PATH" GITHUB_REPOSITORY=burin-labs/harn GITHUB_STEP_SUMMARY="$tmp/summary.md" \
  "$repo_root/scripts/report_ci_cache_budget.sh" >"$tmp/report.json"
jq -e '
  .schema_version == "harn.ci_cache_budget.v1" and
  .configured_limit_bytes == 10737418240 and
  .api_limit_bytes == 10737418240 and
  .active_bytes == 3000 and
  .listed_bytes == 3000 and
  .within_budget == true and
  (.by_class | map(.class) | sort) == ["release", "sccache"]
' "$tmp/report.json" >/dev/null
grep -q 'GitHub Actions cache budget' "$tmp/summary.md"

if PATH="$tmp/bin:$PATH" MOCK_USAGE_BYTES=10737418241 GITHUB_REPOSITORY=burin-labs/harn \
  "$repo_root/scripts/report_ci_cache_budget.sh" >"$tmp/over.json" 2>"$tmp/over.err"; then
  echo "expected over-budget inventory to fail" >&2
  exit 1
fi
grep -q 'cache exceeds the 10737418240-byte policy budget' "$tmp/over.err"

echo "CI cache budget report tests passed"
