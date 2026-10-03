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
cp "$root/scripts/release_contract.harn" "$root/scripts/path_visibility.harn" "$repo/scripts/"
cp "$root/.github/release-runner-policy.json" "$repo/.github/"
git -C "$repo" init -b main --quiet
git -C "$repo" config user.name "Release Trigger Test"
git -C "$repo" config user.email "release-trigger-test@example.com"
git -C "$repo" config commit.gpgsign false

mkdir -p "$tmp/bin"
# The source-candidate resolver asks Harn for its decision, as the workflow
# does after installing the bootstrap interpreter. No Harn is a named failure,
# never a skip.
[[ -n "${HARN_BIN:-}" && -x "${HARN_BIN}" ]] \
  || fail "HARN_BIN must name an executable harn; run through make test-pr-gate-post-warm-integrations"
ln -s "$HARN_BIN" "$tmp/bin/harn"
# gh stub for the candidate-run lookup: FAKE_QUEUE_RUN is the run that built a
# candidate at the pushed commit (unset: none), and FAKE_GH_FAIL=1 makes GitHub
# unreadable.
cat > "$tmp/bin/gh" <<'EOF'
#!/usr/bin/env bash
[[ "${FAKE_GH_FAIL:-0}" == 1 ]] && exit 1
case "$2" in
  */build-release-binaries.yml/runs\?*)
    if [[ -n "${FAKE_QUEUE_SEQUENCE:-}" ]]; then
      state_file="${FAKE_GH_STATE_FILE:?FAKE_GH_STATE_FILE required with FAKE_QUEUE_SEQUENCE}"
      read_count=0
      [[ ! -f "$state_file" ]] || read -r read_count < "$state_file"
      read_count=$((read_count + 1))
      printf '%s\n' "$read_count" > "$state_file"
      IFS=',' read -r -a states <<< "$FAKE_QUEUE_SEQUENCE"
      index=$((read_count - 1))
      (( index < ${#states[@]} )) || index=$((${#states[@]} - 1))
      state="${states[$index]}"
      if [[ "$*" == *"--jq"* ]]; then
        [[ "$state" != success ]] || printf '%s\n' "${FAKE_QUEUE_RUN:-4242}"
      else
        conclusion=null
        [[ "$state" != success ]] || conclusion='"success"'
        [[ "$state" != failed ]] || conclusion='"failure"'
        printf '{"workflow_runs":[{"id":%s,"head_sha":"%s","event":"merge_group","status":"%s","conclusion":%s}]}\n' \
          "${FAKE_QUEUE_RUN:-4242}" "${GITHUB_SHA:?}" "$([[ "$state" == success || "$state" == failed ]] && echo completed || echo "$state")" "$conclusion"
      fi
    else
      if [[ "$*" == *"--jq"* ]]; then
        [[ -z "${FAKE_QUEUE_RUN:-}" ]] || printf '%s\n' "$FAKE_QUEUE_RUN"
      elif [[ -n "${FAKE_QUEUE_RUN:-}" ]]; then
        printf '{"workflow_runs":[{"id":%s,"head_sha":"%s","event":"merge_group","status":"completed","conclusion":"success"}]}\n' \
          "$FAKE_QUEUE_RUN" "${GITHUB_SHA:?}"
      else
        printf '{"workflow_runs":[]}\n'
      fi
    fi
    ;;
  */artifacts\?name=candidate-manifest-*) printf '%s\n' "${FAKE_CANDIDATE_MANIFEST_COUNT:-1}" ;;
  */artifacts\?per_page=100) printf '%s\n' "${FAKE_SOURCE_ARTIFACTS:?source artifact fixture required}" ;;
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

# An explicit main source build reuses the full candidate producer without
# declaring a release. Dispatches cannot enter the promotion trigger.
commit_version 0.10.143-dev "Start development"
resolve source EVENT_NAME=workflow_dispatch INPUT_SOURCE_CANDIDATE=true
[[ "$(output source build_mode)" == candidate &&
   "$(output source candidate_purpose)" == source &&
   "$(output source candidate_source_sha)" == "$(git -C "$repo" rev-parse HEAD)" &&
   "$(output source should_package_archives)" == true ]] \
  || fail "explicit source candidate does not reuse the signed archive producer"
[[ "$(matrix_targets source)" == "$(matrix_targets warm)" ]] \
  || fail "source candidate does not cover the existing full matrix"
[[ "$(output source source_candidate_decision | jq -er '.accepted and .reason == "accepted"')" == true ]] \
  || fail "actual source resolver did not emit its typed admission receipt"

# Scheduled candidates cover even main commits excluded by warm-cache paths.
resolve scheduled EVENT_NAME=schedule INPUT_SOURCE_CANDIDATE=true
[[ "$(output scheduled build_mode)" == candidate &&
   "$(output scheduled should_package_archives)" == true &&
   "$(output scheduled candidate_source_sha)" == "$(git -C "$repo" rev-parse HEAD)" ]] \
  || fail "schedule did not package the exact main commit"
source_artifacts="$(matrix_targets scheduled | jq -R --arg sha "$(git -C "$repo" rev-parse HEAD)" '
  (split(",") | map("harn-" + .)) + ["candidate-manifest-" + $sha, "harn-release-files"] |
  map({name:.,expired:false,size_in_bytes:100}) | {total_count:length,artifacts:.}')"
resolve scheduled_reuse EVENT_NAME=schedule INPUT_SOURCE_CANDIDATE=true \
  FAKE_QUEUE_RUN=4242 FAKE_SOURCE_ARTIFACTS="$source_artifacts"
[[ "$(output scheduled_reuse build_mode)" == queued &&
   "$(output scheduled_reuse should_build_binaries)" == false &&
   "$(output scheduled_reuse reused_source_run_id)" == 4242 ]] \
  || fail "a complete live candidate was not reused"
for missing_proof in expired empty missing partial; do
  case "$missing_proof" in
    expired) filter='.artifacts[0].expired = true' ;;
    empty) filter='.artifacts[0].size_in_bytes = 0' ;;
    missing) filter='.artifacts = .artifacts[1:] | .total_count = (.artifacts | length)' ;;
    partial) filter='.total_count += 1' ;;
  esac
  resolve "scheduled_$missing_proof" EVENT_NAME=schedule INPUT_SOURCE_CANDIDATE=true \
    FAKE_QUEUE_RUN=4242 FAKE_SOURCE_ARTIFACTS="$(jq "$filter" <<< "$source_artifacts")"
  [[ "$(output "scheduled_$missing_proof" should_build_binaries)" == true ]] \
    || fail "schedule reused $missing_proof candidate evidence"
