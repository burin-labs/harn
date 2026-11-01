#!/usr/bin/env bash
# Execute promote-release.yml's real plan step against fixture build runs. Any
# main source declaring an unpublished stable version, with that version's
# transition proved in its first-parent history, is promoted automatically
# from its successful candidate; a failed candidate at the version commit
# strands nothing. A red source is still refused, a release already published
# from any source is a no-op, and an unpublished tag at another commit is
# refused.
set -euo pipefail

root="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
workflow="${RELEASE_PROMOTION_WORKFLOW:-$root/.github/workflows/promote-release.yml}"
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
mkdir -p "$repo/scripts/lib" "$repo/scripts/ci" "$tmp/bin"
cp "$root/scripts/lib/release_version.sh" "$root/scripts/lib/release_candidate_run.sh" \
  "$root/scripts/lib/release_consumer_verdict.sh" "$root/scripts/lib/consumer_canary_policy.sh" \
  "$root/scripts/lib/candidate_archive_contract.sh" "$root/scripts/lib/sha256.sh" \
  "$root/scripts/lib/green_checkpoint.sh" "$repo/scripts/lib/"
cp "$root/scripts/lib/release-candidate-state.mjs" "$root/scripts/lib/consumer-observer.mjs" \
  "$root/scripts/lib/consumer-observation-budget.mjs" "$repo/scripts/lib/"
cp "$root/scripts/ci/consumer_observation_budget.json" "$repo/scripts/ci/"
cp "$root/scripts/ci/consumer_canary_policy.json" "$repo/scripts/ci/"
cp "$root/scripts/release_contract.env" "$repo/scripts/"
git -C "$repo" init -b main --quiet
git -C "$repo" config user.name "Release Promotion Test"
git -C "$repo" config user.email "release-promotion-test@example.com"
git -C "$repo" config commit.gpgsign false

