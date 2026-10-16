#!/usr/bin/env bash
# Resolve the same package selection for every workspace-test partition.
set -euo pipefail

case "${GITHUB_EVENT_NAME:-local}" in
  pull_request|merge_group)
    if [[ "${POST_MERGE_TIER_ACTIVE:-false}" != true ]]; then
      echo --workspace
      exit 0
    fi
    base=$(jq -er '.merge_group.base_sha // .pull_request.base.sha' "${GITHUB_EVENT_PATH:?event path required}")
    if [[ "$GITHUB_EVENT_NAME" == pull_request ]]; then
      merge_base=$(git cat-file -p HEAD | awk '/^parent / {n++; if (n == 1) first=$2} END {if (n == 2) print first}')
      base=${merge_base:-$base}
    fi
    [[ "$base" =~ ^[0-9a-f]{40}$ ]] || { echo 'Invalid test diff base' >&2; exit 1; }
    git cat-file -e "$base^{commit}" 2>/dev/null || git fetch --no-tags --depth=1 origin "$base"
    paths=$(mktemp)
    trap 'rm -f "$paths"' EXIT
    git diff --name-only "$base" HEAD > "$paths"
    args=$(bash scripts/ci/affected_crate_args.sh --changed-files-file "$paths")
    # A Rust lane with no selected crate still owes measured proof. Non-crate
    # inputs and an empty diff conservatively select the complete workspace.
    printf '%s\n' "${args:---workspace}"
    ;;
  *) echo --workspace ;;
esac
