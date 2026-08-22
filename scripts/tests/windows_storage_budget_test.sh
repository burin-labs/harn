#!/usr/bin/env bash
set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd -P)"
script="$repo_root/scripts/ci/windows_storage_budget.sh"
tmpdir="$(mktemp -d)"
trap 'rm -rf "$tmpdir"' EXIT

mkdir -p "$tmpdir/target" "$tmpdir/cargo-home"
printf 'target\n' > "$tmpdir/target/artifact"
printf 'registry\n' > "$tmpdir/cargo-home/index"

echo "report emits one typed receipt and a matching summary"
output="$(
  CARGO_HOME="$tmpdir/cargo-home" \
    CARGO_TARGET_DIR="$tmpdir/target" \
    GITHUB_STEP_SUMMARY="$tmpdir/summary" \
    "$script" report after_restore
)"
for field in \
  'windows_storage_phase=after_restore' \
  'windows_storage_total_bytes=' \
  'windows_storage_used_bytes=' \
  'windows_storage_free_bytes=' \
  'windows_storage_target_bytes=' \
  'windows_storage_cargo_home_bytes='
do
  [[ "$output" == *"$field"* ]] || {
    echo "missing storage receipt field: $field" >&2
    exit 1
  }
done
grep -Fq "$output" "$tmpdir/summary"

echo "missing targets report zero without hiding filesystem capacity"
missing_output="$(
  HOME="$tmpdir/missing-home" CARGO_HOME='' CARGO_TARGET_DIR="$tmpdir/missing-target" \
    "$script" report initial
)"
[[ "$missing_output" == *'windows_storage_phase=initial'* ]]
[[ "$missing_output" == *'windows_storage_target_bytes=0'* ]]
[[ "$missing_output" == *'windows_storage_cargo_home_bytes=0'* ]]

echo "invalid phase labels fail before producing a receipt"
if "$script" report 'after restore' >"$tmpdir/invalid.out" 2>"$tmpdir/invalid.err"; then
  echo "invalid phase unexpectedly passed" >&2
  exit 1
fi
grep -Fq 'PHASE must match' "$tmpdir/invalid.err"
[[ ! -s "$tmpdir/invalid.out" ]]

echo "windows_storage_budget_test: ok"
