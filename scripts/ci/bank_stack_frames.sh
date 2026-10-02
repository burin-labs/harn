#!/usr/bin/env bash
#
# Bank the stack-frame budget from a census of main.
#
# Banking is deliberately not a pull-request act. A branch that rewrites the
# baseline is judged against its own measurement, which turns the gate into a
# record of whoever ran it last. So the gate refuses `--write` unless it is
# told it is running on main, and this script is the only caller that says so.
#
# It opens a pull request rather than pushing to main. The banked numbers are
# a claim about the tree, and a claim that nobody reads is how the first
# version of this gate shipped a baseline too low to hold.
#
# Usage: bank_stack_frames.sh <harn-binary>
# Requires GH_TOKEN (a token whose pull requests start CI) and GITHUB_REPOSITORY.
set -euo pipefail

harn_bin="${1:?usage: bank_stack_frames.sh <harn-binary>}"
repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
cd "$repo_root"

branch="automation/stack-frame-budget"
budget="scripts/stack-frame-budget.json"

mkdir -p .harn-tmp
./scripts/ci/collect_stack_frames.sh .harn-tmp/stack-frame-census.json

# The census refuses to be empty, but a partial one still reads as complete.
# Banking a census that lost a crate is precisely the failure this whole gate
# was rewritten over, so compare what it measured against what is already
# banked before believing it.
measured="$("$harn_bin" run scripts/check_stack_frames.harn -- \
  --census .harn-tmp/stack-frame-census.json \
  --today "$(date -u +%F)" --json | jq -r '.scanned_files')"
banked="$(jq -r '.files | length' "$budget")"
floor=$(( banked * 9 / 10 ))
if [ "$measured" -lt "$floor" ]; then
  echo "refusing to bank: the census measured $measured files against $banked banked." >&2
  echo "A census that lost a tenth of its files is a broken measurement, not a shrink." >&2
  exit 1
fi

"$harn_bin" run scripts/check_stack_frames.harn -- \
  --census .harn-tmp/stack-frame-census.json \
  --today "$(date -u +%F)" --write --bank-on main

rm -rf .harn-tmp

if git diff --quiet -- "$budget"; then
  echo "stack-frame budget already matches main; nothing to bank."
  exit 0
fi

# main requires signed commits, and a local `git commit` here is unsigned: four
# weekly runs in a row measured main, built the commit, and had the push
# refused. Publish through the same GitHub-signed path the release opener
# uses, under the release App's token so the pull request's CI runs.
HARN_BRANCH_COMMIT_TOKEN="$GH_TOKEN" \
  HARN_BRANCH_COMMIT_BRANCH="$branch" \
  HARN_BRANCH_COMMIT_BASE_OID="$(git rev-parse HEAD)" \
  HARN_BRANCH_COMMIT_HEADLINE="[CI] Bank the stack-frame budget from main" \
  "$harn_bin" run --no-sandbox scripts/bump-driver/publish_branch_commit.harn

if [ -z "$(gh pr list --head "$branch" --state open --json number --jq '.[0].number')" ]; then
  gh pr create \
    --base main \
    --head "$branch" \
    --title "[CI] Bank the stack-frame budget from main" \
    --body "Automated. The stack-frame gate measured main and these numbers moved.

Shrinkage and in-band movement already pass the gate, so nothing here unblocks a pull request. Banking keeps the numbers the gate judges against close to the tree, so a real regression is measured against recent ground rather than against whatever was true when the gate landed."
  gh pr edit "$branch" --add-label no-changelog-needed
fi
