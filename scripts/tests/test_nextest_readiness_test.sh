#!/usr/bin/env bash
set -euo pipefail

repo_root=$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd -P)
tmp_root=$(mktemp -d)
trap 'rm -rf "$tmp_root"' EXIT
mkdir "$tmp_root/bin" "$tmp_root/empty-path"
make_bin=$(command -v make)
export NEXTEST_PROBE_LOG="$tmp_root/probe.log"
export NEXTEST_BUILD_LOG="$tmp_root/build.log"

cat > "$tmp_root/bin/cargo-nextest" <<'SH'
#!/bin/bash
set -euo pipefail
[[ "$*" == 'nextest --version' ]]
printf '%s\n' "$*" >> "$NEXTEST_PROBE_LOG"
exit "${NEXTEST_PROBE_STATUS:-0}"
SH
cat > "$tmp_root/build-runner" <<'SH'
#!/bin/bash
set -euo pipefail
printf '%s\n' "$*" >> "$NEXTEST_BUILD_LOG"
if [[ "$*" == 'nextest --version' ]]; then
  echo 'version probe reached heavy admission' >&2
  exit 92
fi
[[ "$*" == 'nextest run -p harn-vm --lib' ]]
SH
chmod +x "$tmp_root/bin/cargo-nextest" "$tmp_root/build-runner"

PATH="$tmp_root/bin:$PATH" make -s -C "$repo_root" test \
  HARN_CARGO_CMD="$tmp_root/build-runner" ARGS='-p harn-vm --lib'
[[ $(wc -l < "$NEXTEST_PROBE_LOG") -eq 1 ]]
[[ $(wc -l < "$NEXTEST_BUILD_LOG") -eq 1 ]]
grep -Fxq 'nextest run -p harn-vm --lib' "$NEXTEST_BUILD_LOG"

# A broken or missing readiness probe must stop before test admission.
: > "$NEXTEST_BUILD_LOG"
if PATH="$tmp_root/bin:$PATH" NEXTEST_PROBE_STATUS=17 \
  make -s -C "$repo_root" test HARN_CARGO_CMD="$tmp_root/build-runner" \
  ARGS='-p harn-vm --lib' > "$tmp_root/broken.out" 2>&1; then
  echo 'broken nextest probe unexpectedly admitted tests' >&2
  exit 1
fi
[[ ! -s "$NEXTEST_BUILD_LOG" ]]
if PATH="$tmp_root/empty-path" "$make_bin" -s -C "$repo_root" test \
  HARN_CARGO_CMD="$tmp_root/build-runner" ARGS='-p harn-vm --lib' \
  > "$tmp_root/missing.out" 2>&1; then
  echo 'missing nextest unexpectedly admitted tests' >&2
  exit 1
fi
grep -Fq 'cargo-nextest is required' "$tmp_root/missing.out"
[[ ! -s "$NEXTEST_BUILD_LOG" ]]
echo 'nextest readiness admission tests passed'
