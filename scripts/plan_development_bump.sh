#!/usr/bin/env bash
# Prove publication before planning a normal or manually repaired cutover.
set -euo pipefail

script_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
root="${HARN_RELEASE_ROOT:-$script_root}"
cd "$root"
# shellcheck source=scripts/lib/release_version.sh
source "$script_root/scripts/lib/release_version.sh"

requested_tag="${PUBLISHED_TAG:-}"
release_args=()
if [[ -n "$requested_tag" ]]; then
  if ! release_tag_is_canonical "$requested_tag" \
    || release_version_is_prerelease "${requested_tag#v}"; then
    echo "error: PUBLISHED_TAG must name a canonical stable release" >&2
    exit 1
  fi
  release_args+=("$requested_tag")
fi
if ! publication="$(gh release view "${release_args[@]}" \
  --repo "${GITHUB_REPOSITORY:?GITHUB_REPOSITORY is required}" \
  --json tagName,isDraft,isPrerelease,publishedAt)"; then
  echo "error: could not read the published release; refusing an unproved development cutover" >&2
  exit 1
fi
if ! published_tag="$(jq -er --arg requested "$requested_tag" '
  select(.isDraft == false and .isPrerelease == false)
  | select(.publishedAt | type == "string" and length > 0)
  | .tagName | select(type == "string" and length > 0)
  | select($requested == "" or . == $requested)
' <<< "$publication")" \
  || ! release_tag_is_canonical "$published_tag" \
  || release_version_is_prerelease "${published_tag#v}"; then
  echo "error: release metadata does not prove the requested stable publication" >&2
  exit 1
fi
version="$(release_workspace_version < Cargo.toml)"
if ! release_version_is_canonical "$version"; then
  echo "error: could not read a canonical workspace version; refusing an unproved development cutover" >&2
  exit 1
fi
release_development_bump_plan "$version" "$published_tag" true
if [[ -n "${GITHUB_OUTPUT:-}" ]]; then
  {
    echo "required=$RELEASE_DEVELOPMENT_BUMP_REQUIRED"
    echo "version=$RELEASE_DEVELOPMENT_BUMP_VERSION"
    echo "reason=$RELEASE_DEVELOPMENT_BUMP_REASON"
    echo "published_tag=$published_tag"
  } >> "$GITHUB_OUTPUT"
fi
echo "::notice::Post-publication development bump: required=$RELEASE_DEVELOPMENT_BUMP_REQUIRED reason=$RELEASE_DEVELOPMENT_BUMP_REASON version=${RELEASE_DEVELOPMENT_BUMP_VERSION:-<none>} published_tag=$published_tag"
