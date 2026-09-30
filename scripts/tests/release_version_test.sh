#!/usr/bin/env bash
set -euo pipefail

root="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
# shellcheck source=scripts/lib/release_version.sh
source "$root/scripts/lib/release_version.sh"

valid=(
  0.0.0
  1.2.3
  1.2.3-rc
  1.2.3-rc.0
  1.2.3-beta-preview.12
)
invalid=(
  01.2.3
  1.02.3
  1.2.03
  1.2
  1.2.3-
  1.2.3-rc..1
  1.2.3-rc.01
  1.2.3+build.1
)

for version in "${valid[@]}"; do
  release_version_is_canonical "$version" || {
    echo "release_version_test: rejected valid version $version" >&2
    exit 1
  }
done
for version in "${invalid[@]}"; do
  if release_version_is_canonical "$version"; then
    echo "release_version_test: accepted invalid version $version" >&2
    exit 1
  fi
done

release_version_is_prerelease 1.2.3-rc.0
if release_version_is_prerelease 1.2.3; then
  echo "release_version_test: stable version reported as prerelease" >&2
  exit 1
fi

[[ "$(release_next_patch_development 1.2.3)" == "1.2.4-dev" ]] || {
  echo "release_version_test: wrong next development version" >&2
  exit 1
}
release_development_target_matches_stable 1.2.4-dev 1.2.3 || {
  echo "release_version_test: matching development target rejected" >&2
  exit 1
}
release_development_target_precedes_stable 1.2.4-dev 1.2.4 || {
  echo "release_version_test: development identity behind its stable tag rejected" >&2
  exit 1
}
if release_development_target_precedes_stable 1.2.5-dev 1.2.4; then
  echo "release_version_test: next development identity reported behind stable tag" >&2
  exit 1
fi
if release_development_target_matches_stable 1.2.5-dev 1.2.3; then
  echo "release_version_test: stale or skipped development target accepted" >&2
  exit 1
fi
if release_next_patch_development 1.2.3-rc.1 >/dev/null; then
  echo "release_version_test: prerelease accepted as stable development base" >&2
  exit 1
fi
[[ "$(release_published_version_for_workspace 1.2.3)" == "1.2.3" ]]
[[ "$(release_published_version_for_workspace 1.2.4-dev)" == "1.2.3" ]]
if release_published_version_for_workspace 1.2.4-rc.1 >/dev/null; then
  echo "release_version_test: arbitrary prerelease projected as published" >&2
  exit 1
fi

tmp_repo="$(mktemp -d)"
trap 'rm -rf "$tmp_repo"' EXIT
git -C "$tmp_repo" init -b main --quiet
git -C "$tmp_repo" config user.name "Release Version Test"
git -C "$tmp_repo" config user.email "release-version-test@example.com"
git -C "$tmp_repo" config commit.gpgsign false
git -C "$tmp_repo" config tag.gpgSign false
printf '[workspace.package]\nversion = "1.2.3"\n' > "$tmp_repo/Cargo.toml"
git -C "$tmp_repo" add Cargo.toml
git -C "$tmp_repo" commit --quiet -m initial
printf '[workspace.package]\nversion = "1.2.4"\n' > "$tmp_repo/Cargo.toml"
git -C "$tmp_repo" add Cargo.toml
git -C "$tmp_repo" commit --quiet -m 'Release v1.2.4 (#42)'
(
  cd "$tmp_repo"
  release_head_is_release_commit_for_version 1.2.4
) || {
  echo "release_version_test: genuine release commit rejected" >&2
  exit 1
}
printf 'not a version change\n' > "$tmp_repo/README.md"
git -C "$tmp_repo" add README.md
git -C "$tmp_repo" commit --quiet -m 'Release v1.2.4 (#43)'
if (cd "$tmp_repo" && release_head_is_release_commit_for_version 1.2.4); then
  echo "release_version_test: title-only release commit accepted" >&2
  exit 1
fi
git -C "$tmp_repo" tag v1.2.4 HEAD~1
(
  cd "$tmp_repo"
  release_development_bump_plan 1.2.4 v1.2.4 true
  [[ "$RELEASE_DEVELOPMENT_BUMP_REQUIRED" == true ]]
  [[ "$RELEASE_DEVELOPMENT_BUMP_VERSION" == 1.2.5-dev ]]
  [[ "$RELEASE_DEVELOPMENT_BUMP_REASON" == published_stable_needs_development_identity ]]
) || {
  echo "release_version_test: later main commit disabled the published development bump" >&2
  exit 1
}

