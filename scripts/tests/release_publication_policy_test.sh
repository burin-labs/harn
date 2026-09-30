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

vscode_workflow="$root/.github/workflows/publish-vscode.yml"
grep -Fq "if: github.ref_type != 'tag' || !contains(github.ref_name, '-')" "$vscode_workflow" \
  || fail "prerelease tag would reach the stable-only VS Code version projection"

publish_workflow="$root/.github/workflows/publish-release.yml"
grep -Fq "grep -E '^v[0-9]+\\.[0-9]+\\.[0-9]+$'" "$publish_workflow" \
  || fail "stable drift comparison can select a prerelease tag"
grep -Fq 'release_development_target_matches_stable "$CARGO_VERSION" "$LATEST_VERSION"' \
  "$publish_workflow" \
  || fail "declared -dev workspace state does not bypass publication"
grep -Fq 'release_development_target_precedes_stable "$CARGO_VERSION" "$LATEST_VERSION"' \
  "$publish_workflow" \
  || fail "certified at-SHA tag turns its pending main state red"
grep -Fq 'scripts/verify_release_tag_main_ancestry.sh --tag "$LATEST_TAG"' \
  "$publish_workflow" \
  || fail "matching version strings bypass trusted candidate tag verification"
if grep -Fq 'permission-contents: write' "$publish_workflow"; then
  fail "read-only crate publication retains a contents-write credential"
fi
if grep -Eq 'bump-fleet\.yml|permission-actions: write' "$publish_workflow"; then
  fail "crate publication can bypass the hosted release owner's convergence decision"
fi
development_opener="$root/scripts/open_development_bump.sh"
grep -Fq 'echo "harn_bin=$harn_bin"' "$development_opener" \
  || fail "post-release workflow does not retain its pre-mutation Harn binary proof"
# Both automated branch openers publish through the one signed-commit script.
release_opener="$root/scripts/open_release_pr.sh"
for opener in "$development_opener" "$release_opener"; do
  grep -Fq 'scripts/bump-driver/publish_branch_commit.harn' "$opener" \
    || fail "$(basename "$opener") bypasses signed GitHub publication"
  grep -Fq 'HARN_BRANCH_COMMIT_TOKEN="$GH_TOKEN"' "$opener" \
    || fail "$(basename "$opener") does not give signed publication the automation identity"
  if grep -Eq 'git (commit|push)' "$opener"; then
    fail "$(basename "$opener") can still create or push an unsigned local commit"
  fi
done
branch_publisher="$root/scripts/bump-driver/publish_branch_commit.harn"
grep -Fq 'import { github_bump_remote } from "./github_remote"' "$branch_publisher" \
  || fail "branch publication does not use the signed GitHub connector seam"
grep -Fq 'remote.publish_commit(' "$branch_publisher" \
  || fail "branch publication does not publish through the signed commit operation"
# One arming path for both automated release-lane pull requests.
grep -Fq 'gh pr merge "$pr_url" --auto --squash' "$root/scripts/lib/release_auto_merge.sh" \
  || fail "the shared release auto-merge helper does not arm a squash merge"
for armer in "$root/scripts/validate_development_bump.sh" "$release_opener"; do
  grep -Fq 'release_arm_auto_merge "$pr_url"' "$armer" \
    || fail "$(basename "$armer") does not arm through the shared release auto-merge helper"
  if grep -Fq 'gh pr merge' "$armer"; then
    fail "$(basename "$armer") arms auto-merge outside the shared helper"
  fi
done

ci_workflow="$root/.github/workflows/ci.yml"
grep -Fq 'release_published_version_for_workspace "$workspace_version"' "$ci_workflow" \
  || fail "source-only documentation checks try to install an unpublished -dev build"
grep -Fq 'scripts/verify_release_tag_main_ancestry.sh --tag "$REF_NAME"' "$publish_workflow" \
  || fail "crate publication does not prove the tag selects merged main"

echo "release publication policy tests passed"
