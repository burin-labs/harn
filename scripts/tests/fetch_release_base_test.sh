#!/usr/bin/env bash
set -euo pipefail

repo_root=$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd -P)
scratch=$(mktemp -d "${TMPDIR:-/tmp}/harn-release-base.XXXXXX")
trap 'rm -rf -- "$scratch"' EXIT

git init --quiet --initial-branch main "$scratch/source"
git -C "$scratch/source" config user.name "Release base fixture"
git -C "$scratch/source" config user.email "fixture@localhost"
git -C "$scratch/source" config commit.gpgsign false
printf 'base\n' > "$scratch/source/state.txt"
git -C "$scratch/source" add state.txt
git -C "$scratch/source" commit --quiet -m base
base_sha=$(git -C "$scratch/source" rev-parse HEAD)
git -C "$scratch/source" branch retired-parent
printf 'head\n' > "$scratch/source/state.txt"
git -C "$scratch/source" commit --quiet -am head
head_sha=$(git -C "$scratch/source" rev-parse HEAD)
git clone --quiet --bare "$scratch/source" "$scratch/remote.git"
git --git-dir "$scratch/remote.git" update-ref -d refs/heads/retired-parent
git clone --quiet "$scratch/remote.git" "$scratch/checkout"

cd "$scratch/checkout"
if git fetch --no-tags origin '+refs/heads/retired-parent:refs/remotes/origin/retired-parent' \
  > "$scratch/legacy.log" 2>&1; then
  echo "error: negative control unexpectedly fetched a deleted branch" >&2
  exit 1
fi
bash "$repo_root/scripts/ci/fetch_release_base.sh" retired-parent "$base_sha"
[[ $(git rev-parse refs/remotes/origin/retired-parent) == "$base_sha" ]]
[[ $(git show refs/remotes/origin/retired-parent:state.txt) == base ]]
[[ $(git rev-parse HEAD) == "$head_sha" && $(cat state.txt) == head ]]

git --git-dir "$scratch/remote.git" update-ref refs/heads/retired-parent "$head_sha"
git fetch --no-tags origin '+refs/heads/retired-parent:refs/remotes/origin/retired-parent'
[[ $(git rev-parse refs/remotes/origin/retired-parent) == "$head_sha" ]]
bash "$repo_root/scripts/ci/fetch_release_base.sh" retired-parent "$base_sha"
[[ $(git rev-parse refs/remotes/origin/retired-parent) == "$base_sha" ]]

if bash "$repo_root/scripts/ci/fetch_release_base.sh" retired-parent '' \
  > "$scratch/missing.log" 2>&1; then
  echo "error: a missing SHA silently selected a remote branch tip" >&2
  exit 1
fi
if bash "$repo_root/scripts/ci/fetch_release_base.sh" 'invalid ref' "$base_sha" \
  > "$scratch/invalid.log" 2>&1; then
  echo "error: invalid local base ref was accepted" >&2
  exit 1
fi
[[ $(git rev-parse refs/remotes/origin/retired-parent) == "$base_sha" ]]
echo "release base: deleted-parent, moved-tip, exact-content, unchanged-head, and missing-input controls passed"
