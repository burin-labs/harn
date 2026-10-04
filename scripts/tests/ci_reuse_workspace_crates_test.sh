#!/usr/bin/env bash
# `reuse_workspace_crates.sh restore` must leave every unchanged tracked path
# older than any build and every changed path newer, and must never act on a
# record it cannot trust. A stale crate passing as fresh ships an old binary as
# a green build, so the negative cases matter as much as the positive one.
set -euo pipefail
script=${HARN_WORKSPACE_REUSE_SCRIPT:-"$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)/ci/reuse_workspace_crates.sh"}

root=$(mktemp -d)
trap 'rm -rf "$root"' EXIT
cd "$root"
git init -q -b main .
git config user.email test@example.com
git config user.name test
git config commit.gpgsign false
mkdir -p crates/a/src crates/b/src crates/c/src
echo a > crates/a/src/lib.rs
echo b > crates/b/src/lib.rs
echo c > crates/c/src/lib.rs
printf 'target/\n.harn-workspace-source/\n' > .gitignore
git add -A && git commit -q -m one
record=.harn-workspace-source/commit

# A dependency cache hit cannot attest a workspace source. Reach the actual
# record reader with a non-null current record, then damage that same record.
(cd crates/a && "$script" record >/dev/null)
"$script" current
for damaged in missing malformed wrong; do
  case "$damaged" in
    missing) rm -f "$record" ;;
    malformed) echo not-a-commit > "$record" ;;
    wrong) printf '%040d\n' 0 > "$record" ;;
  esac
  if "$script" current; then
    echo "source attestation accepted $damaged record" >&2
    exit 1
  fi
done
"$script" record >/dev/null
echo changed > crates/a/src/lib.rs
if "$script" current; then
  echo "source attestation accepted modified working source" >&2
  exit 1
fi
git add crates/a/src/lib.rs
if "$script" current; then
  echo "source attestation accepted staged source" >&2
  exit 1
fi
git commit -q -m changed
if "$script" current; then
  echo "source attestation accepted stale generation" >&2
  exit 1
fi
"$script" record >/dev/null
"$script" current
echo hidden > crates/a/src/untracked.rs
if "$script" current; then
  echo "source attestation accepted untracked native source" >&2
  exit 1
fi
rm -f crates/a/src/untracked.rs
echo docs > README.md
git add README.md && git commit -q -m docs
"$script" current
mkdir -p "$root/target"
output="$root/target/output"
GITHUB_OUTPUT="$output" "$script" restore >/dev/null
[[ "$(cat "$output")" == "source-current=true" ]]
[[ ! -e "$record" ]]
: > "$output"
GITHUB_OUTPUT="$output" "$script" restore >/dev/null
[[ "$(cat "$output")" == "source-current=false" ]]
echo "ci_reuse_workspace_crates_test: source attestation controls passed"

probe="$root/probe"
touch "$probe"
if ! touch -h -d @946684800 "$probe" 2>/dev/null \
  || [[ "$(stat -c %Y "$probe" 2>/dev/null)" != 946684800 ]]; then
  rm -f "$probe"
  # The producer runners are Linux. Elsewhere the script refuses by name, so
  # check that refusal instead of the stamps it cannot set.
  out=$("$script" restore)
  [[ "$out" == *"cannot set file times here"* ]]
  echo "ci_reuse_workspace_crates_test: ok (refusal where file times cannot be set)"
  exit 0
fi
rm -f "$probe"

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
