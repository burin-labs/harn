#!/usr/bin/env bash
# Classify changelog fragments in one release tree against its immutable candidate.
set -euo pipefail

repo="."
tree="HEAD"
version=""
while [[ $# -gt 0 ]]; do
  case "$1" in
    --repo) repo="$2"; shift 2 ;;
    --tree) tree="$2"; shift 2 ;;
    --version) version="$2"; shift 2 ;;
    *) echo "usage: release_fragment_scope.sh --version X.Y.Z [--repo path] [--tree rev]" >&2; exit 2 ;;
  esac
done

if [[ ! "$version" =~ ^[0-9]+\.[0-9]+\.[0-9]+$ ]]; then
  echo "error: --version must be a stable release version" >&2
  exit 2
fi

list_fragments() {
  git -C "$repo" ls-tree -r --name-only "$1" -- changelog.d 2>/dev/null \
    | grep -E '^changelog\.d/[^/]+\.(breaking|added|changed|deprecated|removed|fixed|security)\.md$' \
    || true
}

json_lines() {
  jq -Rsc 'split("\n") | map(select(length > 0))'
}

current="$(list_fragments "$tree")"
notes_blob="$(git -C "$repo" rev-parse --verify "$tree:CHANGELOG.md" 2>/dev/null || true)"
refs="$(git -C "$repo" for-each-ref \
  --format='%(objectname) %(refname)' \
  "refs/heads/release-attempt/v${version}/*" \
  "refs/remotes/*/release-attempt/v${version}/*" 2>/dev/null || true)"

matched=""
while read -r oid _ref; do
  [[ -n "${oid:-}" ]] || continue
  candidate_blob="$(git -C "$repo" rev-parse --verify "$oid:CHANGELOG.md" 2>/dev/null || true)"
  if [[ -n "$notes_blob" && "$candidate_blob" == "$notes_blob" ]]; then
    case " $matched " in
      *" $oid "*) ;;
      *) matched="${matched:+$matched }$oid" ;;
    esac
  fi
done <<< "$refs"

read -r -a matches <<< "$matched"
if [[ -z "$notes_blob" || ${#matches[@]} -ne 1 ]]; then
  detail="cannot resolve exactly one immutable v${version} candidate from the release tree"
  jq -n \
    --arg detail "$detail" \
    --argjson owned "$(printf '%s\n' "$current" | json_lines)" \
    '{schema_version:"harn.release_fragment_scope.v1",resolved:false,candidate_commit:"",owned:$owned,deferred:[],detail:$detail}'
  exit 0
fi

candidate="${matches[0]}"
candidate_fragments="$(list_fragments "$candidate")"
owned=""
deferred=""
while IFS= read -r path; do
  [[ -n "$path" ]] || continue
  if grep -Fxq "$path" <<< "$candidate_fragments"; then
    owned="${owned}${owned:+$'\n'}$path"
  else
    deferred="${deferred}${deferred:+$'\n'}$path"
  fi
done <<< "$current"

jq -n \
  --arg candidate "$candidate" \
  --argjson owned "$(printf '%s\n' "$owned" | json_lines)" \
  --argjson deferred "$(printf '%s\n' "$deferred" | json_lines)" \
  '{schema_version:"harn.release_fragment_scope.v1",resolved:true,candidate_commit:$candidate,owned:$owned,deferred:$deferred,detail:""}'
