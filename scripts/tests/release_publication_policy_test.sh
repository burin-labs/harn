#!/usr/bin/env bash
# shellcheck disable=SC2016 # workflow expressions are intentionally literal fixtures
set -euo pipefail

root="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
# shellcheck source=scripts/lib/release_version.sh
source "$root/scripts/lib/release_version.sh"

fail() {
  echo "FAIL: $*" >&2
  exit 1
}

assert_plan() {
  local version="$1"
  local requested_latest="$2"
  local expected_prerelease="$3"
  local expected_latest="$4"
  shift 4

  release_publication_plan "$version" "$requested_latest" \
    || fail "publication plan rejected $version/$requested_latest"
  [[ "$RELEASE_IS_PRERELEASE" == "$expected_prerelease" ]] \
    || fail "$version prerelease=$RELEASE_IS_PRERELEASE, expected $expected_prerelease"
  [[ "$RELEASE_MAKE_LATEST" == "$expected_latest" ]] \
    || fail "$version make_latest=$RELEASE_MAKE_LATEST, expected $expected_latest"
  [[ "${RELEASE_CONTAINER_TAGS[*]}" == "$*" ]] \
    || fail "$version tags='${RELEASE_CONTAINER_TAGS[*]}', expected '$*'"
}

# Fixture matrix: the current stable cut owns rolling channels, historical
# stable recovery owns only its exact tag, and prereleases can own only their
# full version identity.
assert_plan 1.2.3 true false true 1.2.3 1.2 latest
assert_plan 1.2.2 false false false 1.2.2
assert_plan 1.3.0-rc.2 false true false 1.3.0-rc.2

if release_publication_plan 1.3.0-rc.2 true; then
  fail "prerelease was allowed to move latest channels"
fi
if release_publication_plan 1.3.0-rc.02 false; then
  fail "noncanonical prerelease reached publication policy"
fi

workflow="$root/.github/workflows/build-release-binaries.yml"
# GitHub expressions and shell variables below are intentionally matched as
# literal workflow source.
grep -Fq 'release_publication_plan "$VERSION" "$MAKE_LATEST"' "$workflow" \
  || fail "workflow bypasses the fixture-backed publication policy"
grep -Fq 'prerelease: ${{ needs.setup.outputs.is_prerelease }}' "$workflow" \
  || fail "final GitHub release does not preserve intentional prerelease state"
grep -Fq 'make_latest: ${{ needs.setup.outputs.make_latest }}' "$workflow" \
  || fail "final GitHub release does not consume the stable-only latest policy"
grep -Fq 'type=raw,value=${{ needs.setup.outputs.version }}' "$workflow" \
  || fail "exact container tag is not projected"
grep -Fq -- "--prerelease \\" "$workflow" \
  || fail "stable release placeholder is no longer created as prerelease"
grep -Fq 'scripts/verify_release_tag_main_ancestry.sh --tag "$REF"' "$workflow" \
  || fail "binary publication does not prove the tag selects merged main"
if grep -Eq 'promote_only|candidate_run_id|BUILD_MODE.?=.?promote' "$workflow"; then
  fail "binary publication retains obsolete candidate-promotion authority"
fi

vscode_workflow="$root/.github/workflows/publish-vscode.yml"
grep -Fq "if: github.ref_type != 'tag' || !contains(github.ref_name, '-')" "$vscode_workflow" \
  || fail "prerelease tag would reach the stable-only VS Code version projection"

publish_workflow="$root/.github/workflows/publish-release.yml"
grep -Fq "grep -E '^v[0-9]+\\.[0-9]+\\.[0-9]+$'" "$publish_workflow" \
  || fail "stable drift comparison can select a prerelease tag"
grep -Fq 'release_development_target_matches_stable "$CARGO_VERSION" "$LATEST_VERSION"' \
  "$publish_workflow" \
  || fail "declared -dev workspace state does not bypass publication"
grep -Fq 'release_head_is_release_commit_for_version "$version"' "$publish_workflow" \
  || fail "post-release bump is not bound to the version-changing release commit"
grep -Fq 'HARN_BIN="$harn_bin" ./scripts/prepare_development_version.sh' "$publish_workflow" \
  || fail "post-release workflow bypasses the tested development preparation seam"
grep -Fq 'gh pr merge "$pr_url" --auto --squash' "$publish_workflow" \
  || fail "post-release development bump does not enter the merge queue"

ci_workflow="$root/.github/workflows/ci.yml"
grep -Fq 'release_published_version_for_workspace "$workspace_version"' "$ci_workflow" \
  || fail "source-only documentation checks try to install an unpublished -dev build"
grep -Fq 'scripts/verify_release_tag_main_ancestry.sh --tag "$REF_NAME"' "$publish_workflow" \
  || fail "crate publication does not prove the tag selects merged main"

echo "release publication policy tests passed"