done
if run_resolver scheduled_unread EVENT_NAME=schedule INPUT_SOURCE_CANDIDATE=true FAKE_GH_FAIL=1; then
  fail "unread candidate inventory passed scheduled admission"
fi
for invalid_source in branch tag conflict benchmark profile targets event source_ref source_sha bloat; do
  case "$invalid_source" in
    branch) invalid_args=(REF_NAME=topic) ;;
    tag) invalid_args=(REF_TYPE=tag) ;;
    conflict) invalid_args=(INPUT_WARM_CACHE_ONLY=true) ;;
    benchmark) invalid_args=(INPUT_BENCHMARK_ONLY=true) ;;
    profile) invalid_args=(INPUT_RUNNER_PROFILE=standard) ;;
    targets) invalid_args=(INPUT_TARGETS=aarch64-apple-darwin) ;;
    event) invalid_args=(EVENT_NAME=push) ;;
    source_ref) invalid_args=(INPUT_BENCHMARK_SOURCE_REF=topic) ;;
    source_sha) invalid_args=(INPUT_BENCHMARK_SOURCE_SHA=0123456789012345678901234567890123456789) ;;
    bloat) invalid_args=(INPUT_BENCHMARK_CARGO_BLOAT=true) ;;
  esac
  if run_resolver "source_$invalid_source" EVENT_NAME=workflow_dispatch \
       INPUT_SOURCE_CANDIDATE=true "${invalid_args[@]}"; then
    fail "source candidate accepted invalid $invalid_source context"
  fi
  [[ "$(jq -er '.accepted == false and .reason != "accepted" and .build_mode == "none"' \
      "$tmp/source_$invalid_source.outputs.source-decision.json")" == true ]] \
    || fail "invalid source $invalid_source omitted its typed refusal receipt"
done

# Execute promotion's real version decision at that development commit. An
# absent output cannot stand in for the explicit refusal to publish.
awk '/        id: plan/{found=1} found && /        run: \|/{body=1;next} body && /^  [^ ]/{exit} body && /^      - /{exit} body{print substr($0,11)}' \
  "$root/.github/workflows/promote-release.yml" > "$tmp/promote-source.sh"
grep -Fq 'release_push_is_stable_version_change' "$tmp/promote-source.sh" \
  || fail "could not extract the actual promotion decision"
(
  cd "$repo"
  PATH="$tmp/bin:$PATH" GITHUB_REPOSITORY=burin-labs/harn \
    HEAD_SHA="$(git rev-parse HEAD)" BUILD_RUN_ID=4242 \
    GITHUB_OUTPUT="$tmp/source-promotion.outputs" \
    bash -eu "$tmp/promote-source.sh" > "$tmp/source-promotion.log" 2>&1
)
[[ "$(sed -n 's/^promote=//p' "$tmp/source-promotion.outputs")" == false ]] \
  || fail "source development candidate did not explicitly refuse promotion"

# Restore the stable fixture before the unchanged-version negative control.
commit_version 0.10.142 "Restore stable fixture"
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

