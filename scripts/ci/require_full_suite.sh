#!/usr/bin/env bash
# A release needs a completed main-push CI run at its immutable source.
set -euo pipefail
: "${GH_REPO:?repository required}"
: "${SOURCE_SHA:?source required}"
[[ "$SOURCE_SHA" =~ ^[0-9a-f]{40}$ ]] || exit 1
if [[ "${WAIT_FOR_FULL_SUITE:-false}" == true ]]; then
  active=$(gh api "repos/$GH_REPO/actions/workflows/ci.yml/runs?event=push&branch=main&head_sha=$SOURCE_SHA&per_page=100" --jq '.workflow_runs | sort_by(.id) | last | select(.status != "completed") | .id // empty')
  if [[ -n "$active" ]]; then
    gh run watch "$active" --repo "$GH_REPO" --exit-status
  fi
fi
runs=$(gh api --paginate "repos/$GH_REPO/actions/workflows/ci.yml/runs?event=push&branch=main&head_sha=$SOURCE_SHA&per_page=100" --jq '.workflow_runs[] | [.id, .head_sha, .status, .conclusion] | @tsv')
while IFS=$'\t' read -r id sha status conclusion; do
  [[ "$sha" == "$SOURCE_SHA" && "$status" == completed && "$conclusion" == success ]] || continue
  jobs=$(gh api --paginate "repos/$GH_REPO/actions/runs/$id/jobs?filter=latest&per_page=100" --jq '.jobs[] | [.name, .status, .conclusion] | @tsv')
  # Require the deferred families explicitly. An older workflow's successful
  # aggregate can have skipped these jobs and is not full-suite proof.
  missing=0
  for name in 'CI status' 'Verify publishable crates' 'Stack frame budget' 'Rust workspace tests' 'Run Linux sandbox tests' 'Windows cross-compile check' 'Rust on macOS (deny-warnings build + lint)'; do
    if ! grep -Fxq "$name"$'\tcompleted\tsuccess' <<< "$jobs"; then
      echo "Full-suite proof missing: $name (run $id)" >&2
      missing=$((missing + 1))
    fi
  done
  if (( missing == 0 )); then
    echo "Full-suite CI succeeded at $SOURCE_SHA: https://github.com/$GH_REPO/actions/runs/$id"
    exit 0
  fi
done <<< "$runs"
echo "No green full-suite main push at $SOURCE_SHA; release refused." >&2
exit 1
