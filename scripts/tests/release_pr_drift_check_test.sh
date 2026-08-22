#!/usr/bin/env bash
# shellcheck disable=SC2016 # workflow shell expressions are intentional literals
set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
# shellcheck source=.github/scripts/release-pr-drift.sh
source "$repo_root/.github/scripts/release-pr-drift.sh"

fail() {
  echo "FAIL: $*" >&2
  exit 1
}

assert_version() {
  local ref="$1" expected="$2" actual
  actual=$(release_pr_version "$ref") || fail "release ref was rejected: $ref"
  [[ "$actual" == "$expected" ]] \
    || fail "release ref $ref resolved to $actual, expected $expected"
}

assert_not_release() {
  local ref="$1"
  if release_pr_version "$ref" >/dev/null; then
    fail "non-release ref was accepted: $ref"
  fi
}

assert_version release/v1.2.3 1.2.3
assert_version release/v1.2.3-rc.1 1.2.3-rc.1
assert_version release-attempt/v0.10.108/3cfcd38bf22ef4586671d403881e293b39e0de1d 0.10.108
assert_version release-attempt/v1.2.3-rc.1/0123456789abcdef0123456789abcdef01234567 1.2.3-rc.1
assert_not_release release-attempt/v1.2.3
assert_not_release release-attempt/v1.2.3/not-a-commit
assert_not_release feature/release-attempt/v1.2.3/0123456789abcdef0123456789abcdef01234567

target=$(release_pr_target_json \
  release-attempt/v0.10.108/3cfcd38bf22ef4586671d403881e293b39e0de1d \
  deadbeef \
  6880)
jq -e '
  . == {
    ref: "release-attempt/v0.10.108/3cfcd38bf22ef4586671d403881e293b39e0de1d",
    sha: "deadbeef",
    pr: "6880",
    version: "0.10.108"
  }
' <<< "$target" >/dev/null || fail "release target projection is incorrect"
if release_pr_target_json feature/not-a-release deadbeef 6880 >/dev/null; then
  fail "non-release ref produced a target"
fi

missing_body=$(printf '# Changelog\n\n## v1.2.2\n\n- Prior.\n' | extract_unreleased)
[[ -z "$missing_body" ]] || fail "absent Unreleased section did not project to empty"
direct_body=$(printf '# Changelog\n\n## Unreleased\n\n- Direct note.\n\n## v1.2.2\n' | extract_unreleased)
[[ "$direct_body" == '- Direct note.' ]] || fail "direct Unreleased note was not extracted"

workflow="$repo_root/.github/workflows/release-pr-drift-check.yml"
[[ $(grep -Fc 'source .github/scripts/release-pr-drift.sh' "$workflow") -eq 2 ]] \
  || fail "workflow does not share ref/extraction policy across both steps"
[[ $(grep -Fc 'release_pr_target_json' "$workflow") -eq 2 ]] \
  || fail "workflow event paths do not share the target projection"
grep -Fq 'ver=$(jq -r '\''.version'\'' <<< "$target")' "$workflow" \
  || fail "drift reporting does not consume the parsed release version"

tmp_root=$(mktemp -d)
trap 'rm -rf "$tmp_root"' EXIT

init_fixture() {
  local repo="$1"
  git -C "$repo" init -q
  git -C "$repo" config user.email test@example.com
  git -C "$repo" config user.name test
  git -C "$repo" config commit.gpgsign false
  git -C "$repo" switch -qc main
}

# Different fragment paths are mechanically merge-safe: release preparation
# deletes only the pin-visible fragment and a post-pin fragment remains for the
# next release.
fragment_repo="$tmp_root/fragments"
mkdir -p "$fragment_repo/changelog.d"
init_fixture "$fragment_repo"
printf '# Changelog\n\n## v0.9.0\n\n- Prior.\n' > "$fragment_repo/CHANGELOG.md"
printf -- '- Pin note.\n' > "$fragment_repo/changelog.d/pin.fixed.md"
git -C "$fragment_repo" add CHANGELOG.md changelog.d/pin.fixed.md
git -C "$fragment_repo" commit -qm base
git -C "$fragment_repo" switch -qc release
printf '# Changelog\n\n## v1.0.0\n\n- Pin note.\n\n## v0.9.0\n\n- Prior.\n' > "$fragment_repo/CHANGELOG.md"
git -C "$fragment_repo" rm -q changelog.d/pin.fixed.md
git -C "$fragment_repo" add CHANGELOG.md
git -C "$fragment_repo" commit -qm release
git -C "$fragment_repo" switch -q main
printf -- '- Post-pin note.\n' > "$fragment_repo/changelog.d/post-pin.fixed.md"
git -C "$fragment_repo" add changelog.d/post-pin.fixed.md
git -C "$fragment_repo" commit -qm post-pin
git -C "$fragment_repo" merge -q --no-edit release
[[ -f "$fragment_repo/changelog.d/post-pin.fixed.md" ]] \
  || fail "release merge consumed the post-pin fragment"
! grep -Fq 'Post-pin note.' "$fragment_repo/CHANGELOG.md" \
  || fail "release merge folded the post-pin fragment into the released section"

# Direct Unreleased notes remain supported and preserve the original hazard:
# Git's clean three-way merge absorbs a post-pin bullet into the renamed release
# section. This is why the drift gate must remain rather than be deleted.
direct_repo="$tmp_root/direct"
mkdir -p "$direct_repo"
init_fixture "$direct_repo"
printf '# Changelog\n\n## Unreleased\n\n### Fixed\n\n- Pin note.\n\n## v0.9.0\n\n- Prior.\n' > "$direct_repo/CHANGELOG.md"
git -C "$direct_repo" add CHANGELOG.md
git -C "$direct_repo" commit -qm base
git -C "$direct_repo" switch -qc release
printf '# Changelog\n\n## v1.0.0\n\n### Fixed\n\n- Pin note.\n\n## v0.9.0\n\n- Prior.\n' > "$direct_repo/CHANGELOG.md"
git -C "$direct_repo" add CHANGELOG.md
git -C "$direct_repo" commit -qm release
git -C "$direct_repo" switch -q main
printf '# Changelog\n\n## Unreleased\n\n### Fixed\n\n- Pin note.\n- Post-pin note.\n\n## v0.9.0\n\n- Prior.\n' > "$direct_repo/CHANGELOG.md"
git -C "$direct_repo" add CHANGELOG.md
git -C "$direct_repo" commit -qm post-pin
git -C "$direct_repo" merge -q --no-edit release \
  || fail "direct-note adversarial merge unexpectedly conflicted"
grep -Fq '## v1.0.0' "$direct_repo/CHANGELOG.md" \
  || fail "release heading did not survive the direct-note merge"
grep -Fq 'Post-pin note.' "$direct_repo/CHANGELOG.md" \
  || fail "fixture no longer demonstrates silent direct-note absorption"

echo 'release PR drift check tests passed'