# A candidate that is still running when the push starts is waited on, then
# reused after its exact manifest becomes available. The old resolver rebuilt
# immediately because its success-only query made in-progress look absent.
resolve pushed_waits_for_queue \
  PUSH_BEFORE="$push_base" \
  GITHUB_RUN_ID=9999 \
  FAKE_QUEUE_RUN=4242 \
  FAKE_QUEUE_SEQUENCE=in_progress,success \
  FAKE_GH_STATE_FILE="$tmp/wait-state" \
  RELEASE_CANDIDATE_WAIT_ATTEMPTS=2 \
  RELEASE_CANDIDATE_POLL_SECONDS=0
[[ "$(output pushed_waits_for_queue build_mode)" == queued &&
   "$(output pushed_waits_for_queue should_build_binaries)" == false ]] \
  || fail "the push rebuilt while its exact queue candidate was finishing: $(cat "$tmp/pushed_waits_for_queue.outputs")"
[[ "$(cat "$tmp/wait-state")" == 2 ]] \
  || fail "the resolver did not re-read the in-progress candidate"
grep -Fq "candidate run 4242 is in_progress; waiting" "$tmp/pushed_waits_for_queue.log" \
  || fail "the resolver did not report the bounded wait"

# A terminal failed candidate, an exhausted wait, and an unread API each take
# the safe rebuild path and name why. None may collapse into reuse.
resolve pushed_after_failed_queue \
  PUSH_BEFORE="$push_base" \
  GITHUB_RUN_ID=9999 \
  FAKE_QUEUE_RUN=4243 \
  FAKE_QUEUE_SEQUENCE=failed \
  FAKE_GH_STATE_FILE="$tmp/failed-state" \
  RELEASE_CANDIDATE_WAIT_ATTEMPTS=1 \
  RELEASE_CANDIDATE_POLL_SECONDS=0
[[ "$(output pushed_after_failed_queue build_mode)" == candidate ]] \
  || fail "a failed queue candidate was reused"
grep -Fq "candidate run 4243 concluded failure" "$tmp/pushed_after_failed_queue.log" \
  || fail "the failed-candidate rebuild did not name its reason"

resolve pushed_after_manifestless_queue \
  PUSH_BEFORE="$push_base" \
  GITHUB_RUN_ID=9999 \
  FAKE_QUEUE_RUN=4246 \
  FAKE_QUEUE_SEQUENCE=success \
  FAKE_GH_STATE_FILE="$tmp/manifestless-state" \
  FAKE_CANDIDATE_MANIFEST_COUNT=0 \
  RELEASE_CANDIDATE_WAIT_ATTEMPTS=1 \
  RELEASE_CANDIDATE_POLL_SECONDS=0
[[ "$(output pushed_after_manifestless_queue build_mode)" == candidate ]] \
  || fail "a successful run without the exact candidate manifest was reused"
grep -Fq "run 4246 succeeded without candidate-manifest-$queue_sha" "$tmp/pushed_after_manifestless_queue.log" \
  || fail "the manifestless rebuild did not name its reason"

resolve pushed_after_queue_timeout \
  PUSH_BEFORE="$push_base" \
  GITHUB_RUN_ID=9999 \
  FAKE_QUEUE_RUN=4244 \
  FAKE_QUEUE_SEQUENCE=in_progress \
  FAKE_GH_STATE_FILE="$tmp/timeout-state" \
  RELEASE_CANDIDATE_WAIT_ATTEMPTS=1 \
  RELEASE_CANDIDATE_POLL_SECONDS=0
[[ "$(output pushed_after_queue_timeout build_mode)" == candidate ]] \
  || fail "a timed-out queue candidate was reused"
grep -Fq "Timed out waiting for exact-SHA merge-group candidate run 4244" "$tmp/pushed_after_queue_timeout.log" \
  || fail "the timed-out rebuild did not name its reason"

# The push workflow itself has the same SHA and is always in progress during
# setup. Even a synthetic successful row with its id must not be reused.
resolve pushed_excludes_itself \
  PUSH_BEFORE="$push_base" \
  GITHUB_RUN_ID=4245 \
  FAKE_QUEUE_RUN=4245 \
  FAKE_QUEUE_SEQUENCE=success \
  FAKE_GH_STATE_FILE="$tmp/self-state" \
  RELEASE_CANDIDATE_WAIT_ATTEMPTS=1 \
  RELEASE_CANDIDATE_POLL_SECONDS=0
[[ "$(output pushed_excludes_itself build_mode)" == candidate ]] \
  || fail "the push reused its own workflow run"

resolve pushed_unread PUSH_BEFORE="$push_base" FAKE_GH_FAIL=1
[[ "$(output pushed_unread build_mode)" == candidate && "$(output pushed_unread candidate_source_sha)" == "$queue_sha" ]] \
  || fail "an unreadable queue run did not fall back to building: $(cat "$tmp/pushed_unread.outputs")"

# A pull request builds nothing, even the release PR itself; it only reports.
resolve pull_request EVENT_NAME=pull_request REF_NAME=8861/merge
[[ "$(output pull_request build_mode)" == none && "$(output pull_request should_build_binaries)" == false ]] \
  || fail "a pull request built something: $(cat "$tmp/pull_request.outputs")"

echo "release_candidate_trigger_test: ok"
