#!/usr/bin/env bash
# Create missing assets only. Existing bytes are immutable, including metadata.
set -euo pipefail
script_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=scripts/lib/candidate_archive_contract.sh
source "$script_dir/lib/candidate_archive_contract.sh"
[[ $# == 7 ]] || { echo 'usage: publish_certified_release_assets.sh REPO TAG SOURCE ARTIFACTS NOTES PRERELEASE LATEST' >&2; exit 2; }
repo="$1" tag="$2" source_sha="$3" artifacts="$4" notes="$5" prerelease="$6" latest="$7"
scratch="$(mktemp -d)"
trap 'rm -rf "$scratch"' EXIT
files=("${EXPECTED_RELEASE_ARCHIVES[@]}" SHA256SUMS release-assets.json)
for file in "${files[@]}"; do
  [[ -f "$artifacts/$file" ]] || { echo "error: missing publication asset $file" >&2; exit 1; }
  jq -n --arg name "$file" --arg digest "sha256:$(sha256_file "$artifacts/$file")" '{name:$name,digest:$digest}'
done | jq -s . > "$scratch/expected.json"
tag_object="$(gh api "repos/$repo/git/ref/tags/$tag" --jq '.object | select(.type == "tag") | .sha')"
[[ "$tag_object" =~ ^[0-9a-f]{40}$ ]] || { echo 'error: expected annotated release tag' >&2; exit 1; }
[[ "$(gh api "repos/$repo/git/tags/$tag_object" --jq '.object | select(.type == "commit") | .sha')" == "$source_sha" ]] || { echo 'error: release tag moved from certified source' >&2; exit 1; }
inventory() {
  gh api --paginate --slurp "repos/$repo/releases?per_page=100" > "$scratch/releases.json"
  jq -e --arg tag "$tag" --slurpfile expected "$scratch/expected.json" '
    [.[][] | select(.tag_name == $tag)] as $r |
    ($r | length) <= 1 and
    all($r[].assets[]; . as $asset |
      ([$expected[0][] | select(.name == $asset.name and .digest == $asset.digest)] | length) == 1) and
    all($expected[0][]; . as $e | ([$r[].assets[] | select(.name == $e.name)] | length) <= 1)
  ' "$scratch/releases.json" >/dev/null || { echo 'error: existing release assets conflict with the exact seven-file publication' >&2; return 1; }
}
inventory
if [[ "$(jq --arg tag "$tag" '[.[][] | select(.tag_name == $tag)] | length' "$scratch/releases.json")" == 0 ]]; then
  gh release create "$tag" --repo "$repo" --verify-tag --title "${tag#v}" --prerelease --notes-file "$notes"
fi
for file in "${files[@]}"; do
  inventory
  if [[ "$(jq --arg tag "$tag" --arg name "$file" '[.[][] | select(.tag_name == $tag) | .assets[] | select(.name == $name)] | length' "$scratch/releases.json")" == 0 ]]; then
    # GitHub refuses a concurrent same-name upload. Never delete or clobber it.
    gh release upload "$tag" "$artifacts/$file" --repo "$repo"
  fi
done
inventory
jq -e --arg tag "$tag" '[.[][] | select(.tag_name == $tag) | .assets[]] | length == 7' "$scratch/releases.json" >/dev/null
[[ "$(gh api "repos/$repo/git/ref/tags/$tag" --jq '.object.sha')" == "$tag_object" ]] || { echo 'error: release tag changed during publication' >&2; exit 1; }
gh release edit "$tag" --repo "$repo" --notes-file "$notes" --prerelease="$prerelease" --latest="$latest"
echo 'Published exact seven verified assets without overwriting existing bytes'
