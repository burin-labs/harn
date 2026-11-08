#!/usr/bin/env bash
# Name the state of the workflows that write a Rust cache key.
#
# Usage: rust_cache_writer_state.sh <shared-key>
#
# A restore that brings nothing back usually means no writer has saved the
# current generation yet. When the writer workflow is disabled, that miss lasts
# until someone re-enables it, and nothing else on the run says so: the harn
# CLI cache restored nothing for days while rust-cache-refresh.yml was
# disabled. Writers come from .github/cache-policy.json; a `${{ ... }}` part of
# a registered key matches any text. The workflow's updated_at moves on state
# changes, not file edits, so for a disabled workflow it is the disable date.
#
# Prints one line per writer. Reports only and always exits 0, so it cannot
# fail the job it describes. Needs GH_TOKEN and GITHUB_REPOSITORY.
set -uo pipefail

# shellcheck source=scripts/ci/cache_policy.sh
source "$(dirname "${BASH_SOURCE[0]}")/cache_policy.sh"

key=${1:?usage: rust_cache_writer_state.sh <shared-key>}
harn_cache_policy_require
writers="$(jq -r --arg key "$key" '
  [.writers[] | select((.shared_key | gsub("\\$\\{\\{[^}]*\\}\\}"; ".+")) as $pattern
    | $key | test("^" + $pattern + "$")) | .workflow] | unique | .[]' "$(harn_cache_policy_path)")"
if [[ -z "$writers" ]]; then
  echo "no registered writer for $key"
  exit 0
fi

while IFS= read -r workflow; do
  if ! state="$(gh api "repos/${GITHUB_REPOSITORY}/actions/workflows/${workflow}" \
    --jq '"\(.state) \(.updated_at)"' 2>/dev/null)"; then
    echo "writer ${workflow} state unknown (workflow API unavailable)"
    continue
  fi
  read -r name since <<< "$state"
  if [[ "$name" == active ]]; then
    echo "writer ${workflow} is active"
  else
    echo "writer ${workflow} is ${name} since ${since}"
  fi
done <<< "$writers"
