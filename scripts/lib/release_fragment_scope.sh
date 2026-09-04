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
tree_commit="$(git -C "$repo" rev-parse --verify "$tree^{commit}" 2>/dev/null || true)"
tag_ref="refs/tags/v${version}"
tag_type="$(git -C "$repo" cat-file -t "$tag_ref" 2>/dev/null || true)"
tag_target="$(git -C "$repo" rev-parse --verify "$tag_ref^{commit}" 2>/dev/null || true)"

matched=""
if [[ "$tag_type" == "tag" && -n "$tree_commit" && "$tag_target" == "$tree_commit" ]]; then
  tag_contents="$(git -C "$repo" for-each-ref --format='%(contents)' "$tag_ref" 2>/dev/null || true)"
  candidate_lines="$(grep -E '^Harn-Release-Candidate: [0-9a-fA-F]{40}$' <<< "$tag_contents" || true)"
  declared_lines="$(grep -E '^Harn-Release-Candidate:' <<< "$tag_contents" || true)"
  candidate_count="$(grep -c . <<< "$candidate_lines" || true)"
  declared_count="$(grep -c . <<< "$declared_lines" || true)"
  if [[ "$declared_count" -eq 1 && "$candidate_count" -eq 1 ]]; then
    candidate="$(sed -E 's/^Harn-Release-Candidate: //' <<< "$candidate_lines" | tr '[:upper:]' '[:lower:]')"
    candidate_blob="$(git -C "$repo" rev-parse --verify "$candidate:CHANGELOG.md" 2>/dev/null || true)"
    if [[ -n "$notes_blob" && "$candidate_blob" == "$notes_blob" ]]; then
      matched="$candidate"
    fi
  fi
fi

refs="$(git -C "$repo" for-each-ref \
  --format='%(objectname) %(refname)' \
  "refs/heads/release-attempt/v${version}/*" \
  "refs/remotes/*/release-attempt/v${version}/*" 2>/dev/null || true)"

if [[ -z "$matched" && "${declared_count:-0}" -eq 0 ]]; then
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
fi

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
