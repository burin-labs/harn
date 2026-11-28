#!/usr/bin/env bash
# Read original producer history. Parsing pairing claims remains in consumer-pairing.mjs.

source "$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)/release_version.sh"

release_pairing_relevant_tags() {
  local version=$1 oid reference extra tag
  while read -r oid reference extra; do
    [[ -z "$extra" && "$oid" =~ ^[0-9a-f]{40}$ && "$reference" == refs/tags/* ]] || return 1
    tag=${reference#refs/tags/}
    tag=${tag%"^{}"}
    release_tag_is_canonical "$tag" || continue
    release_version_is_prerelease "${tag#v}" && continue
    release_workspace_supersedes_source "${tag#v}" "$version" || continue
    printf '%s %s\n' "$oid" "$reference"
  done
}

release_pairing_source_json() (
  set -euo pipefail
  # The authenticated SHA selects original objects, not local replacement or graft views.
  export GIT_NO_REPLACE_OBJECTS=1 GIT_GRAFT_FILE=/dev/null
  local directory=${1:-} revision=${2:-} version base base_sha count text local_tags remote_tags
  source_refused() { echo 'CONSUMER_OBSERVER reason=pairing_source_unmeasured' >&2; exit 1; }
  source_git() { git --no-replace-objects --no-optional-locks -C "$directory" "$@" 2>/dev/null; }
  [[ -n "$directory" && "$revision" =~ ^[0-9a-f]{40}$ ]] || source_refused
  [[ "$(source_git rev-parse --is-shallow-repository)" == false ]] || source_refused
  [[ "$(source_git rev-parse --verify "$revision^{commit}")" == "$revision" ]] || source_refused
  version="$(source_git show "$revision:Cargo.toml" | release_workspace_version)" || source_refused
  release_version_is_canonical "$version" && ! release_version_is_prerelease "$version" || source_refused
  # Compare the relevant remote tag census rather than assuming checkout fetched it.
  # Later publications at or above this source's version do not change its boundary.
  local_tags="$(source_git show-ref --tags --dereference | release_pairing_relevant_tags "$version" | LC_ALL=C sort)" || source_refused
  remote_tags="$(source_git ls-remote --tags origin | release_pairing_relevant_tags "$version" | LC_ALL=C sort)" || source_refused
  [[ -n "$local_tags" && "$local_tags" == "$remote_tags" ]] || source_refused
  base="$(release_previous_stable_tag "$version" "$directory")" || source_refused
  base_sha="$(source_git rev-parse --verify "$base^{commit}")" || source_refused
  [[ "$base_sha" =~ ^[0-9a-f]{40}$ ]] || source_refused
  count="$(source_git rev-list --count "$base_sha..$revision")" || source_refused
  [[ "$count" =~ ^[1-9][0-9]*$ ]] || source_refused
  text="$(source_git log --format='%B' "$base_sha..$revision")" || source_refused
  printf '%s' "$text" | jq -Rs --arg source "$revision" --arg version "$version" \
    --arg base "$base" --arg base_sha "$base_sha" --argjson count "$count" \
    '{schema:"harn.release.pairing_source.v1",source_sha:$source,version:$version,
      base_tag:$base,base_sha:$base_sha,commit_count:$count,text:.}'
)

if [[ "${BASH_SOURCE[0]}" == "$0" ]]; then
  release_pairing_source_json "$@"
fi
