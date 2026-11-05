#!/usr/bin/env bash
# Execute build-release-binaries.yml's real setup resolver against fixture
# events. Only an explicit source candidate request builds a candidate. A push
# is a warm at most, whatever version it declares: a commit declaring an
# unpublished stable version starts nothing by itself.
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
grep -Fq 'SOURCE_DECISION=' "$tmp/resolve.sh" \
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
# never a skip. Nothing in the resolver reads GitHub, so a gh call fails.
printf '#!/usr/bin/env bash\nexit 3\n' > "$tmp/bin/gh"
chmod +x "$tmp/bin/gh"
[[ -n "${HARN_BIN:-}" && -x "${HARN_BIN}" ]] \
  || fail "HARN_BIN must name an executable harn; run through make test-pr-gate-post-warm-integrations"
ln -s "$HARN_BIN" "$tmp/bin/harn"
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
      EVENT_NAME=push REF_TYPE=branch REF_NAME=main GITHUB_SHA="$(git rev-parse HEAD)" \
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

# Falsifier: a commit moving main to an unpublished stable version starts no
# candidate and no rehearsal, with or without a build input, and even when its
# subject looks like a release.
commit_version 0.10.142 "Release v0.10.142"
head_sha="$(git -C "$repo" rev-parse HEAD)"
for inputs_changed in false true; do
  resolve "version_commit_$inputs_changed" RELEASE_BUILD_INPUTS_CHANGED="$inputs_changed"
  [[ "$(output "version_commit_$inputs_changed" build_mode)" == warm &&
     "$(output "version_commit_$inputs_changed" should_package_archives)" == false &&
     -z "$(output "version_commit_$inputs_changed" candidate_source_sha)" &&
     -z "$(output "version_commit_$inputs_changed" rehearsal_source_sha)" ]] \
    || fail "a version commit started a candidate: $(cat "$tmp/version_commit_$inputs_changed.outputs")"
done
for trigger in workflow_run schedule push; do
  ! awk '/^on:/{on=1;next} /^[a-z]/{on=0} on' "$root/.github/workflows/promote-release.yml" \
    | grep -Eq "^  $trigger:" || fail "promotion starts by itself on $trigger"
done
awk '/^on:/{on=1;next} /^[a-z]/{on=0} on' "$root/.github/workflows/promote-release.yml" \
  | grep -Eq '^  workflow_dispatch:' || fail "could not read promotion's triggers"
! awk '/^on:/{on=1;next} /^[a-z]/{on=0} on' "$workflow" | grep -Eq '^  schedule:' \
  || fail "the candidate producer has its own schedule beside the checkpoint"

# Pull requests and merge groups build and rehearse nothing, version or not.
resolve release_pull_request EVENT_NAME=pull_request REF_NAME=8861/merge
[[ -z "$(output release_pull_request rehearsal_source_sha)" &&
   "$(output release_pull_request should_build_binaries)" == false &&
   "$(output release_pull_request build_mode)" == none ]] \
  || fail "a version pull request built or rehearsed something"
resolve release_merge_group EVENT_NAME=merge_group REF_NAME=gh-readonly-queue/main/pr-1
[[ -z "$(output release_merge_group rehearsal_source_sha)" &&
   "$(output release_merge_group should_build_binaries)" == false &&
   "$(output release_merge_group build_mode)" == none &&
   "$(output release_merge_group candidate_purpose)" == none ]] \
  || fail "a version merge group built or rehearsed something"

# An explicit main source build is the full candidate producer, with no
# rehearsal: promotion measures that if the candidate is ever published.
resolve source EVENT_NAME=workflow_dispatch INPUT_SOURCE_CANDIDATE=true
[[ "$(output source build_mode)" == candidate &&
   "$(output source candidate_purpose)" == source &&
   "$(output source candidate_source_sha)" == "$(git -C "$repo" rev-parse HEAD)" &&
   "$(output source should_package_archives)" == true ]] \
  || fail "explicit source candidate does not reuse the signed archive producer"
[[ "$(matrix_targets source)" == "$(matrix_targets warm)" &&
   "$(matrix_targets source)" == "aarch64-apple-darwin,aarch64-unknown-linux-gnu,x86_64-apple-darwin,x86_64-pc-windows-msvc,x86_64-unknown-linux-gnu" ]] \
  || fail "source candidate does not cover the full matrix: $(matrix_targets source)"
[[ -z "$(output source rehearsal_source_sha)" ]] \
  || fail "a checkpoint candidate requested a consumer rehearsal"
[[ "$(output source source_candidate_decision | jq -er '.accepted and .reason == "accepted"')" == true ]] \
  || fail "actual source resolver did not emit its typed admission receipt"

for invalid_source in branch tag conflict benchmark profile targets event source_ref source_sha bloat; do
  case "$invalid_source" in
    branch) invalid_args=(REF_NAME=topic) ;;
    tag) invalid_args=(REF_TYPE=tag) ;;
    conflict) invalid_args=(INPUT_WARM_CACHE_ONLY=true) ;;
    benchmark) invalid_args=(INPUT_BENCHMARK_ONLY=true) ;;
    profile) invalid_args=(INPUT_RUNNER_PROFILE=standard) ;;
    targets) invalid_args=(INPUT_TARGETS=aarch64-apple-darwin) ;;
    event) invalid_args=(EVENT_NAME=schedule) ;;
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
