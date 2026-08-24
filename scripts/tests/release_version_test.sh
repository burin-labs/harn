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
git -C "$tmp_repo" init --quiet
git -C "$tmp_repo" config user.name "Release Version Test"
git -C "$tmp_repo" config user.email "release-version-test@example.com"
git -C "$tmp_repo" config commit.gpgsign false
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

echo "release version projection tests passed"
