#!/usr/bin/env bash
set -euo pipefail

repo_root=$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)
tmp_root=$(mktemp -d)
trap 'rm -rf "$tmp_root"' EXIT
mkdir -p "$tmp_root/bin"

cat > "$tmp_root/bin/gh" <<'SH'
#!/usr/bin/env bash
set -euo pipefail
count_file="${FAKE_GH_COUNT:?}"
count=0
if [ -f "$count_file" ]; then
  count=$(<"$count_file")
fi
count=$((count + 1))
printf '%s\n' "$count" > "$count_file"
if [ "${FAKE_GH_FAIL:-0}" = "1" ]; then
  exit 1
fi
printf '%s\n' 'harn-cli.tar.zst'
if [ "$count" -ge 3 ]; then
  printf '%s\n' 'harn-security.tar.zst'
fi
SH
chmod +x "$tmp_root/bin/gh"

output=$(PATH="$tmp_root/bin:$PATH" \
  FAKE_GH_COUNT="$tmp_root/count" \
  GITHUB_REPOSITORY=burin-labs/harn \
  GITHUB_RUN_ID=123 \
  HARN_ARTIFACT_WAIT_MAX_ATTEMPTS=4 \
  HARN_ARTIFACT_WAIT_INTERVAL_SECONDS=0 \
  "$repo_root/scripts/ci/wait_for_run_artifacts.sh" \
  harn-cli.tar.zst harn-security.tar.zst)

[[ "$output" == *"run artifacts ready: harn-cli.tar.zst harn-security.tar.zst"* ]]
[[ "$(<"$tmp_root/count")" = "3" ]]

if PATH="$tmp_root/bin:$PATH" \
  FAKE_GH_COUNT="$tmp_root/fail-count" \
  FAKE_GH_FAIL=1 \
  GITHUB_REPOSITORY=burin-labs/harn \
  GITHUB_RUN_ID=456 \
  HARN_ARTIFACT_WAIT_MAX_ATTEMPTS=2 \
  HARN_ARTIFACT_WAIT_INTERVAL_SECONDS=0 \
  "$repo_root/scripts/ci/wait_for_run_artifacts.sh" harn-cli.tar.zst \
  >"$tmp_root/fail.out" 2>"$tmp_root/fail.err"; then
  echo "artifact wait accepted a terminal API failure" >&2
  exit 1
fi
grep -Fq 'timed out waiting for run artifacts after 2 attempts: harn-cli.tar.zst' \
  "$tmp_root/fail.err"

echo "ci_wait_for_run_artifacts_test: ok"
