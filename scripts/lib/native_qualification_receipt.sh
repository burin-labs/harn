#!/usr/bin/env bash
# Read the run-scoped artifact, never a filename or display title as proof.
read_native_qualification_receipt() {
  local repo=$1 source=$2 policy=$3 run=$4 id attempt revision pages artifact artifact_id scratch value
  id=$(jq -r .id <<< "$run")
  attempt=$(jq -r .run_attempt <<< "$run")
  revision=$(jq -r .head_sha <<< "$run")
  pages=$(gh api --paginate --slurp "repos/$repo/actions/runs/$id/artifacts?name=native-qualification-$attempt&per_page=100") || return 1
  artifact=$(jq -ce --arg name "native-qualification-$attempt" --arg revision "$revision" --argjson id "$id" '
    if type != "array" or length == 0 or any(.[]; (.artifacts|type) != "array" or (.total_count|type) != "number")
      or ([.[].total_count]|unique|length) != 1 then error("unmeasured receipt artifacts") else . as $pages | [.[].artifacts[]]
    | if length != $pages[0].total_count or length != 1 then error("missing or duplicate receipt") else .[0] end
    | if .name == $name and .expired == false and .workflow_run.id == $id and .workflow_run.head_sha == $revision
      and (.id|type == "number" and . > 0 and floor == .) then . else error("foreign receipt artifact") end end' <<< "$pages") || return 1
  artifact_id=$(jq -r .id <<< "$artifact")
  scratch=$(mktemp -d) || return 1
  if gh api "repos/$repo/actions/artifacts/$artifact_id/zip" > "$scratch/receipt.zip" &&
    [[ $(unzip -Z1 "$scratch/receipt.zip") == receipt.json ]] &&
    unzip -p "$scratch/receipt.zip" receipt.json > "$scratch/receipt.json"; then
    value=$(jq -ce --arg repo "$repo" --arg source "$source" --arg revision "$revision" --argjson id "$id" --argjson attempt "$attempt" --argjson policy "$policy" '
      if (keys|sort) == (["kind","repository","source_sha","policy_sha","run_id","run_attempt","phases"]|sort)
        and .kind == "harn.native_qualification.v1" and .repository == $repo and .source_sha == $source
        and .policy_sha == $revision and .run_id == $id and .run_attempt == $attempt and .phases == $policy.required_steps
      then . else error("native receipt identity or phases mismatch") end' "$scratch/receipt.json") || value=
  else
    value=
  fi
  rm -rf "$scratch"
  [[ -n "$value" ]] || return 1
  printf '%s\n' "$value"
}