# gh stub: FAKE_TAG_SHA is the commit the tag points at (unset: no tag),
# FAKE_RELEASE=1 means the release exists, FAKE_CANDIDATE_RUN is the run that
# built the candidate (default 9001, this push's own run; empty: none), and
# FAKE_RUNS_FAIL=1 makes the run listing unreadable. FAKE_GREEN is how green
# relates to the head (default identical; absent: no green branch), and every
# call is appended to $GH_CALLS.
cat > "$tmp/bin/gh" <<'EOF'
#!/usr/bin/env bash
printf '%s\n' "$*" >> "$GH_CALLS"
# shellcheck source=scripts/lib/sha256.sh
source "$FAKE_LIB/sha256.sh"
case "$2" in
  */git/matching-refs/heads/green)
    if [[ "${FAKE_GREEN:-identical}" == absent ]]; then echo '[]'
    else printf '[{"ref":"refs/heads/green","object":{"sha":"%040d"}}]\n' 0; fi
    exit 0 ;;
  */compare/*\?per_page=1) printf '{"status":"%s"}\n' "${FAKE_GREEN:-identical}"; exit 0 ;;
esac
if [[ "$*" == *"/jobs?filter=latest"* ]]; then
  if [[ -n "${FAKE_JOB_CENSUS:-}" ]]; then
    printf '%s\n' "$FAKE_JOB_CENSUS"
    exit 0
  fi
  if [[ "${FAKE_CONSUMER:-success}" == missing ]]; then
    printf '[{"total_count":1,"jobs":[{"id":41,"name":"Build","status":"completed","conclusion":"success"}]}]\n'
  else
    status=completed
    conclusion="${FAKE_CONSUMER:-success}"
    [[ "$conclusion" != pending ]] || { status=in_progress; conclusion=null; }
    [[ "$conclusion" == null ]] || conclusion="\"$conclusion\""
    printf '[{"total_count":1,"jobs":[{"id":42,"name":"Consumer release rehearsal / Consumer canary","status":"%s","conclusion":%s}]}]\n' "$status" "$conclusion"
  fi
  exit 0
fi
tested="${FAKE_CONSUMER_TESTED:-$(printf 'c%.0s' {1..40})}"
case "$1 $2" in
  # The consumer is read only with its own scoped token, and answers --jq.
  "api repos/acme/widget-host"*)
    [[ "${GH_TOKEN:-}" == fixture-consumer ]] || exit 1
    case "$2" in
      */actions/runs/77) record="$(jq -n --arg head "$tested" '{head_sha:$head}')" ;;
      */pulls/7) record="$(jq -n --arg state "${FAKE_PULL_STATE:-open}" --argjson merged "${FAKE_PULL_MERGED:-false}" \
        --arg head "${FAKE_PULL_HEAD:-$tested}" '{state:$state,merged:$merged,head:{sha:$head,repo:{full_name:"acme/widget-host"}}}')" ;;
      */compare/"$tested"...main) record="$(jq -n --arg status "${FAKE_DEFAULT_STATUS:-ahead}" '{status:$status,ahead_by:3}')" ;;
      repos/acme/widget-host) record='{"default_branch":"main"}' ;;
      *) exit 2 ;;
    esac
    jq -r "$4" <<< "$record"
    exit 0 ;;
  # The producer's binding is served as the zip its canary uploaded.
  "api "*/actions/runs/*/artifacts\?per_page=100)
    run="${FAKE_CANDIDATE_RUN-9001}"
    jq -n --arg source "${FAKE_BINDING_SOURCE:-$HEAD_SHA}" --arg producer "${FAKE_BINDING_RUN:-$run}" \
      --arg manifest "${FAKE_BINDING_MANIFEST:-sha256:$(printf 'b%.0s' {1..64})}" \
      --arg head "$(sha256_text_hex "$tested")" --arg pull "${FAKE_BINDING_PULL:-}" \
      '{schema:"harn.release.consumer_rehearsal_binding.v1",source_sha:$source,producer_run:$producer,
        producer_attempt:1,manifest_digest:$manifest,consumer_run:"77",consumer_head_sha256:$head,
        consumer_pull:(if $pull == "" then null else $pull end),declared_pull:null,verdict:"pass"}' \
      > "$GH_CALLS.dir/consumer-rehearsal-binding.json"
    rm -f "$GH_CALLS.dir/binding.zip"
    (cd "$GH_CALLS.dir" && zip -q -X binding.zip consumer-rehearsal-binding.json)
    jq -n --argjson run "$run" --arg sha "$HEAD_SHA" --arg digest "sha256:$(sha256_file_hex "$GH_CALLS.dir/binding.zip")" \
      --arg binding "${FAKE_BINDING:-present}" '{total_count:2,artifacts:([
        {id:78,name:("candidate-manifest-"+$sha),expired:false,digest:("sha256:"+("b"*64)),workflow_run:{id:$run}}] +
        if $binding == "present" then [{id:91,name:"consumer-rehearsal-binding-1",expired:false,digest:$digest,workflow_run:{id:$run}}] else [] end)}'
    exit 0 ;;
  "api "*/actions/artifacts/91/zip) cat "$GH_CALLS.dir/binding.zip"; exit 0 ;;
  "api "*/commits/*) [[ -n "${FAKE_TAG_SHA:-}" ]] || exit 1; printf '%s\n' "$FAKE_TAG_SHA" ;;
  "api "*/build-release-binaries.yml/runs\?*)
    [[ "${FAKE_RUNS_FAIL:-0}" != 1 ]] || exit 1
    run="${FAKE_CANDIDATE_RUN-9001}"
    if [[ -n "$run" ]]; then
      sha="${2#*head_sha=}"; sha="${sha%%&*}"
      jq -n --argjson run "$run" --arg sha "$sha" '{total_count:1,workflow_runs:[{id:$run,run_attempt:1,event:"push",head_branch:"main",head_sha:$sha,status:"completed",conclusion:"success"}]}'
    else echo '{"total_count":0,"workflow_runs":[]}'; fi ;;
  "api "*/artifacts\?name=candidate-manifest-*)
    sha="${2#*candidate-manifest-}"; sha="${sha%%&*}"
    jq -n --arg sha "$sha" --argjson run "${FAKE_CANDIDATE_RUN-9001}" '{total_count:1,artifacts:[{id:78,name:("candidate-manifest-"+$sha),expired:false,size_in_bytes:100,digest:("sha256:"+("b"*64)),workflow_run:{id:$run,head_sha:$sha}}]}' ;;
  "api "*/actions/runs/*)
    printf '{"id":%s,"head_sha":"%s","status":"completed","conclusion":"success"}\n' \
      "${FAKE_CANDIDATE_RUN-9001}" "${FAKE_RUN_SHA:-$HEAD_SHA}" ;;
  "release "*) [[ "${FAKE_RELEASE:-0}" == 1 ]] ;;
  "api "*/releases\?per_page=50) [[ "${FAKE_RELEASE:-0}" == 1 ]] && echo true || echo false ;;
  "issue list") ;;
  "issue create") echo "issue create $*" >> "${FAKE_ISSUE_LOG:-/dev/null}" ;;
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
  : > "$tmp/$name.calls"
  mkdir -p "$tmp/$name.calls.dir"
  local status=0
  (
    cd "$repo"
    env PATH="$tmp/bin:$PATH" GITHUB_REPOSITORY=burin-labs/harn GH_CALLS="$tmp/$name.calls" \
      FAKE_LIB="$root/scripts/lib" CANARY_REPOSITORY=acme/widget-host CONSUMER_READ_TOKEN=fixture-consumer \
      GREEN_WAIT_SECONDS=0 GREEN_POLL_SECONDS=0 \
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
[[ "$(cat "$tmp/warm.status")" == 0 && "$(output warm promote)" == false &&
   "$(output warm reconcile)" == true ]] \
  || fail "a development push was promoted or skipped reconciliation: $(cat "$tmp/warm.log")"
# Only a version commit waits on green; an ordinary push never reads it.
! grep -Fq 'matching-refs/heads/green' "$tmp/warm.calls" || fail "a development push read green"
plan warm_red FAKE_GREEN=ahead
[[ "$(cat "$tmp/warm_red.status")" == 0 && "$(output warm_red promote)" == false ]] \
  || fail "an uncertified development push failed instead of passing as a no-op"
plan recovery_warm RECOVERY=true
[[ "$(cat "$tmp/recovery_warm.status")" != 0 && -z "$(output recovery_warm promote)" ]] \
  || fail "manual recovery accepted a warm run"

commit_version 0.10.142 "Bump the workspace version"
head_sha="$(git -C "$repo" rev-parse HEAD)"
plan candidate
[[ "$(cat "$tmp/candidate.status")" == 0 && "$(output candidate promote)" == true ]] \
  || fail "the version commit was not promoted: $(cat "$tmp/candidate.log") calls: $(cat "$tmp/candidate.calls")"
[[ "$(output candidate tag)" == v0.10.142 && "$(output candidate version)" == 0.10.142 &&
   "$(output candidate major_minor)" == 0.10 && "$(output candidate run_id)" == 9001 ]] \
  || fail "wrong release identity: $(cat "$tmp/candidate.outputs")"
grep -Fq 'matching-refs/heads/green' "$tmp/candidate.calls" || fail "the version commit skipped the green check"
for green in ahead diverged absent bogus; do
  plan "green_$green" "FAKE_GREEN=$green"
  [[ "$(cat "$tmp/green_$green.status")" != 0 && "$(output "green_$green" promote)" != true ]] \
    || fail "a version commit green does not contain ($green) was promoted"
done

# The workflow run itself is green in every fixture, including a legacy
# tolerated consumer failure. Promotion must read the real job before arming.
for verdict in missing pending failure cancelled; do
  plan "consumer_$verdict" "FAKE_CONSUMER=$verdict"
  [[ "$(cat "$tmp/consumer_$verdict.status")" != 0 &&
     "$(output "consumer_$verdict" promote)" != true ]] \
    || fail "consumer $verdict authorized publication"
  grep -Fq 'Consumer release rehearsal' "$tmp/consumer_$verdict.log" \
    || fail "consumer $verdict refusal did not name its owner"
done
plan wrong_consumer_source FAKE_RUN_SHA=1111111111111111111111111111111111111111
[[ "$(cat "$tmp/wrong_consumer_source.status")" != 0 ]] \
  || fail "another source's consumer verdict authorized publication"

# A green consumer job is evidence only for what its rehearsal measured. The
# candidate's binding names the Harn source, the manifest digest and the
# consumer revision; promotion re-reads each and refuses a stale pass by name.
refused_by() {
  local name=$1 reason=$2
  [[ "$(cat "$tmp/$name.status")" != 0 && "$(output "$name" promote)" != true ]] \
    || fail "$name authorized publication: $(cat "$tmp/$name.log")"
  grep -Fq "reason=$reason" "$tmp/$name.log" || fail "$name was not refused as $reason: $(cat "$tmp/$name.log")"
  ! grep -Fq widget-host "$tmp/$name.log" || fail "$name named the consumer"
}
grep -Fq 'decision=default_advanced_from_rehearsed_head ahead_by=3' "$tmp/candidate.log" \
  || fail "an advanced consumer default branch was not logged as a decision: $(cat "$tmp/candidate.log")"
# Falsifier: the rehearsal measured Harn commit A; promotion of commit B refuses.
plan binding_other_source FAKE_BINDING_SOURCE=1111111111111111111111111111111111111111
refused_by binding_other_source rehearsal_source_mismatch
plan binding_other_archives "FAKE_BINDING_MANIFEST=sha256:$(printf 'e%.0s' {1..64})"
refused_by binding_other_archives rehearsal_archive_mismatch
plan binding_other_producer FAKE_BINDING_RUN=9002
refused_by binding_other_producer rehearsal_producer_mismatch
plan binding_absent FAKE_BINDING=absent
refused_by binding_absent rehearsal_binding_missing
# The paired pull request passes only while its rehearsed head is current.
plan paired_current FAKE_BINDING_PULL=7
[[ "$(output paired_current promote)" == true ]] || fail "a current paired head was refused: $(cat "$tmp/paired_current.log")"
grep -Fq 'consumer=pull-7 decision=rehearsed_head_current' "$tmp/paired_current.log" \
  || fail "the paired decision is not logged: $(cat "$tmp/paired_current.log")"
# Falsifier: the paired pull request moved after a passing rehearsal.
plan paired_moved FAKE_BINDING_PULL=7 "FAKE_PULL_HEAD=$(printf 'd%.0s' {1..40})"
refused_by paired_moved consumer_pull_head_moved
grep -Fq 'consumer_pull_request=7' "$tmp/paired_moved.log" || fail "the moved pairing does not name its repair"
plan paired_merged FAKE_BINDING_PULL=7 FAKE_PULL_STATE=closed FAKE_PULL_MERGED=true
refused_by paired_merged consumer_pull_merged_since_rehearsal
plan paired_closed FAKE_BINDING_PULL=7 FAKE_PULL_STATE=closed
refused_by paired_closed consumer_pull_closed_since_rehearsal
plan default_diverged FAKE_DEFAULT_STATUS=diverged
refused_by default_diverged consumer_default_diverged
plan default_current FAKE_DEFAULT_STATUS=identical
[[ "$(output default_current promote)" == true ]] || fail "a current default head was refused"
plan consumer_unread CONSUMER_READ_TOKEN=
refused_by consumer_unread consumer_read_authority_missing

# The release's merge group built the candidate, so this push's run built
# nothing: the queue's run is the one promoted.
plan from_queue FAKE_CANDIDATE_RUN=4242
[[ "$(output from_queue promote)" == true && "$(output from_queue run_id)" == 4242 ]] \
  || fail "the merge group's candidate run was not promoted: $(cat "$tmp/from_queue.log")"

# Recovery retains the requested producer rather than selecting another run
# of the same source; normal verification still reads its consumer result.
plan recovery RECOVERY=true FAKE_RUNS_FAIL=1
[[ "$(output recovery promote)" == true && "$(output recovery run_id)" == 9001 ]] \
  || fail "recovery did not retain its certified run: $(cat "$tmp/recovery.log")"
plan standalone RECOVERY=true STANDALONE_HARN=true FAKE_CONSUMER=failure
[[ "$(output standalone promote)" == true && "$(output standalone requires_rehearsal)" == false ]] \
  || fail "standalone publication incorrectly requires downstream acceptance"
! grep -Fq '/jobs?filter=latest' "$tmp/standalone.calls" \
  || fail "standalone plan incorrectly reads a consumer verdict"
plan standalone_automatic STANDALONE_HARN=true
[[ "$(cat "$tmp/standalone_automatic.status")" != 0 ]] \
  || fail "automatic publication accepted standalone scope"
plan standalone_red_source RECOVERY=true STANDALONE_HARN=true FAKE_GREEN=ahead
[[ "$(cat "$tmp/standalone_red_source.status")" != 0 ]] \
  || fail "standalone publication bypassed source CI admission"
plan recovery_missing RECOVERY=true FAKE_CONSUMER=missing
[[ "$(output recovery_missing promote)" == true && "$(output recovery_missing requires_rehearsal)" == true ]] \
  || fail "missing rehearsal did not require actual measurement"
plan recovery_explicit_pair RECOVERY=true FAKE_CONSUMER=missing CONSUMER_PULL_REQUEST=7
[[ "$(output recovery_explicit_pair promote)" == true && "$(output recovery_explicit_pair requires_rehearsal)" == true ]] \
  || fail "explicit consumer repair did not require actual measurement"
plan recovery_existing_pair RECOVERY=true CONSUMER_PULL_REQUEST=7
[[ "$(cat "$tmp/recovery_existing_pair.status")" != 0 ]] \
  || fail "explicit pair replaced an existing measured rehearsal"
plan verified_repair RECOVERY=true REQUIRES_CONSUMER_REPAIR=true CONSUMER_PULL_REQUEST=7 FAKE_CONSUMER=failure
[[ "$(cat "$tmp/verified_repair.status")" == 0 && "$(output verified_repair requires_rehearsal)" == true ]] \
  || fail "verified archive repair did not require a fresh consumer measurement"
for invalid in no_pull nonnumeric_pull not_manual; do
  case "$invalid" in
    no_pull) extras=(RECOVERY=true) ;;
    nonnumeric_pull) extras=(RECOVERY=true CONSUMER_PULL_REQUEST=other) ;;
    not_manual) extras=(RECOVERY=false CONSUMER_PULL_REQUEST=7) ;;
  esac
  plan "repair_$invalid" REQUIRES_CONSUMER_REPAIR=true "${extras[@]}"
  [[ "$(cat "$tmp/repair_$invalid.status")" != 0 ]] || fail "repair $invalid was admitted"
