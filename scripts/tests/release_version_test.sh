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

[[ "$(release_next_patch_stable 1.2.3)" == "1.2.4" ]] || {
  echo "release_version_test: wrong next stable version" >&2
  exit 1
}
if release_next_patch_stable 1.2.3-rc.1 >/dev/null; then
  echo "release_version_test: prerelease accepted as stable base" >&2
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
git -C "$tmp_repo" commit --quiet -m 'Declare 1.2.4'
printf 'not a version change\n' > "$tmp_repo/README.md"
git -C "$tmp_repo" add README.md
git -C "$tmp_repo" commit --quiet -m 'Later main work'
git -C "$tmp_repo" tag v1.2.4 HEAD~1
(
  cd "$tmp_repo"
  release_development_bump_plan 1.2.4 v1.2.4 true
  [[ "$RELEASE_DEVELOPMENT_BUMP_REQUIRED" == true ]]
  [[ "$RELEASE_DEVELOPMENT_BUMP_VERSION" == 1.2.5 ]]
  [[ "$RELEASE_DEVELOPMENT_BUMP_REASON" == published_stable_needs_next_version ]]
) || {
  echo "release_version_test: later main commit disabled the published version bump" >&2
  exit 1
}

# A release PR lands through the repository's required squash method. The tag
# therefore names the certified candidate while main carries an equivalent
# one-parent fold, not the tag commit as an ancestor. That published release is
# still the stable identity main declares and must advance to the next version.
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
  [[ "$RELEASE_DEVELOPMENT_BUMP_VERSION" == 1.2.5 ]]
  [[ "$RELEASE_DEVELOPMENT_BUMP_REASON" == published_stable_needs_next_version ]]
) || {
  echo "release_version_test: squash-folded published release did not advance main" >&2
  exit 1
}
# Publishing an older commit leaves main already on the next version, or above it.
(
  cd "$tmp_repo"
  release_development_bump_plan 1.2.5 v1.2.4 true
  [[ "$RELEASE_DEVELOPMENT_BUMP_REQUIRED" == false ]]
  [[ "$RELEASE_DEVELOPMENT_BUMP_REASON" == next_version_already_declared ]]
  release_development_bump_plan 1.2.6 v1.2.4 true
  [[ "$RELEASE_DEVELOPMENT_BUMP_REQUIRED" == false ]]
  [[ "$RELEASE_DEVELOPMENT_BUMP_REASON" == workspace_does_not_match_latest_stable ]]
) || {
  echo "release_version_test: a declared next version produced another bump" >&2
  exit 1
}

# The one-time cutover: main on either -dev identity moves to the next stable,
# which needs no ancestry because the tag cannot be its parent.
for development in 1.2.4-dev 1.2.5-dev; do
  (
    cd "$tmp_repo"
    release_development_bump_plan "$development" v1.2.4 true
    [[ "$RELEASE_DEVELOPMENT_BUMP_REQUIRED" == true ]]
    [[ "$RELEASE_DEVELOPMENT_BUMP_VERSION" == 1.2.5 ]]
    [[ "$RELEASE_DEVELOPMENT_BUMP_REASON" == development_identity_becomes_next_version ]]
  ) || {
    echo "release_version_test: $development did not cut over to the next stable" >&2
    exit 1
  }
done

# A candidate tag alone is not publication.
(
  cd "$tmp_repo"
  release_development_bump_plan 1.2.4 v1.2.4 false
  [[ "$RELEASE_DEVELOPMENT_BUMP_REQUIRED" == false ]]
  [[ "$RELEASE_DEVELOPMENT_BUMP_REASON" == latest_stable_release_not_published ]]
) || {
  echo "release_version_test: candidate tag alone triggered a version bump" >&2
  exit 1
}

# Which versions publication admits. Only the GitHub transport is a stub.
stub_bin="$tmp_repo/stub-bin"
mkdir "$stub_bin"
cat > "$stub_bin/gh" <<'EOF'
#!/usr/bin/env bash
[[ "${FAKE_GH_FAIL:-0}" != 1 ]] || exit 1
case "$2" in
  */releases/latest) printf '%s\n' "$FAKE_LATEST" ;;
  *) [[ "$FAKE_PUBLISHED" == *" $CHECKED "* ]] && echo true || echo false ;;
esac
EOF
chmod +x "$stub_bin/gh"
promotable() {
  CHECKED="v$1" FAKE_LATEST=v1.2.4 FAKE_PUBLISHED=' v1.2.3 v1.2.4 v1.2.6 ' PATH="$stub_bin:$PATH" \
    release_require_promotable_version acme/widget "$1" 2>/dev/null
}
promotable 1.2.5 || { echo "release_version_test: next version refused" >&2; exit 1; }
promotable 1.3.0 || { echo "release_version_test: higher version refused" >&2; exit 1; }
for refused in 1.2.4 1.2.3 1.2.2 1.2.6 1.2.5-dev 1.2.5-rc.1 not-a-version; do
  if promotable "$refused"; then
    echo "release_version_test: $refused was admitted for publication" >&2
    exit 1
  fi
done
if CHECKED=v1.2.5 FAKE_GH_FAIL=1 PATH="$stub_bin:$PATH" \
  release_require_promotable_version acme/widget 1.2.5 2>/dev/null; then
  echo "release_version_test: unread publication state admitted a version" >&2
  exit 1
fi

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

workspace_manifest=$'[workspace]\nmembers = []\n\n[workspace.package]\nversion = "0.10.142"\n\n[workspace.dependencies]\nserde = { version = "1" }\n'
if [[ "$(release_workspace_version <<<"$workspace_manifest")" != "0.10.142" ]]; then
  echo "release_version_test: workspace version was not read from [workspace.package]" >&2
  exit 1
fi
if [[ -n "$(release_workspace_version <<<$'[workspace]\nmembers = []\n')" ]]; then
  echo "release_version_test: a manifest with no version reported one" >&2
  exit 1
fi
echo "release version projection tests passed"
