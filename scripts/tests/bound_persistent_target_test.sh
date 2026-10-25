#!/usr/bin/env bash
# Tests for scripts/ci/bound_persistent_target.sh. The positive case (a target
# over its ceiling is emptied) is the control for the keep cases: if the script
# never measured, every keep case would pass vacuously.
set -euo pipefail

repo_root=$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)
script="$repo_root/scripts/ci/bound_persistent_target.sh"
tmp=$(mktemp -d)
trap 'rm -rf "$tmp"' EXIT
root="$tmp/harn-ci/target"
target="$root/macos-workspace-runner"

fail() { echo "FAIL: $*" >&2; exit 1; }

fill() {
  rm -rf "$root"
  mkdir -p "$target/debug/deps"
  # 2 MiB of dependency artifacts.
  dd if=/dev/zero of="$target/debug/deps/libx.rlib" bs=1048576 count=2 2> /dev/null
}

run() { HARN_PERSISTENT_TARGET_ROOT="$root" "$script" "$@"; }

# Over the ceiling: emptied, and the directory itself survives.
fill
HARN_PERSISTENT_TARGET_MAX_GIB=0 run "$target" > "$tmp/out"
[[ -d "$target" && -z "$(ls -A "$target")" ]] || fail "an over-ceiling target was not emptied"
grep -q 'over the 0 GiB ceiling' "$tmp/out" || fail "no ceiling reason reported"

# Under the ceiling with room to spare: kept.
fill
HARN_PERSISTENT_TARGET_MAX_GIB=80 HARN_PERSISTENT_TARGET_MIN_FREE_GIB=0 run "$target" > "$tmp/out"
[[ -f "$target/debug/deps/libx.rlib" ]] || fail "an in-bounds target was emptied"
grep -q 'within bounds' "$tmp/out" || fail "no keep verdict reported"

# Volume short on space: emptied.
fill
HARN_PERSISTENT_TARGET_MAX_GIB=80 HARN_PERSISTENT_TARGET_MIN_FREE_GIB=999999 run "$target" > "$tmp/out"
[[ -z "$(ls -A "$target")" ]] || fail "a low-disk target was not emptied"
grep -q 'below 999999 GiB free' "$tmp/out" || fail "no free-space reason reported"

# A path outside the owned root is refused and untouched.
fill
mkdir -p "$tmp/elsewhere" && touch "$tmp/elsewhere/keep"
if HARN_PERSISTENT_TARGET_MAX_GIB=0 run "$tmp/elsewhere" 2> "$tmp/err"; then fail "a target outside the root was accepted"; fi
[[ -f "$tmp/elsewhere/keep" ]] || fail "a refused target was modified"
grep -q 'outside' "$tmp/err" || fail "wrong refusal reason"
# The root itself is not a target.
if HARN_PERSISTENT_TARGET_MAX_GIB=0 run "$root" 2> "$tmp/err"; then fail "the root was accepted as a target"; fi

echo "bound_persistent_target_test: ok"