done
for verdict in pending failure cancelled; do
  plan "recovery_consumer_$verdict" RECOVERY=true "FAKE_CONSUMER=$verdict"
  [[ "$(cat "$tmp/recovery_consumer_$verdict.status")" != 0 ]] \
    || fail "recovery replaced a measured $verdict consumer with a new rehearsal"
done

# A complete page count is not a complete measurement when records cannot
# identify the jobs. Exercise the actual recovery decision, not just a parser.
census='[{"total_count":2,"jobs":[{"id":41,"name":"Build","status":"completed","conclusion":"success"},{"id":42,"name":"Consumer release rehearsal / Consumer canary","status":"completed","conclusion":"success"}]}]'
plan valid_census RECOVERY=true "FAKE_JOB_CENSUS=$census"
[[ "$(output valid_census promote)" == true && "$(output valid_census requires_rehearsal)" == false ]] \
  || fail 'complete named job census refused'
for defect in missing_name empty_name whitespace_name numeric_name missing_id zero_id fractional_id string_id duplicate_id missing_status missing_conclusion partial_page empty_census; do
  case "$defect" in
    missing_name) filter='del(.[0].jobs[1].name) | .[0].jobs[1].conclusion = "failure"' ;;
    empty_name) filter='.[0].jobs[1].name = ""' ;;
    whitespace_name) filter='.[0].jobs[1].name = "  "' ;;
    numeric_name) filter='.[0].jobs[1].name = 42' ;;
    missing_id) filter='del(.[0].jobs[1].id)' ;;
    zero_id) filter='.[0].jobs[1].id = 0' ;;
    fractional_id) filter='.[0].jobs[1].id = 42.5' ;;
    string_id) filter='.[0].jobs[1].id = "42"' ;;
    duplicate_id) filter='.[0].jobs[1].id = 41' ;;
    missing_status) filter='del(.[0].jobs[0].status)' ;;
    missing_conclusion) filter='del(.[0].jobs[0].conclusion)' ;;
    partial_page) filter='.[0].total_count = 3' ;;
    empty_census) filter='.[0] = {total_count:0,jobs:[]}' ;;
  esac
  malformed="$(jq -c "$filter" <<< "$census")"
  plan "malformed_$defect" RECOVERY=true "FAKE_JOB_CENSUS=$malformed"
  [[ "$(cat "$tmp/malformed_$defect.status")" != 0 &&
     "$(output "malformed_$defect" promote)" != true &&
     "$(output "malformed_$defect" requires_rehearsal)" != true ]] \
    || fail "malformed $defect census authorized recovery"
