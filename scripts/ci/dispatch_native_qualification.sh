#!/usr/bin/env bash
# Main owns execution policy; only a reviewed main ancestor may be selected.
set -euo pipefail

repo="${GH_REPO:?repository required}"
sha="${SOURCE_SHA:?source required}"
[[ "$sha" =~ ^[0-9a-f]{40}$ ]] || { echo '::error::Invalid source commit.' >&2; exit 1; }
script_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
source "$script_dir/require_native_qualification.sh"
policy="$(jq -ce '[.platforms[] | select(.required)] | if length == 1 then .[0] else error("expected one required native platform") end' "$script_dir/../release_audit_contract.json")"
workflow=$(jq -r .workflow_path <<< "$policy")
record_run() {
  local id=$1
  [[ "$id" =~ ^[1-9][0-9]*$ ]] || { echo '::error::Native dispatch ID is unmeasured.' >&2; return 1; }
  if [[ -n ${GITHUB_OUTPUT:-} ]]; then
    printf 'run_id=%s\n' "$id" >> "$GITHUB_OUTPUT"
  fi
  echo "::notice::Native qualification run $id observes source $sha."
}
if qualification=$(require_native_platform "$repo" "$sha" "$policy" 2>/dev/null); then
  record_run "$(jq -er .run_id <<< "$qualification")"
  echo "::notice::Native qualification receipt at $sha already passed."
  exit 0
fi
# Reuse live main-policy runs, but never treat a title as successful proof.
runs="$(gh api --paginate --slurp "repos/$repo/actions/workflows/${workflow##*/}/runs?event=workflow_dispatch&branch=main&per_page=100")"
active=$(jq -ce --arg source "$sha" --arg repo "$repo" --arg workflow "$workflow" '
  if type != "array" or length == 0 or any(.[]; (.workflow_runs|type) != "array" or (.total_count|type) != "number")
    or ([.[].total_count]|unique|length) != 1 then error("unmeasured native runs") else . as $pages | [.[].workflow_runs[]]
  | if length != $pages[0].total_count or (map(.id)|unique|length) != length then error("partial native runs") else . end
  | [.[] | select(.display_title == ("Native qualification " + $source) and .head_branch == "main"
      and .event == "workflow_dispatch" and .path == $workflow and .repository.full_name == $repo
      and .head_repository.full_name == $repo and (.head_sha|test("^[0-9a-f]{40}$")) and .status != "completed") | .id] end' <<< "$runs")
if [[ $(jq length <<< "$active") -gt 0 ]]; then
  record_run "$(jq 'max' <<< "$active")"
  echo "::notice::Native qualification at $sha already live: runs $active."
  exit 0
fi
request=$(jq -nc --arg source "$sha" '{ref:"main",inputs:{source_sha:$source},return_run_details:true}')
dispatch=$(gh api "repos/$repo/actions/workflows/${workflow##*/}/dispatches" --method POST \
  -H 'X-GitHub-Api-Version: 2026-03-10' --input - <<< "$request")
id=$(jq -er --arg repo "$repo" '
  .workflow_run_id as $id |
  if ($id | type == "number" and . > 0 and floor == .)
    and .run_url == ("https://api.github.com/repos/" + $repo + "/actions/runs/" + ($id|tostring))
    and .html_url == ("https://github.com/" + $repo + "/actions/runs/" + ($id|tostring))
  then $id else error("native dispatch identity is unmeasured") end' <<< "$dispatch")
record_run "$id"
echo "::notice::Dispatched ${workflow##*/} from main to qualify $sha."
