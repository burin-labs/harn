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
grep -Fq 'release_range_release_commits' "$tmp/resolve.sh" \
  || fail "could not extract the setup resolver from $workflow"

repo="$tmp/repo"
mkdir -p "$repo/scripts/lib" "$repo/.github"
cp "$root/scripts/lib/release_version.sh" "$root/scripts/lib/release_candidate_run.sh" "$repo/scripts/lib/"
cp "$root/scripts/release_contract.env" "$root/scripts/release_runner_matrix.sh" "$repo/scripts/"
cp "$root/.github/release-runner-policy.json" "$repo/.github/"
git -C "$repo" init -b main --quiet
git -C "$repo" config user.name "Release Trigger Test"
git -C "$repo" config user.email "release-trigger-test@example.com"
git -C "$repo" config commit.gpgsign false

# gh stub for the candidate-run lookup: FAKE_QUEUE_RUN is the run that built a
# candidate at the pushed commit (unset: none), and FAKE_GH_FAIL=1 makes GitHub
# unreadable.
mkdir -p "$tmp/bin"
cat > "$tmp/bin/gh" <<'EOF'
#!/usr/bin/env bash
[[ "${FAKE_GH_FAIL:-0}" == 1 ]] && exit 1
case "$2" in
  */build-release-binaries.yml/runs\?*) [[ -z "${FAKE_QUEUE_RUN:-}" ]] || printf '%s\n' "$FAKE_QUEUE_RUN" ;;
  */artifacts\?name=candidate-manifest-*) printf '1\n' ;;
  *) exit 2 ;;
esac
EOF
chmod +x "$tmp/bin/gh"

commit_version() {
  local version=$1
  local subject=$2
  printf '[workspace]\nmembers = []\n\n[workspace.package]\nversion = "%s"\n' "$version" \
    > "$repo/Cargo.toml"
  git -C "$repo" add -A
  git -C "$repo" commit --quiet --allow-empty -m "$subject"
}

# run_resolver <name> [VAR=value...]: a push to main unless the overrides say
# otherwise.
run_resolver() {
  local name=$1
  shift
  : > "$tmp/$name.outputs"
  (
    cd "$repo"
    env \
      PATH="$tmp/bin:$PATH" GITHUB_REPOSITORY=burin-labs/harn \
      EVENT_NAME=push REF_TYPE=branch REF_NAME=main \
      GITHUB_SHA="$(git rev-parse HEAD)" PUSH_BEFORE='' MERGE_GROUP_BASE='' \
      GITHUB_OUTPUT="$tmp/$name.outputs" GITHUB_STEP_SUMMARY="$tmp/$name.summary" \
      INPUT_WARM_CACHE_ONLY=false INPUT_TARGETS='' INPUT_BENCHMARK_ONLY=false \
      INPUT_BENCHMARK_SOURCE_REF='' INPUT_BENCHMARK_SOURCE_SHA='' \
      INPUT_BENCHMARK_CARGO_BLOAT=false INPUT_RUNNER_PROFILE=policy \
      HARN_RELEASE_ENABLE_BLACKSMITH_MACOS=false \
      RELEASE_BUILD_INPUTS_CHANGED=false "$@" \
      bash -eu "$tmp/resolve.sh" > "$tmp/$name.log" 2>&1
  )
}