done

# Historical candidates need not contain today's publication policy helpers.
# The controller owns those helpers; the candidate supplies only version facts.
git clone --quiet --no-hardlinks "$repo" "$tmp/old-source"
rm -rf "$tmp/old-source/scripts"
plan historical_source RECOVERY=true "SOURCE_DIRECTORY=$tmp/old-source"
[[ "$(output historical_source promote)" == true ]] \
  || fail "historical source required current controller helpers: $(cat "$tmp/historical_source.log")"

# Negative controls: no run holds a candidate, or the runs cannot be read. A
# release with nothing to publish is a failure, never a green no-op.
plan no_candidate FAKE_CANDIDATE_RUN=
[[ "$(cat "$tmp/no_candidate.status")" != 0 ]] || fail "a release with no candidate run was not refused"
grep -Fq "No successful build run holds a v0.10.142 candidate" "$tmp/no_candidate.log" \
  || fail "the missing candidate is not named: $(cat "$tmp/no_candidate.log")"
plan runs_unread FAKE_RUNS_FAIL=1
[[ "$(cat "$tmp/runs_unread.status")" != 0 ]] || fail "an unreadable run listing was not refused"

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

# The class falsifier: the version commit's candidate failed, so no run holds a
# candidate there, and main moved on while still declaring v0.10.142. A later
# main commit's successful candidate, whatever event built it, publishes the
# version automatically. Before this invariant, it was a no-op and the version
# stayed stranded until a person recovered it.
commit_version 0.10.142 "Fix the cause of the failed candidate"
descendant_sha="$(git -C "$repo" rev-parse HEAD)"
plan descendant_without_candidate FAKE_CANDIDATE_RUN=
[[ "$(cat "$tmp/descendant_without_candidate.status")" == 0 &&
   "$(output descendant_without_candidate promote)" == false &&
   "$(output descendant_without_candidate reconcile)" == true ]] \
  || fail "a descendant without a candidate did not hand off to reconciliation: $(cat "$tmp/descendant_without_candidate.log")"
