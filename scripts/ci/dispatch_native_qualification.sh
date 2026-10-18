#!/usr/bin/env bash
# Start the exact-source macOS qualification a release candidate needs.
#
# Publication requires a successful workflow_dispatch run of the native
# qualification workflow at the candidate commit (require_native_qualification.sh).
# Nothing started it automatically, so every release waited on a person. The
# candidate producer calls this beside its build: it reuses a live or successful
# exact-source dispatch, and otherwise creates the immutable
# release-certify/<sha> ref and dispatches the workflow on it.
#
# Inputs (environment): GH_REPO, SOURCE_SHA.
set -euo pipefail

repo="${GH_REPO:?repository required}"
sha="${SOURCE_SHA:?source required}"
[[ "$sha" =~ ^[0-9a-f]{40}$ ]] || { echo '::error::Invalid source commit.' >&2; exit 1; }
script_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
policy="$script_dir/../release_audit_contract.json"
workflow="$(jq -er '[.platforms[] | select(.required)] | if length == 1 then .[0].workflow_path else error("expected one required native platform") end' "$policy")"
workflow="${workflow##*/}"
ref="release-certify/$sha"

runs="$(gh api "repos/$repo/actions/workflows/$workflow/runs?event=workflow_dispatch&head_sha=$sha&per_page=100" \
  --jq '[.workflow_runs[] | select(.status != "completed" or .conclusion == "success") | .id]')"
if [[ "$(jq length <<< "$runs")" -gt 0 ]]; then
  echo "::notice::Native qualification at $sha already live or passed: runs $(jq -c . <<< "$runs")."
  exit 0
fi

if target="$(gh api "repos/$repo/git/ref/heads/$ref" --jq .object.sha 2>/dev/null)"; then
  [[ "$target" == "$sha" ]] || {
    echo "::error::$ref points at $target, not $sha; refusing to reset a certification ref." >&2
    exit 1
  }
else
  gh api -X POST "repos/$repo/git/refs" -f "ref=refs/heads/$ref" -f "sha=$sha" >/dev/null
  target="$(gh api "repos/$repo/git/ref/heads/$ref" --jq .object.sha)"
  [[ "$target" == "$sha" ]] || { echo "::error::$ref read back $target, not $sha." >&2; exit 1; }
fi
gh workflow run "$workflow" --repo "$repo" --ref "$ref"
echo "::notice::Dispatched $workflow on $ref for native release qualification."
