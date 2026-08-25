#!/usr/bin/env bash

# Shell projection of std/semver's release-version boundary for bootstrap and
# GitHub Actions code that cannot execute Harn yet. Keep the fixture matrix in
# scripts/tests/release_version_test.sh aligned with std/semver conformance.

release_version_is_canonical() {
  local version="${1:-}"
  if [[ ! "$version" =~ ^(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)(-([0-9A-Za-z.-]+))?$ ]]; then
    return 1
  fi
  local prerelease="${BASH_REMATCH[5]:-}"
  if [[ -z "$prerelease" ]]; then
    return 0
  fi
  local identifiers=()
  IFS=. read -r -a identifiers <<<"$prerelease"
  local identifier
  for identifier in "${identifiers[@]}"; do
    if [[ -z "$identifier" || ! "$identifier" =~ ^[0-9A-Za-z-]+$ ]]; then
      return 1
    fi
    if [[ "$identifier" =~ ^[0-9]+$ && ${#identifier} -gt 1 && "$identifier" == 0* ]]; then
      return 1
    fi
  done
}

release_version_is_prerelease() {
  release_version_is_canonical "${1:-}" && [[ "$1" == *-* ]]
}

release_next_patch_development() {
  local stable="${1:-}"
  if [[ ! "$stable" =~ ^(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)$ ]]; then
    return 1
  fi
  printf '%s.%s.%s-dev\n' \
    "${BASH_REMATCH[1]}" \
    "${BASH_REMATCH[2]}" \
    "$(( 10#${BASH_REMATCH[3]} + 1 ))"
}

release_development_target_matches_stable() {
  local development="${1:-}"
  local stable="${2:-}"
  local expected
  expected="$(release_next_patch_development "$stable")" || return 1
  [[ "$development" == "$expected" ]]
}

release_published_version_for_workspace() {
  local workspace="${1:-}"
  if [[ "$workspace" =~ ^(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)$ ]]; then
    printf '%s\n' "$workspace"
    return 0
  fi
  if [[ "$workspace" =~ ^(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)-dev$ ]] \
    && (( 10#${BASH_REMATCH[3]} > 0 )); then
    printf '%s.%s.%s\n' \
      "${BASH_REMATCH[1]}" \
      "${BASH_REMATCH[2]}" \
      "$(( 10#${BASH_REMATCH[3]} - 1 ))"
    return 0
  fi
  return 1
}

release_head_is_release_commit_for_version() {
  local version="${1:-}"
  release_version_is_canonical "$version" || return 1
  release_version_is_prerelease "$version" && return 1
  local subject
  subject="$(git log -1 --format='%s' HEAD)" || return 1
  [[ "$subject" =~ ^Release\ v"$version"([[:space:]].*)?$ ]] || return 1
  git show HEAD -- Cargo.toml \
    | grep -Eq "^\+version = \"$version\"$"
}

# Decide whether a published stable workspace needs the next development
# identity. This is release state, not branch-tip authorship: after the tag is
# public, unrelated commits may sit above the release commit without changing
# which stable version the workspace still declares.
#
# Results are returned in RELEASE_DEVELOPMENT_BUMP_* globals so workflow source
# and fixture tests consume one decision owner.
# shellcheck disable=SC2034 # public result globals are consumed by sourcing callers
release_development_bump_plan() {
  local workspace_version="${1:-}"
  local latest_tag="${2:-}"

  RELEASE_DEVELOPMENT_BUMP_REQUIRED=false
  RELEASE_DEVELOPMENT_BUMP_VERSION=""
  RELEASE_DEVELOPMENT_BUMP_REASON=""

  if [[ -z "$latest_tag" ]]; then
    RELEASE_DEVELOPMENT_BUMP_REASON="no_stable_release_tag"
    return 0
  fi
  if ! release_tag_is_canonical "$latest_tag" \
    || release_version_is_prerelease "${latest_tag#v}"; then
    RELEASE_DEVELOPMENT_BUMP_REASON="latest_tag_is_not_stable"
    return 0
  fi
  if [[ "$workspace_version" != "${latest_tag#v}" ]]; then
    RELEASE_DEVELOPMENT_BUMP_REASON="workspace_does_not_match_latest_stable"
    return 0
  fi
  if ! git merge-base --is-ancestor "$latest_tag" HEAD; then
    RELEASE_DEVELOPMENT_BUMP_REASON="latest_stable_tag_is_not_in_head_ancestry"
    return 0
  fi

  RELEASE_DEVELOPMENT_BUMP_VERSION="$(release_next_patch_development "$workspace_version")" \
    || return 1
  RELEASE_DEVELOPMENT_BUMP_REQUIRED=true
  RELEASE_DEVELOPMENT_BUMP_REASON="published_stable_needs_development_identity"
}

release_tag_is_canonical() {
  [[ "${1:-}" == v* ]] && release_version_is_canonical "${1#v}"
}

release_branch_is_canonical() {
  [[ "${1:-}" == release/v* ]] && release_version_is_canonical "${1#release/v}"
}

# Project a canonical release version into the public channel policy consumed by
# build-release-binaries.yml. Results are returned in RELEASE_* globals so the
# workflow and its fixture test share one policy owner.
# shellcheck disable=SC2034 # public result globals are consumed by sourcing callers
release_publication_plan() {
  local version="${1:-}"
  local make_latest="${2:-false}"

  release_version_is_canonical "$version" || return 1
  [[ "$make_latest" == "true" || "$make_latest" == "false" ]] || return 1

  RELEASE_IS_PRERELEASE=false
  if release_version_is_prerelease "$version"; then
    RELEASE_IS_PRERELEASE=true
    [[ "$make_latest" == "false" ]] || return 1
  fi

  RELEASE_MAKE_LATEST="$make_latest"
  RELEASE_CONTAINER_TAGS=("$version")
  if [[ "$make_latest" == "true" ]]; then
    RELEASE_CONTAINER_TAGS+=("${version%.*}" latest)
  fi
}