! grep -Fq 'matching-refs/heads/green' "$tmp/descendant_without_candidate.calls" \
  || fail "a descendant without a candidate waited on green"
plan descendant FAKE_CANDIDATE_RUN=5150
[[ "$(cat "$tmp/descendant.status")" == 0 && "$(output descendant promote)" == true &&
   "$(output descendant tag)" == v0.10.142 && "$(output descendant run_id)" == 5150 &&
   -z "$(output descendant reconcile)" ]] \
  || fail "a qualified descendant candidate of the unpublished version was not promoted: $(cat "$tmp/descendant.log")"
grep -Fq "Promoting descendant $descendant_sha of v0.10.142 version commit $head_sha" "$tmp/descendant.log" \
  || fail "the descendant promotion does not name its version commit: $(cat "$tmp/descendant.log")"
# Negative control: a red descendant never publishes, candidate or not.
for green in ahead diverged absent; do
  plan "descendant_green_$green" FAKE_CANDIDATE_RUN=5150 "FAKE_GREEN=$green"
  [[ "$(cat "$tmp/descendant_green_$green.status")" != 0 &&
     "$(output "descendant_green_$green" promote)" != true ]] \
    || fail "a descendant green does not contain ($green) was promoted"
done
plan descendant_failed_consumer FAKE_CANDIDATE_RUN=5150 FAKE_CONSUMER=failure
[[ "$(cat "$tmp/descendant_failed_consumer.status")" != 0 ]] \
  || fail "a descendant bypassed its failed consumer rehearsal"
