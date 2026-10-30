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
mkdir -p "$repo/scripts/lib" "$repo/scripts/ci" "$repo/.github"
cp "$root/scripts/lib/release_version.sh" "$root/scripts/lib/release_candidate_run.sh" \
  "$root/scripts/lib/release_consumer_verdict.sh" "$root/scripts/lib/consumer_canary_policy.sh" \
  "$root/scripts/lib/candidate_archive_contract.sh" "$root/scripts/lib/sha256.sh" \
  "$root/scripts/lib/green_checkpoint.sh" "$repo/scripts/lib/"
cp "$root/scripts/lib/release-candidate-state.mjs" "$root/scripts/lib/consumer-observer.mjs" \
  "$root/scripts/lib/consumer-observation-budget.mjs" "$repo/scripts/lib/"
cp "$root/scripts/ci/consumer_observation_budget.json" "$repo/scripts/ci/"
cp "$root/scripts/ci/consumer_canary_policy.json" "$repo/scripts/ci/"
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
# gh stub for the scheduled source-candidate lookup: FAKE_SOURCE_RUN is a live
# run at the commit (unset: none), FAKE_SOURCE_ARTIFACTS its artifact
# inventory, and FAKE_GH_FAIL=1 makes GitHub unreadable.
cat > "$tmp/bin/gh" <<'STUB'
#!/usr/bin/env bash
[[ "${FAKE_GH_FAIL:-0}" == 1 ]] && exit 1
case "$2" in
  */releases\?per_page=50)
    [[ "${FAKE_RELEASES_FAIL:-0}" != 1 ]] || exit 1
    jq -n --argjson tags "${FAKE_RELEASES:-[]}" '[$tags[] | {tag_name: ., draft: false}]' | jq -r "$4" ;;
  */build-release-binaries.yml/runs\?*)
    sha="${2#*head_sha=}"; sha="${sha%%&*}"
    if [[ -n "${FAKE_SOURCE_RUN:-}" ]]; then
      jq -n --argjson id "$FAKE_SOURCE_RUN" --arg sha "$sha" '{total_count:1,workflow_runs:[{id:$id,run_attempt:1,event:"workflow_dispatch",head_branch:"main",head_sha:$sha,status:"completed",conclusion:"success"}]}'
    else echo '{"total_count":0,"workflow_runs":[]}'; fi ;;
  */artifacts\?per_page=100) printf '%s\n' "${FAKE_SOURCE_ARTIFACTS:?source artifact fixture required}" ;;
  *) exit 2 ;;
esac
STUB
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
      PR_HEAD_SHA="$(git rev-parse HEAD)" PR_BASE_SHA="$(git rev-parse HEAD^)" \
      GITHUB_OUTPUT="$tmp/$name.outputs" GITHUB_STEP_SUMMARY="$tmp/$name.summary" \
      INPUT_WARM_CACHE_ONLY=false INPUT_TARGETS='' INPUT_BENCHMARK_ONLY=false \
      INPUT_BENCHMARK_SOURCE_REF='' INPUT_BENCHMARK_SOURCE_SHA='' \
      INPUT_BENCHMARK_CARGO_BLOAT=false INPUT_RUNNER_PROFILE=policy \
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
[[ "$(output candidate rehearsal_source_sha)" == "$head_sha" ]] \
  || fail "release candidate did not request an exact-source consumer rehearsal"
resolve release_pull_request EVENT_NAME=pull_request REF_NAME=8861/merge
[[ "$(output release_pull_request rehearsal_source_sha)" == "$head_sha" &&
   "$(output release_pull_request should_build_binaries)" == false ]] \
  || fail "release PR must rehearse its exact head without building archives"
[[ "$(matrix_targets candidate)" == "aarch64-apple-darwin,aarch64-unknown-linux-gnu,x86_64-apple-darwin,x86_64-pc-windows-msvc,x86_64-unknown-linux-gnu" ]] \
  || fail "the candidate does not build all five targets: $(matrix_targets candidate)"