resolve() {
  run_resolver "$@" || fail "$1: resolver failed: $(cat "$tmp/$1.log")"
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
resolve warm RELEASE_BUILD_INPUTS_CHANGED=true
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

# A merge queue lands several entries in one push. A release commit at the
# head of a batched push is the candidate, judged from the previous main.
push_base="$(git -C "$repo" rev-parse HEAD)"
commit_version 0.10.143-rc.1 "Queued ahead of the release"
commit_version 0.10.143 "Version commit"
head_sha="$(git -C "$repo" rev-parse HEAD)"
resolve batched_head PUSH_BEFORE="$push_base"
[[ "$(output batched_head build_mode)" == candidate && "$(output batched_head candidate_source_sha)" == "$head_sha" ]] \
  || fail "a release at the head of a batched push was not the candidate: $(cat "$tmp/batched_head.outputs")"

# A release commit buried under a later entry is refused loudly. Judged by
# HEAD^ alone it looked like an unchanged version and ran as a green warm build.
push_base="$(git -C "$repo" rev-parse HEAD)"
commit_version 0.10.144 "Version commit"
buried_sha="$(git -C "$repo" rev-parse HEAD)"
commit_version 0.10.144 "Queued behind the release"
if run_resolver buried PUSH_BEFORE="$push_base"; then
  fail "a release commit buried under a later entry was not refused: $(cat "$tmp/buried.outputs")"
fi
grep -Fq "release commit(s) $buried_sha below its head" "$tmp/buried.log" \
  || fail "the refusal does not name the buried release commit: $(cat "$tmp/buried.log")"

# The merge-group guard in ci.yml keeps a release last in its group: the entry
# behind the buried release fails, and the release entry itself passes.
awk '/- name: Keep a release commit last in its merge group/{found=1} found && /        run: \|/{body=1;next} body && /^      - /{exit} body{print substr($0,11)}' \
  "$root/.github/workflows/ci.yml" > "$tmp/guard.sh"
grep -Fq 'release_range_release_commits' "$tmp/guard.sh" || fail "could not extract the merge-group guard from ci.yml"
guard() {
  (cd "$repo" && BASE_SHA="$1" HEAD_SHA="$2" bash -eu "$tmp/guard.sh") > "$tmp/guard.log" 2>&1
}
if guard "$push_base" "$(git -C "$repo" rev-parse HEAD)"; then
  fail "the merge-group guard admitted an entry queued behind a release"
fi
guard "$push_base" "$buried_sha" || fail "the merge-group guard refused the release entry: $(cat "$tmp/guard.log")"

# The release's merge group builds the candidate at its own commit, which is
# the commit that lands on main; a group without a release at its head builds
# nothing.
git -C "$repo" checkout --quiet -b queue "$push_base"
commit_version 0.10.144-dev "Queued entry"
resolve queue_idle EVENT_NAME=merge_group REF_NAME=gh-readonly-queue/main/pr-1 MERGE_GROUP_BASE="$push_base"
[[ "$(output queue_idle build_mode)" == none && "$(output queue_idle should_build_binaries)" == false ]] \
  || fail "a merge group without a release built something: $(cat "$tmp/queue_idle.outputs")"
commit_version 0.10.144 "Version commit"
queue_sha="$(git -C "$repo" rev-parse HEAD)"
resolve queue_release EVENT_NAME=merge_group REF_NAME=gh-readonly-queue/main/pr-2 MERGE_GROUP_BASE="$push_base"
[[ "$(output queue_release build_mode)" == candidate && "$(output queue_release candidate_source_sha)" == "$queue_sha" &&
   "$(output queue_release should_package_archives)" == true ]] \
  || fail "the release's merge group did not build its candidate: $(cat "$tmp/queue_release.outputs")"

# When that commit reaches main, the push finds the queue's run and builds
# nothing; with the queue's run unreadable it builds rather than guess.
resolve pushed_after_queue PUSH_BEFORE="$push_base" FAKE_QUEUE_RUN=4242
[[ "$(output pushed_after_queue build_mode)" == queued && "$(output pushed_after_queue should_build_binaries)" == false &&
   "$(matrix_targets pushed_after_queue)" == "" ]] \
  || fail "the push rebuilt a candidate its merge group had built: $(cat "$tmp/pushed_after_queue.outputs")"
grep -Fq "Merge group run 4242" "$tmp/pushed_after_queue.log" || fail "the push does not name the queue's run"
resolve pushed_unread PUSH_BEFORE="$push_base" FAKE_GH_FAIL=1
[[ "$(output pushed_unread build_mode)" == candidate && "$(output pushed_unread candidate_source_sha)" == "$queue_sha" ]] \
  || fail "an unreadable queue run did not fall back to building: $(cat "$tmp/pushed_unread.outputs")"

# A pull request builds nothing, even the release PR itself; it only reports.
resolve pull_request EVENT_NAME=pull_request REF_NAME=8861/merge
[[ "$(output pull_request build_mode)" == none && "$(output pull_request should_build_binaries)" == false ]] \
  || fail "a pull request built something: $(cat "$tmp/pull_request.outputs")"

echo "release_candidate_trigger_test: ok"
