#!/usr/bin/env bash
set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd -P)"
checker="$repo_root/scripts/check_stdlib_host_neutral.sh"
tmp_dir="$(mktemp -d "${TMPDIR:-/tmp}/harn-stdlib-host-neutral-test.XXXXXX")"
trap 'rm -rf "$tmp_dir"' EXIT

mkdir -p "$tmp_dir/stdlib"
printf '%s\n' '// host-neutral fixture' >"$tmp_dir/stdlib/example.harn"
: >"$tmp_dir/baseline.txt"

HARN_STDLIB_HOST_NEUTRAL_ROOT="$tmp_dir/stdlib" \
HARN_STDLIB_HOST_NEUTRAL_BASELINE="$tmp_dir/baseline.txt" \
  "$checker" >/dev/null

printf '%s\n' '// Burin-specific behavior must not enter the stdlib' \
  >>"$tmp_dir/stdlib/example.harn"
if HARN_STDLIB_HOST_NEUTRAL_ROOT="$tmp_dir/stdlib" \
  HARN_STDLIB_HOST_NEUTRAL_BASELINE="$tmp_dir/baseline.txt" \
  "$checker" >"$tmp_dir/out.txt" 2>"$tmp_dir/err.txt"; then
  echo "expected an unreviewed host-specific name to fail the scan" >&2
  exit 1
fi
grep -q 'unreviewed host-specific name' "$tmp_dir/err.txt"

echo "stdlib host-neutral scan test passed"
