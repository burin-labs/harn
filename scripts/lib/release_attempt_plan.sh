# shellcheck shell=bash
# The opener owns recovery; this read-only plan names the immutable attempts
# and proves which conflicts can be refolded or regenerated from green.
release_attempt_plan() {
  local version=$1 green=$2 prs=$3 main_oid pr url branch oid record base output status tree path main_blob green_blob
  local clean='[]' superseded='[]' waiting='[]' conflicts=()
  jq -e 'type == "array" and all(.[]; (.url|type)=="string" and (.headRefName|type)=="string")' <<< "$prs" >/dev/null || return 1
  # Actions checks out green at depth one. Merge classification needs the real
  # common ancestor, including an attempt cut from an earlier green checkpoint.
  if [[ $(git rev-parse --is-shallow-repository) == true ]]; then
    git fetch --quiet --unshallow origin main || return 1
  else
    git fetch --quiet origin main || return 1
  fi
  main_oid=$(git rev-parse --verify FETCH_HEAD) || return 1
  while IFS= read -r pr; do
    url=$(jq -r .url <<< "$pr")
    branch=$(jq -r .headRefName <<< "$pr")
    git check-ref-format "refs/heads/$branch" || return 1
    git fetch --quiet origin "refs/heads/$branch" || return 1
    oid=$(git rev-parse --verify FETCH_HEAD) || return 1
    if [[ "$branch" == release-attempt/* && "$branch" != "release-attempt/v$version/$oid" ]]; then
      echo "error: immutable attempt ref does not name its head: $branch" >&2
      return 1
    fi
    record=$(git ls-remote --refs origin "refs/heads/release-attempt/v$version/$oid") || return 1
    if [[ "$record" != "$oid"$'\t'"refs/heads/release-attempt/v$version/$oid" ]]; then
      echo "error: $branch has no matching immutable release attempt; refusing recovery" >&2
      return 1
    fi
    pr=$(jq -c --arg oid "$oid" '. + {head_sha:$oid}' <<< "$pr")
    output=$(mktemp) || return 1
    status=0
    git merge-tree --write-tree --name-only -z "$main_oid" "$oid" > "$output" || status=$?
    if (( status == 0 )); then
      clean=$(jq -c --argjson pr "$pr" '. + [$pr]' <<< "$clean")
      rm -f -- "$output"
      continue
    fi
    if (( status != 1 )); then
      rm -f -- "$output"
      return 1
    fi
    conflicts=()
    {
      IFS= read -r -d '' tree
      while IFS= read -r -d '' path && [[ -n "$path" ]]; do conflicts+=("$path"); done
    } < "$output"
    rm -f -- "$output"
    [[ "$tree" =~ ^[0-9a-f]{40}$ && ${#conflicts[@]} -gt 0 ]] || return 1
    base=$(git merge-base "$main_oid" "$oid") || return 1
    for path in "${conflicts[@]}"; do
      if release_fragment_path "$path" &&
        [[ $(git diff --name-only --no-renames --diff-filter=D "$base" "$oid" -- "$path") == "$path" ]] &&
        [[ $(git diff --name-only --no-renames --diff-filter=M "$base" "$main_oid" -- "$path") == "$path" ]]; then
        :
      elif ! python3 "$(dirname "${BASH_SOURCE[0]}")/release-generated-artifacts.py" matches "$path"; then
        echo "error: release conflict is neither a modified folded fragment nor a regenerated release artifact: $path" >&2
        return 1
      fi
      main_blob=$(git rev-parse --verify "$main_oid:$path") || return 1
      green_blob=$(git rev-parse --verify "$green:$path" 2>/dev/null) || green_blob=''
      if [[ "$green_blob" != "$main_blob" ]]; then
        waiting=$(jq -c --arg path "$path" '. + [$path] | unique' <<< "$waiting")
      fi
    done
    pr=$(jq -c --argjson conflicts "$(printf '%s\n' "${conflicts[@]}" | jq -Rsc 'split("\n")[:-1]')" \
      '. + {conflicts:$conflicts}' <<< "$pr")
    superseded=$(jq -c --argjson pr "$pr" '. + [$pr]' <<< "$superseded")
  done < <(jq -c '.[]' <<< "$prs")
  if (( $(jq length <<< "$clean") > 1 )); then
    echo 'error: multiple mergeable release attempts; refusing an ambiguous source' >&2
    return 1
  fi
  jq -nc --arg main "$main_oid" --argjson clean "$clean" --argjson superseded "$superseded" --argjson waiting "$waiting" \
    '{main_sha:$main,current:($clean[0] // null),superseded:$superseded,waiting_for_green:$waiting,
      action:(if ($waiting|length)>0 then "wait-green" elif ($clean|length)>0 then
        (if ($superseded|length)>0 then "reconcile" else "existing" end) else "open" end)}'
}

release_deliver_attempt() {
  local plan=$1 current_url=$2 current_sha=$3 newly_opened=${4:-false} old url expected meta closing=()
  meta=$(gh pr view "$current_url" --json state,headRefOid,isCrossRepository) || return 1
  if ! jq -e --arg expected "$current_sha" '.isCrossRepository == false and .headRefOid == $expected and .state == "OPEN"' <<< "$meta" >/dev/null; then
    echo "error: replacement attempt changed or is no longer open: $current_url" >&2
    return 1
  fi
  while IFS= read -r old; do
    url=$(jq -r .url <<< "$old")
    expected=$(jq -r .head_sha <<< "$old")
    meta=$(gh pr view "$url" --json state,headRefOid,isCrossRepository) || return 1
    if ! jq -e --arg expected "$expected" '.isCrossRepository == false and .headRefOid == $expected and (.state == "OPEN" or .state == "CLOSED")' <<< "$meta" >/dev/null; then
      echo "error: superseded attempt changed or merged: $url" >&2
      return 1
    fi
    if [[ $(jq -r .state <<< "$meta") == OPEN ]]; then
      closing+=("$url")
    fi
  done < <(jq -c '.superseded[]' <<< "$plan")
  # Arm only a newly created attempt. A retry must preserve an explicit Ship
  # withdrawal on an existing attempt, including interrupted replacements.
  if [[ "$newly_opened" == true ]]; then
    release_enroll_ship "$current_url" || return 1
  fi
  for url in ${closing[@]+"${closing[@]}"}; do
    gh pr close "$url" --comment "Superseded by $current_url at immutable attempt $current_sha after preparing a coherent replacement from green. The earlier attempt remains recorded." || return 1
  done
}