# A feature branch proposes no version change even when main has advanced to
# the next development version. Compare its fork point, not today's main.
commit_version 0.10.143-dev "Start development"
development_sha="$(git -C "$repo" rev-parse HEAD)"
git -C "$repo" checkout --quiet -b stale-feature "$head_sha"
printf 'ordinary feature change\n' > "$repo/feature.txt"
git -C "$repo" add feature.txt
git -C "$repo" commit --quiet -m "Change a feature without changing the version"
feature_sha="$(git -C "$repo" rev-parse HEAD)"
git -C "$repo" checkout --quiet main
resolve stale_feature_pull_request EVENT_NAME=pull_request REF_NAME=8862/merge \
  PR_BASE_SHA="$development_sha" PR_HEAD_SHA="$feature_sha"
[[ -z "$(output stale_feature_pull_request rehearsal_source_sha)" &&
   "$(output stale_feature_pull_request should_build_binaries)" == false ]] \
  || fail "a stale feature branch requested a release rehearsal"

if run_resolver unreadable_pull_request_history EVENT_NAME=pull_request REF_NAME=8863/merge \
  PR_BASE_SHA=0000000000000000000000000000000000000001 PR_HEAD_SHA="$feature_sha"; then
  fail "unreadable pull-request comparison history passed as no release"
fi

# An explicit main source build reuses the full candidate producer without
# declaring a release. Dispatches cannot enter the promotion trigger.
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
  FAKE_SOURCE_RUN=4242 FAKE_SOURCE_ARTIFACTS="$source_artifacts"
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
    FAKE_SOURCE_RUN=4242 FAKE_SOURCE_ARTIFACTS="$(jq "$filter" <<< "$source_artifacts")"
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
) || fail "promotion plan failed at the development commit: $(cat "$tmp/source-promotion.log")"
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

# While main owes unpublished v0.10.142, a source candidate of any later main
# commit is a release candidate with the full release proofs. Once published,
# the same request is an ordinary source candidate.
stable_sha="$(git -C "$repo" rev-parse HEAD)"
for event in schedule workflow_dispatch; do
  resolve "owed_$event" EVENT_NAME="$event" INPUT_SOURCE_CANDIDATE=true FAKE_RELEASES='["v0.10.141"]'
  [[ "$(output "owed_$event" build_mode)" == candidate &&
     "$(output "owed_$event" candidate_purpose)" == release &&
     "$(output "owed_$event" rehearsal_source_sha)" == "$stable_sha" ]] \
    || fail "an owed $event source candidate is not a release candidate: $(cat "$tmp/owed_$event.outputs")"
done
resolve published_source EVENT_NAME=schedule INPUT_SOURCE_CANDIDATE=true FAKE_RELEASES='["v0.10.142"]'
[[ "$(output published_source candidate_purpose)" == source &&
   -z "$(output published_source rehearsal_source_sha)" ]] \
  || fail "a source candidate of a published version claimed a release"
if run_resolver owed_unread EVENT_NAME=schedule INPUT_SOURCE_CANDIDATE=true FAKE_RELEASES_FAIL=1; then
  fail "an unread publication state admitted a candidate purpose"
fi

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
# behind the buried release fails, and the release entry itself passes. GitHub
# gives the entry behind the release the release commit as its group base, so
# the guard must judge from main, never from that base.
awk '/^  changes:/{admission=1;next} admission && /^  [a-zA-Z0-9_-]+:/{exit} admission && /- name: Keep a release commit last in its merge group/{found=1} found && /        run: \|/{body=1;next} body && /^      - /{exit} body{print substr($0,11)}' \
  "$root/.github/workflows/ci.yml" > "$tmp/guard.sh"
grep -Fq 'release_range_release_commits' "$tmp/guard.sh" || fail "the merge-group guard must run in changes before expensive CI admission"
guard() {
  (cd "$repo" && MAIN_REF="$1" HEAD_SHA="$2" BASE_SHA="$buried_sha" bash -eu "$tmp/guard.sh") > "$tmp/guard.log" 2>&1
}
if guard "$push_base" "$(git -C "$repo" rev-parse HEAD)"; then
  fail "the merge-group guard admitted an entry queued behind a release"
