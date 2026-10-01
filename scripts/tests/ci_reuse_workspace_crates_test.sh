#!/usr/bin/env bash
# `reuse_workspace_crates.sh restore` must leave every unchanged tracked path
# older than any build and every changed path newer, and must never act on a
# record it cannot trust. A stale crate passing as fresh ships an old binary as
# a green build, so the negative cases matter as much as the positive one.
set -euo pipefail
script="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)/ci/reuse_workspace_crates.sh"

probe=$(mktemp)
if ! touch -h -d @946684800 "$probe" 2>/dev/null \
  || [[ "$(stat -c %Y "$probe" 2>/dev/null)" != 946684800 ]]; then
  rm -f "$probe"
  # The producer runners are Linux. Elsewhere the script refuses by name, so
  # check that refusal instead of the stamps it cannot set.
  out=$(cd "$(mktemp -d)" && git init -q . && "$script" restore)
  [[ "$out" == *"cannot set file times here"* ]]
  echo "ci_reuse_workspace_crates_test: ok (refusal where file times cannot be set)"
  exit 0
fi
rm -f "$probe"

root=$(mktemp -d)
trap 'rm -rf "$root"' EXIT
cd "$root"
git init -q -b main .
git config user.email test@example.com
git config user.name test
mkdir -p crates/a/src crates/b/src crates/c/src
echo a > crates/a/src/lib.rs
echo b > crates/b/src/lib.rs
echo c > crates/c/src/lib.rs
printf 'target/\n.harn-workspace-source/\n' > .gitignore
git add -A && git commit -q -m one
record=.harn-workspace-source/commit
# Recording from a subdirectory still writes the record at the checkout root,
# where the rust-cache action saves it.
(cd crates/a && "$script" record >/dev/null)
[[ "$(cat "$record")" == "$(git rev-parse HEAD)" ]]

echo b2 > crates/b/src/lib.rs
git rm -q crates/c/src/lib.rs
mkdir -p crates/c/src && echo new > crates/c/src/new.rs
git add -A && git commit -q -m two

build_time=1577836800 # 2020-01-01T00:00:00Z
stamp() { stat -c %Y "$1"; }
out=$("$script" restore)
[[ "$out" == *"3 path(s) changed"* ]]
[[ ! -e "$record" ]]
(( $(stamp crates/a/src/lib.rs) < build_time ))
(( $(stamp crates/a/src) < build_time ))
(( $(stamp crates/b/src/lib.rs) > build_time ))
(( $(stamp crates/c/src/new.rs) > build_time ))
# The deleted file's directory must look changed to a build script watching it.
(( $(stamp crates/c/src) > build_time ))
# Untouched siblings of a changed directory stay old.
(( $(stamp crates/a) < build_time ))

# No record: nothing is back-dated, so everything rebuilds as before.
touch crates/a/src/lib.rs
out=$("$script" restore)
[[ "$out" == *"no source record"* ]]
(( $(stamp crates/a/src/lib.rs) > build_time ))

# An unreadable or unfetchable record is dropped and changes nothing.
echo not-a-commit > "$record"
out=$("$script" restore)
[[ "$out" == *"unreadable source record"* ]]
[[ ! -e "$record" ]]
(( $(stamp crates/a/src/lib.rs) > build_time ))
printf '%040d\n' 0 > "$record"
out=$("$script" restore)
[[ "$out" == *"cannot fetch"* ]]
(( $(stamp crates/a/src/lib.rs) > build_time ))

echo "ci_reuse_workspace_crates_test: ok"
