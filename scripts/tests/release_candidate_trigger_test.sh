#!/usr/bin/env bash
# Execute build-release-binaries.yml's real setup resolver against fixture
# pushes to main. A push that changes the workspace version to a stable X.Y.Z
# builds the release candidate at exactly that commit; every other push keeps
# warm-cache behavior. The version decides, never the commit subject.
set -euo pipefail

root="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
workflow="$root/.github/workflows/build-release-binaries.yml"
tmp="$(mktemp -d)"
trap 'rm -rf "$tmp"' EXIT

fail() {
  echo "FAIL: $*" >&2
  exit 1
}

awk '/        id: resolve/{found=1} found && /        run: \|/{body=1;next} body && /^  [^ ]/{exit} body && /^      - /{exit} body{print substr($0,11)}' \
  "$workflow" > "$tmp/resolve.sh"
grep -Fq 'release_push_is_stable_version_change' "$tmp/resolve.sh" \
  || fail "could not extract the setup resolver from $workflow"

repo="$tmp/repo"
mkdir -p "$repo/scripts/lib" "$repo/.github"
cp "$root/scripts/lib/release_version.sh" "$repo/scripts/lib/"
cp "$root/scripts/release_contract.env" "$root/scripts/release_runner_matrix.sh" "$repo/scripts/"
cp "$root/.github/release-runner-policy.json" "$repo/.github/"
git -C "$repo" init --quiet
git -C "$repo" config user.name "Release Trigger Test"
git -C "$repo" config user.email "release-trigger-test@example.com"
git -C "$repo" config commit.gpgsign false

commit_version() {
  local version=$1
  local subject=$2
  printf '[workspace]\nmembers = []\n\n[workspace.package]\nversion = "%s"\n' "$version" \
    > "$repo/Cargo.toml"
  git -C "$repo" add -A
  git -C "$repo" commit --quiet --allow-empty -m "$subject"
}

# resolve <name> [RELEASE_BUILD_INPUTS_CHANGED]
resolve() {
  local name=$1
  local inputs_changed=${2:-false}
  : > "$tmp/$name.outputs"
  (
    cd "$repo"
    env \
      EVENT_NAME=push REF_TYPE=branch REF_NAME=main \
      GITHUB_SHA="$(git rev-parse HEAD)" \
      GITHUB_OUTPUT="$tmp/$name.outputs" GITHUB_STEP_SUMMARY="$tmp/$name.summary" \
      INPUT_WARM_CACHE_ONLY=false INPUT_TARGETS='' INPUT_BENCHMARK_ONLY=false \
      INPUT_BENCHMARK_SOURCE_REF='' INPUT_BENCHMARK_SOURCE_SHA='' \
      INPUT_BENCHMARK_CARGO_BLOAT=false INPUT_RUNNER_PROFILE=policy \
      HARN_RELEASE_ENABLE_BLACKSMITH_MACOS=false \
      RELEASE_BUILD_INPUTS_CHANGED="$inputs_changed" \
      bash -eu "$tmp/resolve.sh" > "$tmp/$name.log" 2>&1
  ) || fail "$name: resolver failed: $(cat "$tmp/$name.log")"
}

output() {
  sed -n "s/^$2=//p" "$tmp/$1.outputs"
}

matrix_targets() {
  awk '/^build_matrix<<__JSON__$/{json=1;next} /^__JSON__$/{json=0} json' "$tmp/$1.outputs" \
    | jq -r '[.[].target] | sort | join(",")'
}

commit_version 0.10.142-dev "Start 0.10.142 development"
commit_version 0.10.142-dev "Change something"

# A development-version push that changed no build input builds nothing.
resolve idle
[[ "$(output idle build_mode)" == warm && "$(output idle should_build_binaries)" == false ]] \
  || fail "an ordinary push built binaries: $(cat "$tmp/idle.outputs")"

# The same push with a build input changed is a warm, never a candidate.
resolve warm true
[[ "$(output warm build_mode)" == warm && "$(output warm should_build_binaries)" == true &&
   "$(output warm should_package_archives)" == false && -z "$(output warm candidate_source_sha)" ]] \
  || fail "a build-input push did not warm: $(cat "$tmp/warm.outputs")"

# The version commit: 0.10.142-dev -> 0.10.142. A subject that does not look
# like a release proves the decision reads the version.
commit_version 0.10.142 "Bump the workspace version"
head_sha="$(git -C "$repo" rev-parse HEAD)"
resolve candidate
[[ "$(output candidate build_mode)" == candidate ]] \
  || fail "the version commit did not build a candidate: $(cat "$tmp/candidate.outputs")"
[[ "$(output candidate candidate_source_sha)" == "$head_sha" && "$(output candidate ref)" == "$head_sha" ]] \
  || fail "the candidate is not pinned to the pushed commit"
[[ "$(output candidate should_build_binaries)" == true && "$(output candidate should_package_archives)" == true ]] \
  || fail "the candidate does not sign and package"
[[ "$(output candidate version)" == 0.10.142 ]] || fail "candidate version is $(output candidate version)"
[[ "$(matrix_targets candidate)" == "aarch64-apple-darwin,aarch64-unknown-linux-gnu,x86_64-apple-darwin,x86_64-pc-windows-msvc,x86_64-unknown-linux-gnu" ]] \
  || fail "the candidate does not build all five targets: $(matrix_targets candidate)"

# Negative control: a commit whose subject says Release but whose version did
# not change is not a candidate.
commit_version 0.10.142 "Release v0.10.142"
resolve release_subject
[[ "$(output release_subject build_mode)" == warm ]] \
  || fail "a Release subject without a version change built a candidate"

# Negative control: a prerelease version change is not a release candidate.
commit_version 0.10.143-rc.1 "Prerelease"
resolve prerelease
[[ "$(output prerelease build_mode)" == warm ]] \
  || fail "a prerelease version change built a candidate"

echo "release_candidate_trigger_test: ok"