fi
grep -Fq "queued behind release commit(s) $buried_sha" "$tmp/guard.log" \
  || fail "the merge-group guard does not name the buried release: $(cat "$tmp/guard.log")"
guard "$push_base" "$buried_sha" || fail "the merge-group guard refused the release entry: $(cat "$tmp/guard.log")"
guard "$buried_sha" "$(git -C "$repo" rev-parse HEAD)" \
  || fail "the merge-group guard refused an entry after its release reached main: $(cat "$tmp/guard.log")"

# Failed admission leaves the tier output unmeasured. Execute the aggregate's
# actual first decision to prove that absence cannot become a green skip.
awk '/^  ci-status:/{aggregate=1} aggregate && /        run: \|/{body=1;next} body{print substr($0,11)} body && /          esac/{exit}' \
  "$root/.github/workflows/ci.yml" > "$tmp/admission-result.sh"
grep -Fq 'CI tier was not measured.' "$tmp/admission-result.sh" \
  || fail "could not extract the aggregate's admission-result decision"
for tier in '' unmeasured; do
  if POST_MERGE_TIER_ACTIVE="$tier" bash -eu "$tmp/admission-result.sh" > "$tmp/admission-result.log" 2>&1; then
    fail "the aggregate accepted an absent or invalid admission decision"
  fi
  grep -Fq 'CI tier was not measured.' "$tmp/admission-result.log" \
    || fail "the aggregate did not name missing admission: $(cat "$tmp/admission-result.log")"
done
for tier in true false; do
  POST_MERGE_TIER_ACTIVE="$tier" bash -eu "$tmp/admission-result.sh" \
    || fail "the aggregate refused a measured admission decision"
done

# A merge group never builds a candidate: the trusted macOS smoke host admits
# only refs/heads/main. A group whose head is a release commit rehearses the
# consumer at that exact commit under the release purpose, so the required
# verdict cannot pass it without that rehearsal.
git -C "$repo" checkout --quiet -b queue "$push_base"
commit_version 0.10.144-dev "Queued entry"
resolve queue_idle EVENT_NAME=merge_group REF_NAME=gh-readonly-queue/main/pr-1 MERGE_GROUP_BASE="$push_base"
[[ "$(output queue_idle build_mode)" == none && "$(output queue_idle should_build_binaries)" == false &&
   "$(output queue_idle candidate_purpose)" == none && -z "$(output queue_idle rehearsal_source_sha)" ]] \
  || fail "a merge group without a release built or rehearsed something: $(cat "$tmp/queue_idle.outputs")"
commit_version 0.10.144 "Version commit"
queue_sha="$(git -C "$repo" rev-parse HEAD)"
resolve queue_release EVENT_NAME=merge_group REF_NAME=gh-readonly-queue/main/pr-2 MERGE_GROUP_BASE="$push_base"
[[ "$(output queue_release build_mode)" == none && "$(output queue_release should_build_binaries)" == false &&
   "$(output queue_release should_package_archives)" == false && -z "$(output queue_release candidate_source_sha)" &&
   "$(matrix_targets queue_release)" == "" ]] \
  || fail "the release's merge group built a candidate off main: $(cat "$tmp/queue_release.outputs")"
[[ "$(output queue_release candidate_purpose)" == release &&
   "$(output queue_release rehearsal_source_sha)" == "$queue_sha" ]] \
  || fail "the release's merge group did not rehearse its exact commit: $(cat "$tmp/queue_release.outputs")"

# When that commit reaches main, the push builds the candidate itself. It reads
# no queue run, so unreadable GitHub cannot stall or refuse it.
resolve pushed_release PUSH_BEFORE="$push_base" FAKE_GH_FAIL=1
[[ "$(output pushed_release build_mode)" == candidate &&
   "$(output pushed_release candidate_source_sha)" == "$queue_sha" &&
   "$(output pushed_release should_package_archives)" == true &&
   "$(output pushed_release rehearsal_source_sha)" == "$queue_sha" ]] \
  || fail "the release push did not build its own candidate: $(cat "$tmp/pushed_release.outputs") $(cat "$tmp/pushed_release.log")"

