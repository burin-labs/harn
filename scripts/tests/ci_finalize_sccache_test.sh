#!/usr/bin/env bash
set -euo pipefail

repo_root=$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)
tmp_root=$(mktemp -d)
trap 'rm -rf "$tmp_root"' EXIT

mkdir -p "$tmp_root/bin"
record="$tmp_root/record"
summary="$tmp_root/summary"
shared_record="$tmp_root/shared-record"
shared_summary="$tmp_root/shared-summary"
tier_record="$tmp_root/tier-record"
tier_summary="$tmp_root/tier-summary"

cat > "$tmp_root/bin/sccache" <<'SH'
#!/usr/bin/env bash
set -euo pipefail
printf '%s\n' "$*" >> "$SCCACHE_TEST_RECORD"
case "$*" in
  '--show-stats --stats-format=json')
    printf '%s\n' "$SCCACHE_TEST_STATS"
    exit "${SCCACHE_TEST_STATUS:-0}"
    ;;
  --stop-server) ;;
  *) exit 90 ;;
esac
SH
chmod +x "$tmp_root/bin/sccache"
export SCCACHE_TEST_STATS='{"stats":{"compile_requests":321,"cache_hits":{"counts":{}},"cache_misses":{"counts":{"Rust":321}}}}'

output=$(PATH="$tmp_root/bin:$PATH" \
  SCCACHE_TEST_RECORD="$record" \
  GITHUB_STEP_SUMMARY="$summary" \
  HARN_SHARED_SCCACHE=off \
  "$repo_root/scripts/ci/finalize_sccache.sh")

[[ "$output" == *"sccache measured: requests=321 hits=0 misses=321"* ]]
[[ "$output" == *"::warning title=sccache is cold::321 cacheable compilations produced zero cache hits."* ]]
grep -Fxq -- '--show-stats --stats-format=json' "$record"
grep -Fxq -- '--stop-server' "$record"
grep -Fq '### sccache' "$summary"
grep -Fq 'compile_requests' "$summary"

PATH="$tmp_root/bin:$PATH" \
  SCCACHE_TEST_RECORD="$shared_record" \
  GITHUB_STEP_SUMMARY="$shared_summary" \
  HARN_SHARED_SCCACHE=on \
  "$repo_root/scripts/ci/finalize_sccache.sh" >/dev/null

grep -Fxq -- '--show-stats --stats-format=json' "$shared_record"
if grep -Fxq -- '--stop-server' "$shared_record"; then
  echo "shared sccache daemon must outlive a runner job" >&2
  exit 1
fi

PATH="$tmp_root/bin:$PATH" \
  SCCACHE_TEST_RECORD="$tier_record" \
  GITHUB_STEP_SUMMARY="$tier_summary" \
  HARN_RUNNER_TIER=self-hosted \
  "$repo_root/scripts/ci/finalize_sccache.sh" >/dev/null

grep -Fxq -- '--show-stats --stats-format=json' "$tier_record"
if grep -Fxq -- '--stop-server' "$tier_record"; then
  echo "self-hosted runner must not stop its host-owned sccache daemon" >&2
  exit 1
fi

# Every observation still finalizes the job-owned daemon, even when stats are
# unusable. None of these failures may invent a measured zero or cold verdict.
for invalid in \
  'not json' \
  '{}' \
  '{"stats":{"compile_requests":0}}' \
  "$(jq '.stats.cache_hits.counts = null' <<< "$SCCACHE_TEST_STATS")" \
  "$(jq '.stats.cache_misses.counts = []' <<< "$SCCACHE_TEST_STATS")" \
  "$(jq '.stats.compile_requests = -1' <<< "$SCCACHE_TEST_STATS")" \
  "$(jq '.stats.cache_hits.counts.Rust = 0.5' <<< "$SCCACHE_TEST_STATS")" \
  "$(jq '.stats.cache_misses.counts.Rust = "321"' <<< "$SCCACHE_TEST_STATS")"; do
  : > "$record"
  output=$(PATH="$tmp_root/bin:$PATH" SCCACHE_TEST_RECORD="$record" \
    SCCACHE_TEST_STATS="$invalid" HARN_SHARED_SCCACHE=off HARN_RUNNER_TIER=github-hosted \
    "$repo_root/scripts/ci/finalize_sccache.sh")
  [[ "$output" == *"::warning title=sccache measurement unavailable::"* ]]
  [[ "$output" != *"sccache measured:"* && "$output" != *"sccache is cold"* ]]
  grep -Fxq -- '--stop-server' "$record"
done

: > "$record"
output=$(PATH="$tmp_root/bin:$PATH" SCCACHE_TEST_RECORD="$record" \
  SCCACHE_TEST_STATUS=7 HARN_SHARED_SCCACHE=on \
  "$repo_root/scripts/ci/finalize_sccache.sh")
[[ "$output" == *"Stats command failed with exit 7; cache activity is unknown."* ]]
[[ "$output" != *"sccache measured:"* ]]
if grep -Fxq -- '--stop-server' "$record"; then
  echo "a failed stats read must not stop a shared daemon" >&2
  exit 1
fi

hits='{"stats":{"compile_requests":321,"cache_hits":{"counts":{"Rust":200,"C/C++":100}},"cache_misses":{"counts":{"Rust":21}}}}'
output=$(PATH="$tmp_root/bin:$PATH" SCCACHE_TEST_RECORD="$record" SCCACHE_TEST_STATS="$hits" \
  "$repo_root/scripts/ci/finalize_sccache.sh")
[[ "$output" == *"sccache measured: requests=321 hits=300 misses=21"* ]]
[[ "$output" != *"::warning"* ]]

zero='{"stats":{"compile_requests":0,"cache_hits":{"counts":{}},"cache_misses":{"counts":{}}}}'
output=$(PATH="$tmp_root/bin:$PATH" SCCACHE_TEST_RECORD="$record" SCCACHE_TEST_STATS="$zero" \
  "$repo_root/scripts/ci/finalize_sccache.sh")
[[ "$output" == *"sccache measured: requests=0 hits=0 misses=0"* ]]
[[ "$output" == *"No compile requests were observed."* ]]
[[ "$output" != *"::warning"* ]]

# Rust linking and compiler version probes are not cacheable work. A busy
# daemon that only saw these must not be misreported as a cold compiler cache.
uncacheable="$(jq '.stats.compile_requests = 321' <<< "$zero")"
output=$(PATH="$tmp_root/bin:$PATH" SCCACHE_TEST_RECORD="$record" SCCACHE_TEST_STATS="$uncacheable" \
  "$repo_root/scripts/ci/finalize_sccache.sh")
[[ "$output" == *"sccache measured: requests=321 hits=0 misses=0"* ]]
[[ "$output" != *"::warning"* && "$output" != *"No compile requests"* ]]

output=$(SCCACHE_PATH="$tmp_root/not-installed" "$repo_root/scripts/ci/finalize_sccache.sh")
[[ "$output" == *"Compiler-cache activity was not measured; sccache is not installed."* ]]
[[ "$output" != *"sccache measured:"* ]]

echo "ci_finalize_sccache_test: ok"
