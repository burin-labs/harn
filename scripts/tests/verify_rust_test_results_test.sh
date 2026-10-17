#!/usr/bin/env bash
set -euo pipefail
root=$(cd "$(dirname "$0")/../.." && pwd)
scratch=$(mktemp -d)
trap 'rm -rf "$scratch"' EXIT
normal=(CHANGES_RESULT=success RUST_SELECTED=true PRODUCER_RESULT=success PARTITIONS_RESULT=success ARCHIVE_RESULT=skipped)
archive=(CHANGES_RESULT=success RUST_SELECTED=true ARCHIVE_MODE=true PRODUCER_RESULT=success WORKSPACE_ARTIFACT_READY=true PARTITIONS_RESULT=skipped ARCHIVE_RESULT=success)
unselected=(CHANGES_RESULT=success RUST_SELECTED=false PRODUCER_RESULT=skipped PARTITIONS_RESULT=skipped ARCHIVE_RESULT=skipped)
cases=0
check() {
  local expected=$1
  shift
  local actual=0
  env -i PATH="$PATH" "$@" bash "$root/scripts/ci/verify_rust_test_results.sh" > "$scratch/result" 2>&1 || actual=$?
  if [[ "$actual" != "$expected" ]]; then
    cat "$scratch/result" >&2
    echo "Expected exit $expected, got $actual" >&2
    exit 1
  fi
  grep -Fq 'pending=' "$scratch/result"
  grep -Fq 'failing=' "$scratch/result"
  cases=$((cases + 1))
}
check 0 "${normal[@]}"
check 0 "${archive[@]}"
check 0 "${unselected[@]}"
for result in '' failure cancelled skipped pending; do
  check 1 "${normal[@]}" PARTITIONS_RESULT="$result"
  grep -Fq 'Required Rust proof did not complete: Rust test partitions' "$scratch/result"
  check 1 "${normal[@]}" PRODUCER_RESULT="$result"
  check 1 "${archive[@]}" ARCHIVE_RESULT="$result"
done
check 1 "${normal[@]}" PARTITIONS_RESULT=pending
grep -Fq 'pending=1 failing=1' "$scratch/result"
check 1 "${normal[@]}" CHANGES_RESULT=failure
check 1 "${normal[@]}" RUST_SELECTED=
check 1 "${archive[@]}" WORKSPACE_ARTIFACT_READY=
check 1 "${archive[@]}" PARTITIONS_RESULT=success
check 1 "${normal[@]}" ARCHIVE_RESULT=success
check 1 "${unselected[@]}" PARTITIONS_RESULT=success
echo "Rust test aggregate: $cases cases passed, 0 skipped"