# Newest-wins: once any candidate published the version, the rest are no-ops.
plan descendant_after_publication FAKE_CANDIDATE_RUN=5150 "FAKE_TAG_SHA=$head_sha" FAKE_RELEASE=1
[[ "$(cat "$tmp/descendant_after_publication.status")" == 0 &&
   "$(output descendant_after_publication promote)" == false ]] \
  || fail "a candidate after publication from another source was not a no-op: $(cat "$tmp/descendant_after_publication.log")"
# An unproved transition is never promoted automatically.
git clone --quiet --depth 1 "file://$repo" "$tmp/unproved-source"
plan descendant_unproved FAKE_CANDIDATE_RUN=5150 "SOURCE_DIRECTORY=$tmp/unproved-source"
[[ "$(cat "$tmp/descendant_unproved.status")" != 0 ]] \
  || fail "a descendant without a proved transition was promoted"
plan corrected_source RECOVERY=true
[[ "$(cat "$tmp/corrected_source.status")" == 0 && "$(output corrected_source promote)" == true ]] \
  || fail "certified corrected stable source was refused: $(cat "$tmp/corrected_source.log")"
plan corrected_source_failed_consumer RECOVERY=true FAKE_CONSUMER=failure
[[ "$(cat "$tmp/corrected_source_failed_consumer.status")" != 0 ]] \
  || fail "corrected source bypassed its failed consumer rehearsal"
