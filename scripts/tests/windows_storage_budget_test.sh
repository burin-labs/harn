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

# The floor is a gate, so both directions get a control. A one-sided test would
# pass just as happily against a --require-free-headroom flag that never
# compares anything.
policy_dir="$tmpdir/policy"
mkdir -p "$policy_dir"
write_policy() {
  cat > "$policy_dir/cache-policy.json" <<POLICY
{"schema_version": 6, "windows_workspace_warm": {"build_headroom_bytes": $1}}
POLICY
}

echo "a reading above the policy floor still passes with the receipt"
write_policy 1
above_output="$(
  HARN_CACHE_POLICY_PATH="$policy_dir/cache-policy.json"     CARGO_HOME="$tmpdir/cargo-home"     CARGO_TARGET_DIR="$tmpdir/target"     "$script" report before_build --require-free-headroom
)"
[[ "$above_output" == *'windows_storage_phase=before_build'* ]]

echo "a reading below the policy floor fails with both numbers in the message"
# 2^62 bytes: no real filesystem clears it, so this control cannot pass by luck.
write_policy 4611686018427387904
if HARN_CACHE_POLICY_PATH="$policy_dir/cache-policy.json"   CARGO_HOME="$tmpdir/cargo-home"   CARGO_TARGET_DIR="$tmpdir/target"   "$script" report before_build --require-free-headroom   >"$tmpdir/floor.out" 2>"$tmpdir/floor.err"; then
  echo "reading below the build headroom floor unexpectedly passed" >&2
  exit 1
fi
grep -Fq '4611686018427387904-byte build headroom floor' "$tmpdir/floor.err"
grep -Eq 'free space [0-9]+ bytes is below' "$tmpdir/floor.err"
# The failing run publishes the same receipt a healthy run publishes.
grep -Fq 'windows_storage_phase=before_build' "$tmpdir/floor.out"

echo "the floor is opt-in: the same undersized reading passes without the flag"
HARN_CACHE_POLICY_PATH="$policy_dir/cache-policy.json"   CARGO_HOME="$tmpdir/cargo-home"   CARGO_TARGET_DIR="$tmpdir/target"   "$script" report before_build >/dev/null

echo "windows_storage_budget_test: ok"