# A release PR lands through the repository's required squash method. The tag
# therefore names the certified candidate while main carries an equivalent
# one-parent fold, not the tag commit as an ancestor. That published release is
# still the stable identity main declares and must advance to the next -dev.
fold_repo="$tmp_repo/squash-fold"
git init --initial-branch=main --quiet "$fold_repo"
git -C "$fold_repo" config user.name "release-version-test"
git -C "$fold_repo" config user.email "release-version-test@example.com"
git -C "$fold_repo" config commit.gpgsign false
git -C "$fold_repo" config tag.gpgSign false
printf '[workspace.package]\nversion = "1.2.3"\n' > "$fold_repo/Cargo.toml"
git -C "$fold_repo" add Cargo.toml
git -C "$fold_repo" commit --quiet -m initial
fold_base="$(git -C "$fold_repo" rev-parse HEAD)"
git -C "$fold_repo" switch --quiet -c certified-candidate
printf '[workspace.package]\nversion = "1.2.4"\n' > "$fold_repo/Cargo.toml"
printf 'certified release note\n' > "$fold_repo/CHANGELOG.md"
git -C "$fold_repo" add Cargo.toml CHANGELOG.md
git -C "$fold_repo" commit --quiet -m 'Release v1.2.4'
git -C "$fold_repo" tag v1.2.4
git -C "$fold_repo" diff "$fold_base"..v1.2.4 > "$tmp_repo/release.patch"
git -C "$fold_repo" switch --quiet main
printf 'unrelated main work\n' > "$fold_repo/README.md"
git -C "$fold_repo" add README.md
git -C "$fold_repo" commit --quiet -m 'Unrelated main work'
git -C "$fold_repo" apply "$tmp_repo/release.patch"
git -C "$fold_repo" add Cargo.toml CHANGELOG.md
git -C "$fold_repo" commit --quiet -m 'Release v1.2.4 (#42)'
(
  cd "$fold_repo"
  release_development_bump_plan 1.2.4 v1.2.4 true
  [[ "$RELEASE_DEVELOPMENT_BUMP_REQUIRED" == true ]]
  [[ "$RELEASE_DEVELOPMENT_BUMP_VERSION" == 1.2.5-dev ]]
  [[ "$RELEASE_DEVELOPMENT_BUMP_REASON" == published_stable_needs_development_identity ]]
) || {
  echo "release_version_test: squash-folded published release did not advance development" >&2
  exit 1
}
(
  cd "$tmp_repo"
  release_development_bump_plan 1.2.5-dev v1.2.4 true
  [[ "$RELEASE_DEVELOPMENT_BUMP_REQUIRED" == false ]]
  [[ "$RELEASE_DEVELOPMENT_BUMP_REASON" == workspace_does_not_match_latest_stable ]]
) || {
  echo "release_version_test: development workspace produced another bump" >&2
  exit 1
}

# An immutable candidate tag is not publication. The at-SHA recovery shape
# leaves main on the same patch's -dev identity and the certified tag on a
# separate history line. It must stay inert until the GitHub Release exists.
(
  cd "$tmp_repo"
  release_development_bump_plan 1.2.4-dev v1.2.4 false
  [[ "$RELEASE_DEVELOPMENT_BUMP_REQUIRED" == false ]]
  [[ "$RELEASE_DEVELOPMENT_BUMP_REASON" == latest_stable_release_not_published ]]
) || {
  echo "release_version_test: candidate tag alone triggered a development bump" >&2
  exit 1
}
(
  cd "$tmp_repo"
  release_development_bump_plan 1.2.4-dev v1.2.4 true
  [[ "$RELEASE_DEVELOPMENT_BUMP_REQUIRED" == true ]]
  [[ "$RELEASE_DEVELOPMENT_BUMP_VERSION" == 1.2.5-dev ]]
  [[ "$RELEASE_DEVELOPMENT_BUMP_REASON" == published_candidate_supersedes_development_identity ]]
) || {
  echo "release_version_test: published at-SHA candidate did not advance development" >&2
  exit 1
}
git -C "$tmp_repo" switch --orphan orphan --quiet
rm -f "$tmp_repo/Cargo.toml" "$tmp_repo/README.md"
printf '[workspace.package]\nversion = "1.2.4"\n' > "$tmp_repo/Cargo.toml"
git -C "$tmp_repo" add Cargo.toml
git -C "$tmp_repo" commit --quiet -m orphan
(
  cd "$tmp_repo"
  release_development_bump_plan 1.2.4 v1.2.4 true
  [[ "$RELEASE_DEVELOPMENT_BUMP_REQUIRED" == false ]]
  [[ "$RELEASE_DEVELOPMENT_BUMP_REASON" == latest_stable_tag_is_not_in_head_ancestry ]]
) || {
  echo "release_version_test: unrelated tag ancestry was accepted" >&2
  exit 1
}
release_tag_is_canonical v1.2.3-rc.0
if release_tag_is_canonical 1.2.3-rc.0; then
  echo "release_version_test: bare version reported as canonical tag" >&2
  exit 1
fi
release_branch_is_canonical release/v1.2.3-rc.0
if release_branch_is_canonical releases/v1.2.3-rc.0; then
  echo "release_version_test: malformed release branch accepted" >&2
  exit 1
fi

git -C "$tmp_repo" tag v1.3.0-rc.1
git -C "$tmp_repo" tag not-a-release
if [[ "$(cd "$tmp_repo" && release_latest_stable_tag)" != "v1.2.4" ]]; then
  echo "release_version_test: latest stable tag selection accepted a prerelease or invalid tag" >&2
  exit 1
fi

# The candidate trigger: a push is a release exactly when the workspace version
# changes to a stable X.Y.Z.
workspace_manifest=$'[workspace]\nmembers = []\n\n[workspace.package]\nversion = "0.10.142"\n\n[workspace.dependencies]\nserde = { version = "1" }\n'
if [[ "$(release_workspace_version <<<"$workspace_manifest")" != "0.10.142" ]]; then
  echo "release_version_test: workspace version was not read from [workspace.package]" >&2
  exit 1
fi
if [[ -n "$(release_workspace_version <<<$'[workspace]\nmembers = []\n')" ]]; then
  echo "release_version_test: a manifest with no version reported one" >&2
  exit 1
fi
release_push_is_stable_version_change 0.10.142-dev 0.10.142
release_push_is_stable_version_change 0.10.141 0.10.142
release_push_is_stable_version_change "" 0.10.142
for pair in "0.10.142 0.10.142" "0.10.142 0.10.143-dev" "0.10.142-dev 0.10.143-rc.1" \
  "0.10.141 0.10" "0.10.141 "; do
  read -r previous current <<<"$pair"
  if release_push_is_stable_version_change "$previous" "${current:-}"; then
    echo "release_version_test: '$previous' -> '${current:-}' was treated as a release" >&2
    exit 1
  fi
done

echo "release version projection tests passed"
