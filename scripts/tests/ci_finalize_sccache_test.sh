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
  --show-stats)
    printf 'Compile requests                    321\nCache hits                            0\nCache misses                        321\n'
    ;;
  --stop-server) ;;
  *) exit 90 ;;
esac
SH
chmod +x "$tmp_root/bin/sccache"

output=$(PATH="$tmp_root/bin:$PATH" \
  SCCACHE_TEST_RECORD="$record" \
  GITHUB_STEP_SUMMARY="$summary" \
  HARN_SHARED_SCCACHE=off \
  "$repo_root/scripts/ci/finalize_sccache.sh")

[[ "$output" == *"Compile requests"* ]]
[[ "$output" == *"::warning title=sccache is cold::321 compile requests produced zero cache hits."* ]]
grep -Fxq -- '--show-stats' "$record"
grep -Fxq -- '--stop-server' "$record"
grep -Fq '### sccache' "$summary"
grep -Fq 'Compile requests' "$summary"

PATH="$tmp_root/bin:$PATH" \
  SCCACHE_TEST_RECORD="$shared_record" \
  GITHUB_STEP_SUMMARY="$shared_summary" \
  HARN_SHARED_SCCACHE=on \
  "$repo_root/scripts/ci/finalize_sccache.sh" >/dev/null

grep -Fxq -- '--show-stats' "$shared_record"
if grep -Fxq -- '--stop-server' "$shared_record"; then
  echo "shared sccache daemon must outlive a runner job" >&2
  exit 1
fi

PATH="$tmp_root/bin:$PATH" \
  SCCACHE_TEST_RECORD="$tier_record" \
  GITHUB_STEP_SUMMARY="$tier_summary" \
  HARN_RUNNER_TIER=self-hosted \
  "$repo_root/scripts/ci/finalize_sccache.sh" >/dev/null

grep -Fxq -- '--show-stats' "$tier_record"
if grep -Fxq -- '--stop-server' "$tier_record"; then
  echo "self-hosted runner must not stop its host-owned sccache daemon" >&2
  exit 1
fi

echo "ci_finalize_sccache_test: ok"