# A pull request builds nothing, even the release PR itself; it only reports.
resolve pull_request EVENT_NAME=pull_request REF_NAME=8861/merge
[[ "$(output pull_request build_mode)" == none && "$(output pull_request should_build_binaries)" == false ]] \
  || fail "a pull request built something: $(cat "$tmp/pull_request.outputs")"

# A benchmark's AOT and build checkouts must remain identical when main
# advances between jobs. Execute the real setup resolver, then use its ref for
# both checkouts exactly as the workflow's two consumers do.
git -C "$repo" checkout --quiet main
commit_version 0.10.160 "Stable benchmark fixture"
benchmark_sha="$(git -C "$repo" rev-parse HEAD)"
resolve benchmark_frozen EVENT_NAME=workflow_dispatch INPUT_BENCHMARK_ONLY=true \
  INPUT_TARGETS=x86_64-unknown-linux-gnu INPUT_RUNNER_PROFILE=standard
benchmark_ref="$(output benchmark_frozen ref)"
aot_version="$(git -C "$repo" show "$benchmark_ref:Cargo.toml" | sed -n 's/^version = "\([^"]*\)"$/\1/p')"
commit_version 0.10.161-dev "Advance main after AOT"
build_version="$(git -C "$repo" show "$benchmark_ref:Cargo.toml" | sed -n 's/^version = "\([^"]*\)"$/\1/p')"
[[ "$aot_version" == "$build_version" && "$build_version" == 0.10.160 &&
   "$benchmark_ref" == "$benchmark_sha" ]] \
  || fail "split benchmark checkouts changed source/version: AOT=$aot_version build=$build_version ref=$benchmark_ref"
echo "ok: benchmark AOT and build retain 0.10.160 after main advances to 0.10.161-dev"

resolve manual_warm_frozen EVENT_NAME=workflow_dispatch INPUT_WARM_CACHE_ONLY=true
warm_sha="$(git -C "$repo" rev-parse HEAD)"
[[ "$(output manual_warm_frozen ref)" == "$warm_sha" ]] \
  || fail "manual warm retained a moving branch ref"

# Explicit source selection keeps policy from current HEAD but reads its
# actual source version, and refuses a source ref that does not bind the SHA.
git -C "$repo" tag retained-benchmark "$benchmark_sha"
git clone --bare --quiet "$repo" "$tmp/origin.git"
git -C "$repo" remote add origin "$tmp/origin.git"
resolve explicit_benchmark EVENT_NAME=workflow_dispatch INPUT_BENCHMARK_ONLY=true \
  INPUT_TARGETS=x86_64-unknown-linux-gnu INPUT_RUNNER_PROFILE=standard \
  INPUT_BENCHMARK_SOURCE_REF=retained-benchmark INPUT_BENCHMARK_SOURCE_SHA="$benchmark_sha"
[[ "$(output explicit_benchmark ref)" == "$benchmark_sha" &&
   "$(output explicit_benchmark version)" == 0.10.160 ]] \
  || fail "explicit benchmark source/version came from policy HEAD"
for source_control in missing mismatched malformed; do
  source_ref=retained-benchmark source_sha="$benchmark_sha"
  case "$source_control" in
    missing) source_ref=absent-source ;;
    mismatched) source_sha="$warm_sha" ;;
    malformed) source_sha=not-a-sha ;;
  esac
  if run_resolver "explicit_$source_control" EVENT_NAME=workflow_dispatch INPUT_BENCHMARK_ONLY=true \
      INPUT_TARGETS=x86_64-unknown-linux-gnu INPUT_RUNNER_PROFILE=standard \
      INPUT_BENCHMARK_SOURCE_REF="$source_ref" INPUT_BENCHMARK_SOURCE_SHA="$source_sha"; then
    fail "explicit benchmark admitted $source_control source identity"
  fi
done
echo "ok: explicit benchmark binds source ref, exact revision, and source version"

echo "release_candidate_trigger_test: ok"
