#!/usr/bin/env bash
# A main push lands the exact commit its merge group already built and proved,
# so the push can restore that run's shared Harn CLI instead of spending about
# 16 minutes rebuilding it. Prints `found=true` and `run_id=<id>` for a
# successful CI run of this commit that still holds an unexpired
# `harn-cli.tar.zst`, and `found=false` otherwise. The run is the commit's
# merge-group run by default; `push` selects its main push run instead, which is
# what certifies a commit for green and so what the release opener restores.
#
# Absence is safe here: `found=false` means the push builds, as it always did.
# What makes reuse correct is the restore that follows, which refuses a bundle
# whose manifest names another commit, toolchain, or build profile.
set -euo pipefail

sha=${1:?usage: find_merge_group_cli.sh <commit-sha> [merge_group|push]}
event=${2:-merge_group}
[[ "$event" == merge_group || "$event" == push ]] || { echo "event must be merge_group or push: $event" >&2; exit 2; }
[[ "$sha" =~ ^[0-9a-f]{40}$ ]] || { echo "commit must be a full 40-hex SHA: $sha" >&2; exit 2; }
repo=${GH_REPO:?GH_REPO must name owner/repo}

run_id=$(gh api "repos/$repo/actions/workflows/ci.yml/runs?event=$event&head_sha=$sha&status=success&per_page=1" \
  --jq '.workflow_runs[0].id // empty' 2>/dev/null || true)
artifact=""
if [[ "$run_id" =~ ^[0-9]+$ ]]; then
  artifact=$(gh api "repos/$repo/actions/runs/$run_id/artifacts?name=harn-cli.tar.zst" \
    --jq '[.artifacts[] | select(.expired == false)][0].id // empty' 2>/dev/null || true)
fi
if [[ "$artifact" =~ ^[0-9]+$ ]]; then
  printf 'found=true\nrun_id=%s\n' "$run_id"
  echo "Restoring the shared CLI $event run $run_id built for $sha." >&2
else
  echo 'found=false'
  echo "No $event CLI for $sha (run: ${run_id:-none}); building it." >&2
fi
