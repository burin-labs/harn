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
