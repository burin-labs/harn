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

echo "ci_finalize_sccache_test: ok"