plan corrected_source_conflicting_tag RECOVERY=true "FAKE_TAG_SHA=$head_sha"
[[ "$(cat "$tmp/corrected_source_conflicting_tag.status")" != 0 ]] \
  || fail "corrected source replaced a tag at the original source"
git clone --quiet --depth 2 "file://$repo" "$tmp/shallow-source"
plan corrected_source_unproved RECOVERY=true "SOURCE_DIRECTORY=$tmp/shallow-source"
[[ "$(cat "$tmp/corrected_source_unproved.status")" != 0 ]] \
  || fail "missing transition history authorized corrected-source recovery"
commit_version 0.10.143-rc.1 "Prerelease"
plan prerelease
[[ "$(cat "$tmp/prerelease.status")" == 0 && "$(output prerelease promote)" == false ]] \
  || fail "a prerelease was promoted"
plan recovery_prerelease RECOVERY=true
[[ "$(cat "$tmp/recovery_prerelease.status")" != 0 ]] \
  || fail "manual recovery accepted a prerelease run"

# Main has retired the older unpublished source. Recovery must not resurrect
# it, but an actually published tag still takes the existing follow-through
# path before the superseded-source guard.
commit_version 0.10.143-dev "Retire unpublished v0.10.142"
plan retired_unpublished RECOVERY=true "SOURCE_DIRECTORY=$tmp/old-source" "HEAD_SHA=$head_sha"
[[ "$(cat "$tmp/retired_unpublished.status")" != 0 &&
   "$(output retired_unpublished promote)" != true ]] || fail "retired unpublished source was resurrected"
grep -Fq 'superseded by main' "$tmp/retired_unpublished.log" || fail 'retirement was not the refusal reason'
plan published_after_cutover RECOVERY=true "SOURCE_DIRECTORY=$tmp/old-source" "HEAD_SHA=$head_sha" \
  "FAKE_TAG_SHA=$head_sha" FAKE_RELEASE=1
[[ "$(cat "$tmp/published_after_cutover.status")" == 0 &&
   "$(output published_after_cutover promote)" == false ]] || fail 'published source lost its existing completion path'

echo "release_promotion_plan_test: ok"
