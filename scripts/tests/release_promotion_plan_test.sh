#!/usr/bin/env bash
# Execute promote-release.yml's real plan step against fixture build runs. Only
# the run of a commit that changes the workspace version to a stable X.Y.Z is
# promoted; a rerun after the release exists is a no-op, and a tag at another
# commit is refused.
set -euo pipefail

root="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
workflow="$root/.github/workflows/promote-release.yml"
tmp="$(mktemp -d)"
trap 'rm -rf "$tmp"' EXIT

fail() {
  echo "FAIL: $*" >&2
  exit 1
}

awk '/        id: plan/{found=1} found && /        run: \|/{body=1;next} body && /^  [^ ]/{exit} body && /^      - /{exit} body{print substr($0,11)}' \
  "$workflow" > "$tmp/plan.sh"
grep -Fq 'release_push_is_stable_version_change' "$tmp/plan.sh" \
  || fail "could not extract the plan step from $workflow"
# The plan must make the same decision as the build that made the candidate.
# The plan sees only HEAD^; the build judges the whole push and refuses a push
# whose release commit is not its head, so every push the plan promotes is one
# where HEAD^ is the previous version.
grep -Fq 'release_range_release_commits "$PUSH_BASE" "$GITHUB_SHA"' \
  "$root/.github/workflows/build-release-binaries.yml" \
  || fail "build-release-binaries.yml no longer decides candidates with release_range_release_commits"

repo="$tmp/repo"
mkdir -p "$repo/scripts/lib" "$tmp/bin"
cp "$root/scripts/lib/release_version.sh" "$repo/scripts/lib/"
cp "$root/scripts/release_contract.env" "$repo/scripts/"
git -C "$repo" init -b main --quiet
git -C "$repo" config user.name "Release Promotion Test"
git -C "$repo" config user.email "release-promotion-test@example.com"
git -C "$repo" config commit.gpgsign false

# gh stub: FAKE_TAG_SHA is the commit the tag points at (unset: no tag), and
# FAKE_RELEASE=1 means the release exists.
cat > "$tmp/bin/gh" <<'EOF'
#!/usr/bin/env bash
case "$1" in
  api) [[ -n "${FAKE_TAG_SHA:-}" ]] || exit 1; printf '%s\n' "$FAKE_TAG_SHA" ;;
  release) [[ "${FAKE_RELEASE:-0}" == 1 ]] ;;
  *) exit 2 ;;
esac
EOF
chmod +x "$tmp/bin/gh"

commit_version() {
  printf '[workspace]\nmembers = []\n\n[workspace.package]\nversion = "%s"\n' "$1" > "$repo/Cargo.toml"
  git -C "$repo" add -A
  git -C "$repo" commit --quiet --allow-empty -m "$2"
}

# plan <name> [env...]; the plan's exit status is saved in <name>.status.
plan() {
  local name=$1
  shift
  : > "$tmp/$name.outputs"
  local status=0
  (
    cd "$repo"
    env PATH="$tmp/bin:$PATH" GITHUB_REPOSITORY=burin-labs/harn \
      HEAD_SHA="$(git rev-parse HEAD)" BUILD_RUN_ID=9001 GITHUB_OUTPUT="$tmp/$name.outputs" "$@" \
      bash -eu "$tmp/plan.sh" > "$tmp/$name.log" 2>&1
  ) || status=$?
  echo "$status" > "$tmp/$name.status"
}

output() {
  sed -n "s/^$2=//p" "$tmp/$1.outputs"
}

commit_version 0.10.142-dev "Start 0.10.142 development"
commit_version 0.10.142-dev "Change something"
plan warm
[[ "$(cat "$tmp/warm.status")" == 0 && "$(output warm promote)" == false ]] \
  || fail "a development push was promoted: $(cat "$tmp/warm.log")"

commit_version 0.10.142 "Bump the workspace version"
head_sha="$(git -C "$repo" rev-parse HEAD)"
plan candidate
[[ "$(cat "$tmp/candidate.status")" == 0 && "$(output candidate promote)" == true ]] \
  || fail "the version commit was not promoted: $(cat "$tmp/candidate.log")"
[[ "$(output candidate tag)" == v0.10.142 && "$(output candidate version)" == 0.10.142 &&
   "$(output candidate major_minor)" == 0.10 ]] \
  || fail "wrong release identity: $(cat "$tmp/candidate.outputs")"

# A tag at this commit with no release yet is a promotion that stopped before
# publishing; it is promoted again.
plan resume "FAKE_TAG_SHA=$head_sha"
[[ "$(output resume promote)" == true ]] || fail "a tag without a release was not resumed"

# Rerun after the release exists: no-op.
plan rerun "FAKE_TAG_SHA=$head_sha" FAKE_RELEASE=1
[[ "$(cat "$tmp/rerun.status")" == 0 && "$(output rerun promote)" == false ]] \
  || fail "an existing release was promoted again: $(cat "$tmp/rerun.log")"

# Negative control: the tag already names another commit.
plan conflict FAKE_TAG_SHA=1111111111111111111111111111111111111111
[[ "$(cat "$tmp/conflict.status")" != 0 ]] || fail "a tag at another commit was not refused"
grep -Fq "not the candidate commit $head_sha" "$tmp/conflict.log" \
  || fail "the tag conflict does not name the candidate: $(cat "$tmp/conflict.log")"

# Negative controls: a Release subject without a version change, and a
# prerelease version change, are not promoted.
commit_version 0.10.142 "Release v0.10.142"
plan release_subject
[[ "$(output release_subject promote)" == false ]] || fail "a Release subject was promoted"
commit_version 0.10.143-rc.1 "Prerelease"
plan prerelease
[[ "$(output prerelease promote)" == false ]] || fail "a prerelease was promoted"

echo "release_promotion_plan_test: ok"
