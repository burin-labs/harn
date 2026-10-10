#!/usr/bin/env bash
# Real shallow Git reproduces a push or merge group that lands several commits
# in the public API job's depth-2 checkout.
set -euo pipefail
repo_root=$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)
resolve="$repo_root/.github/scripts/resolve-public-api-baseline.sh"
gate="$repo_root/.github/scripts/breaking-surface-check.sh"
tmp=$(mktemp -d)
trap 'rm -rf "$tmp"' EXIT
source_repo="$tmp/source"
git init -q -b main "$source_repo"
git -C "$source_repo" config user.name 'Harn Test'
git -C "$source_repo" config user.email 'harn-test@example.invalid'
git -C "$source_repo" config commit.gpgsign false
git -C "$source_repo" config maintenance.auto false
mkdir -p "$source_repo/spec"
printf 'harn run\n' > "$source_repo/spec/cli-surface.txt"
commits=()
for i in $(seq 0 11); do
  printf '%s\n' "$i" > "$source_repo/README.md"
  git -C "$source_repo" add .
  git -C "$source_repo" commit -qm "commit $i"
  commits+=("$(git -C "$source_repo" rev-parse HEAD)")
done

# actions/checkout with fetch-depth: 2 on the pushed head.
checkout() {
  cd "$tmp"
  rm -rf "$tmp/checkout"
  git clone -q --depth=2 --single-branch --branch main "file://$source_repo" "$tmp/checkout"
  cd "$tmp/checkout"
}

# Negative control: the former inline step fetched only the baseline at
# depth 1, which leaves a two-commit push with no merge base.
checkout
git fetch -q --no-tags --depth=1 origin "${commits[9]}"
if git merge-base "${commits[9]}" HEAD > /dev/null 2>&1; then
  echo 'fixture did not reproduce the disconnected shallow ancestry' >&2
  exit 1
fi
status=0
BASE_SHA="${commits[9]}" HEAD_SHA=HEAD bash "$gate" cli > "$tmp/gate.log" 2>&1 || status=$?
if [[ $status != 1 ]] || ! grep -q 'commit ancestry is unreadable' "$tmp/gate.log"; then
  cat "$tmp/gate.log" >&2
  echo 'fixture did not reproduce the CI refusal' >&2
  exit 1
fi
echo 'PASS: depth-1 baseline of a two-commit push reproduces the unreadable ancestry'

checkout
EVENT=push PUSH_BEFORE_SHA="${commits[9]}" GITHUB_OUTPUT="$tmp/out" bash "$resolve" > "$tmp/resolve.log"
grep -qx "base_sha=${commits[9]}" "$tmp/out"
[[ $(git merge-base "${commits[9]}" HEAD) == "${commits[9]}" ]]
BASE_SHA="${commits[9]}" HEAD_SHA=HEAD bash "$gate" cli > "$tmp/gate.log" 2>&1
grep -q 'no cli break' "$tmp/gate.log"
echo 'PASS: a two-commit push deepens to its baseline and the comparison is measured'

checkout
EVENT=push PUSH_BEFORE_SHA="${commits[10]}" bash "$resolve" > "$tmp/resolve.log"
grep -q 'deepened=0$' "$tmp/resolve.log"
echo 'PASS: a one-commit push needs no deepening'

checkout
EVENT=pull_request bash "$resolve" > "$tmp/resolve.log"
grep -q "base=${commits[10]} " "$tmp/resolve.log"
echo 'PASS: a pull request merge commit compares against its first parent'

checkout
status=0
EVENT=merge_group MERGE_GROUP_BASE_SHA="${commits[0]}" PUBLIC_API_BASELINE_MAX_DEEPEN=8 \
  bash "$resolve" > "$tmp/resolve.log" 2>&1 || status=$?
if [[ $status != 1 ]] || ! grep -q 'unmeasured' "$tmp/resolve.log"; then
  cat "$tmp/resolve.log" >&2
  echo 'a baseline beyond the deepen cap was not refused' >&2
  exit 1
fi
EVENT=merge_group MERGE_GROUP_BASE_SHA="${commits[0]}" bash "$resolve" > "$tmp/resolve.log"
[[ $(git merge-base "${commits[0]}" HEAD) == "${commits[0]}" ]]
echo 'PASS: a distant merge-group base is refused past the cap and reached within it'

checkout
for before in 0000000000000000000000000000000000000000 ''; do
  if EVENT=push PUSH_BEFORE_SHA="$before" bash "$resolve" > "$tmp/resolve.log" 2>&1; then
    echo "push with before='$before' unexpectedly resolved a baseline" >&2
    exit 1
  fi
done
echo 'PASS: a push with no before commit refuses a baseline'
