#!/usr/bin/env bash
# Real Git reproduces a closed PR head absent from a full squash-only checkout.
set -euo pipefail
repo_root=$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)
prepare="$repo_root/.github/scripts/prepare-pr-gate-range.sh"
gate=${PR_GATE_TEST_BREAKING_SCRIPT:-$repo_root/.github/scripts/breaking-surface-check.sh}
tmp=$(mktemp -d)
trap 'rm -rf "$tmp"' EXIT
source_repo="$tmp/source"
git init -q -b main "$source_repo"
git -C "$source_repo" config user.name 'Harn Test'
git -C "$source_repo" config user.email 'harn-test@example.invalid'
git -C "$source_repo" config commit.gpgsign false
git -C "$source_repo" config maintenance.auto false
mkdir -p "$source_repo/spec"
printf 'harn run\nharn run --old\n' > "$source_repo/spec/cli-surface.txt"
git -C "$source_repo" add .
git -C "$source_repo" commit -qm base
base=$(git -C "$source_repo" rev-parse HEAD)
git -C "$source_repo" checkout -qb pr
printf 'source head\n' > "$source_repo/README.md"
git -C "$source_repo" add .
git -C "$source_repo" commit -qm 'original PR head'
head=$(git -C "$source_repo" rev-parse HEAD)
git -C "$source_repo" update-ref refs/pull/1/head "$head"
git -C "$source_repo" checkout -q main
git -C "$source_repo" merge --squash pr > "$tmp/squash.log"
git -C "$source_repo" commit -qm 'squashed PR'
git -C "$source_repo" branch -D pr > /dev/null
git clone -q --no-local --single-branch --branch main "$source_repo" "$tmp/checkout"
cd "$tmp/checkout"
if git cat-file -e "${head}^{commit}" 2>/dev/null; then
  echo 'fixture did not reproduce the absent original head' >&2
  exit 1
fi

refuse_gate() {
  local before=$1 after=$2 status=0
  BASE_SHA="$before" HEAD_SHA="$after" bash "$gate" cli > "$tmp/gate.log" 2>&1 || status=$?
  if [[ $status != 1 ]] || ! grep -q 'unmeasured' "$tmp/gate.log" \
    || grep -q 'Removed or broken:' "$tmp/gate.log"; then
    cat "$tmp/gate.log" >&2
    echo 'unreadable source was treated as a surface verdict' >&2
    exit 1
  fi
}
missing=1111111111111111111111111111111111111111
refuse_gate "$missing" HEAD
refuse_gate "$base" "$head"
echo 'PASS: missing base and closed PR head refuse an unmeasured surface'

jq -n --arg base "$base" --arg head "$head" \
  '{pull_request:{base:{sha:$base},head:{sha:$head}}}' > "$tmp/event.json"
bash "$prepare" "$tmp/event.json" > "$tmp/prepared.log" 2>&1
[[ $(git rev-parse "$head") == "$head" ]]
[[ $(git merge-base "$base" "$head") == "$base" ]]
[[ $(git rev-list "$base..$head" | wc -l | tr -d ' ') == 1 ]]
BASE_SHA="$base" HEAD_SHA="$head" bash "$gate" cli > "$tmp/gate.log"
grep -q 'no cli break' "$tmp/gate.log"
echo 'PASS: full main checkout recovers exact closed PR head and original ancestry'

jq -n --arg base "$base" --arg head "$head" \
  '{merge_group:{base_sha:$base,head_sha:$head}}' > "$tmp/queue.json"
bash "$prepare" "$tmp/queue.json" > "$tmp/queue.log"
grep -q 'endpoints=2 pending=0' "$tmp/queue.log"
echo 'PASS: nonempty merge-group range remains available'

jq -n --arg base "$base" --arg head "$missing" \
  '{pull_request:{base:{sha:$base},head:{sha:$head}}}' > "$tmp/missing.json"
if bash "$prepare" "$tmp/missing.json" > "$tmp/missing.log" 2>&1; then
  echo 'unavailable source fetch unexpectedly passed' >&2
  exit 1
fi
printf '{"pull_request":{"base":{"sha":"HEAD"},"head":{"sha":""}}}\n' > "$tmp/invalid.json"
if bash "$prepare" "$tmp/invalid.json" > "$tmp/invalid.log" 2>&1; then
  echo 'invalid event range unexpectedly passed' >&2
  exit 1
fi
echo 'PASS: unavailable source and malformed event refuse preparation'

# A known commit with an unreadable surface blob must not become file deletion.
# Use loose objects so no packed fallback can hide the absent blob.
git init -q -b main "$tmp/broken"
git -C "$tmp/broken" config user.name 'Harn Test'
git -C "$tmp/broken" config user.email 'harn-test@example.invalid'
git -C "$tmp/broken" config commit.gpgsign false
git -C "$tmp/broken" config maintenance.auto false
mkdir "$tmp/broken/spec"
printf 'harn run\n' > "$tmp/broken/spec/cli-surface.txt"
git -C "$tmp/broken" add .
git -C "$tmp/broken" commit -qm base
cd "$tmp/broken"
blob=$(git rev-parse HEAD:spec/cli-surface.txt)
rm ".git/objects/${blob:0:2}/${blob:2}"
refuse_gate HEAD HEAD
echo 'PASS: existing tree with unreadable blob refuses comparison'

# Workflow uses the same helper in both consumers of the event range.
[[ $(grep -c 'run: bash .github/scripts/prepare-pr-gate-range.sh' "$repo_root/.github/workflows/pr-gates.yml") == 2 ]]
echo 'pr_gate_range_test: ok'
