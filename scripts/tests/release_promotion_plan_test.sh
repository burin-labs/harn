#!/usr/bin/env bash
# Execute promote-release.yml's real plan step against fixture build runs. A
# dispatch names a commit. It publishes only a stable version that commit declares,
# with no release, above the latest published one, whose candidate qualifies and
# whose full CI passed. Every other request is refused rather than passed as a
# no-op, and an older commit may publish.
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
grep -Fq 'release_require_promotable_version' "$tmp/plan.sh" \
  || fail "could not extract the plan step from $workflow"

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
printf '# Changelog\n\n## v0.10.141\n\n- Earlier.\n' > "$repo/CHANGELOG.md"

# gh stub: FAKE_TAG_SHA is the commit the tag points at (unset: no tag),
# FAKE_RELEASE=1 means the release exists, FAKE_LATEST is the latest published tag
# (default v0.10.141), and FAKE_CANDIDATE_RUN is the run that built the candidate
# (default 9001). FAKE_GREEN is how green relates to the head (default identical;
# absent: no green branch), and every call is appended to $GH_CALLS.
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
  */releases/latest)
    [[ "${FAKE_LATEST_FAIL:-0}" != 1 ]] || exit 1
    printf '%s\n' "${FAKE_LATEST:-v0.10.141}"; exit 0 ;;
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
  "api "*/actions/runs/*/artifacts\?per_page=100*)
    run="${FAKE_CANDIDATE_RUN-9001}"
    if [[ ! -f "$GH_CALLS.dir/binding.zip" ]]; then
    jq -n --arg source "$HEAD_SHA" --arg producer "$run" \
      '{schemaVersion:"burin-labs.candidate_manifest.v1",repository:"burin-labs/harn",sourceCommit:$source,
        runId:$producer,runAttempt:"1",artifacts:[]}' > "$GH_CALLS.dir/candidate-manifest.json"
    rm -f "$GH_CALLS.dir/manifest.zip"
    (cd "$GH_CALLS.dir" && zip -q -X manifest.zip candidate-manifest.json)
    manifest_digest="sha256:$(sha256_file_hex "$GH_CALLS.dir/manifest.zip")"
    jq -n --arg source "${FAKE_BINDING_SOURCE:-$HEAD_SHA}" --arg producer "${FAKE_BINDING_RUN:-$run}" \
      --arg manifest "${FAKE_BINDING_MANIFEST:-$manifest_digest}" \
      --arg head "$(sha256_text_hex "$tested")" --arg pull "${FAKE_BINDING_PULL:-}" \
      '{schema:"harn.release.consumer_rehearsal_binding.v1",source_sha:$source,producer_run:$producer,
        producer_attempt:1,manifest_digest:$manifest,consumer_run:"77",consumer_head_sha256:$head,
        consumer_pull:(if $pull == "" then null else $pull end),declared_pull:null,verdict:"pass"}' \
      > "$GH_CALLS.dir/consumer-rehearsal-binding.json"
    rm -f "$GH_CALLS.dir/binding.zip"
    (cd "$GH_CALLS.dir" && zip -q -X binding.zip consumer-rehearsal-binding.json)
    fi
    manifest_digest="sha256:$(sha256_file_hex "$GH_CALLS.dir/manifest.zip")"
    jq -n --argjson run "$run" --arg sha "$HEAD_SHA" --arg digest "sha256:$(sha256_file_hex "$GH_CALLS.dir/binding.zip")" \
      --arg manifest "$manifest_digest" --argjson msize "$(wc -c < "$GH_CALLS.dir/manifest.zip")" \
      --argjson bsize "$(wc -c < "$GH_CALLS.dir/binding.zip")" --arg binding "${FAKE_BINDING:-present}" '
      ([{id:78,name:("candidate-manifest-"+$sha),expired:false,size_in_bytes:$msize,digest:$manifest,workflow_run:{id:$run,head_sha:$sha}}] +
        if $binding == "present" then [{id:91,name:"consumer-rehearsal-binding-1",expired:false,size_in_bytes:$bsize,digest:$digest,workflow_run:{id:$run,head_sha:$sha}}] else [] end) as $artifacts |
      {total_count:($artifacts|length),artifacts:$artifacts}'
    exit 0 ;;
  "api "*/actions/artifacts/78/zip) cat "$GH_CALLS.dir/manifest.zip"; exit 0 ;;
  "api "*/actions/artifacts/91/zip) cat "$GH_CALLS.dir/binding.zip"; exit 0 ;;
  "api "*/actions/runs/*/attempts/1/jobs\?*)
    jq -n --argjson run "${FAKE_CANDIDATE_RUN-9001}" --arg sha "$HEAD_SHA" \
      '{total_count:1,jobs:[{id:42,name:"Consumer release rehearsal / Consumer canary",run_id:$run,
        run_attempt:1,head_sha:$sha,status:"completed",conclusion:"success"}]}' ;;
  "api "*/commits/*) [[ -n "${FAKE_TAG_SHA:-}" ]] || exit 1; printf '%s\n' "$FAKE_TAG_SHA" ;;
  "api "*/artifacts\?name=candidate-manifest-*)
    sha="${2#*candidate-manifest-}"; sha="${sha%%&*}"
    jq -n --arg sha "$sha" --argjson run "${FAKE_CANDIDATE_RUN-9001}" '{total_count:1,artifacts:[{id:78,name:("candidate-manifest-"+$sha),expired:false,size_in_bytes:100,digest:("sha256:"+("b"*64)),workflow_run:{id:$run,head_sha:$sha}}]}' ;;
  "api "*/actions/runs/*)
    jq -n --argjson id "${FAKE_CANDIDATE_RUN-9001}" --arg sha "${FAKE_RUN_SHA:-$HEAD_SHA}" \
      '{id:$id,head_sha:$sha,run_attempt:1,event:"push",head_branch:"main",
        path:".github/workflows/build-release-binaries.yml",repository:{full_name:"burin-labs/harn"},
        head_repository:{full_name:"burin-labs/harn"},status:"completed",conclusion:"success"}' ;;
  "api "*/releases\?per_page=50) [[ "${FAKE_RELEASE:-0}" == 1 ]] && echo true || echo false ;;
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
# A prerelease is refused by name, never passed as a no-op.
plan development
[[ "$(cat "$tmp/development.status")" != 0 && -z "$(output development promote)" ]] \
  || fail "a development source passed: $(cat "$tmp/development.log")"
grep -Fq 'not a stable release version' "$tmp/development.log" \
  || fail "the development refusal does not say why: $(cat "$tmp/development.log")"

commit_version 0.10.142 "Declare the next version"
head_sha="$(git -C "$repo" rev-parse HEAD)"
plan candidate
[[ "$(cat "$tmp/candidate.status")" == 0 && "$(output candidate promote)" == true ]] \
  || fail "the requested commit was not promoted: $(cat "$tmp/candidate.log") calls: $(cat "$tmp/candidate.calls")"
[[ "$(output candidate tag)" == v0.10.142 && "$(output candidate version)" == 0.10.142 &&
   "$(output candidate major_minor)" == 0.10 && "$(output candidate run_id)" == 9001 ]] \
  || fail "wrong release identity: $(cat "$tmp/candidate.outputs")"
grep -Fq 'matching-refs/heads/green' "$tmp/candidate.calls" || fail "the plan skipped the green check"
for green in ahead diverged absent bogus; do
  plan "green_$green" "FAKE_GREEN=$green"
  [[ "$(cat "$tmp/green_$green.status")" != 0 && "$(output "green_$green" promote)" != true ]] \
    || fail "a commit green does not contain ($green) was promoted"
done

# Falsifier: a dispatch for a commit declaring a published version is refused,
# whichever commit asks and wherever the tag points, and so is a lower one.
plan rerun "FAKE_TAG_SHA=$head_sha" FAKE_RELEASE=1 FAKE_LATEST=v0.10.142
[[ "$(cat "$tmp/rerun.status")" != 0 && "$(output rerun promote)" != true ]] \
  || fail "a published version was promoted again: $(cat "$tmp/rerun.log")"
grep -Fq 'v0.10.142 is already published' "$tmp/rerun.log" \
  || fail "the published refusal does not say why: $(cat "$tmp/rerun.log")"
! grep -Fq 'matching-refs/heads/green' "$tmp/rerun.calls" || fail "a published version waited on green"
plan published_elsewhere FAKE_TAG_SHA=1111111111111111111111111111111111111111 FAKE_RELEASE=1
[[ "$(cat "$tmp/published_elsewhere.status")" != 0 ]] \
  || fail "a version published from another commit was promoted"
plan lower FAKE_LATEST=v0.10.150
[[ "$(cat "$tmp/lower.status")" != 0 && "$(output lower promote)" != true ]] \
  || fail "a version below the latest release was promoted"
grep -Fq 'not above the latest published release v0.10.150' "$tmp/lower.log" \
  || fail "the lower refusal does not say why: $(cat "$tmp/lower.log")"
plan latest_unread FAKE_LATEST_FAIL=1
[[ "$(cat "$tmp/latest_unread.status")" != 0 ]] || fail "an unread latest release admitted a version"

# The workflow run itself is green in every fixture, including a legacy
# tolerated consumer failure. Promotion must read the real job before arming.
for verdict in pending failure cancelled; do
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
refused_by binding_other_source rehearsal_binding_unmeasured
plan binding_other_archives "FAKE_BINDING_MANIFEST=sha256:$(printf 'e%.0s' {1..64})"
refused_by binding_other_archives rehearsal_binding_unmeasured
plan binding_other_producer FAKE_BINDING_RUN=9002
refused_by binding_other_producer rehearsal_binding_unmeasured
plan binding_absent FAKE_BINDING=absent
refused_by binding_absent rehearsal_binding_unmeasured
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

# The plan publishes the run it was handed, which the resolver validated.
plan named_run BUILD_RUN_ID=4242 FAKE_CANDIDATE_RUN=4242
[[ "$(output named_run promote)" == true && "$(output named_run run_id)" == 4242 ]] \
  || fail "the requested candidate run was not retained: $(cat "$tmp/named_run.log")"

plan standalone STANDALONE_HARN=true FAKE_CONSUMER=failure
[[ "$(output standalone promote)" == true && "$(output standalone requires_rehearsal)" == false ]] \
  || fail "standalone publication incorrectly requires downstream acceptance"
! grep -Fq '/jobs?filter=latest' "$tmp/standalone.calls" \
  || fail "standalone plan incorrectly reads a consumer verdict"
plan standalone_red_source STANDALONE_HARN=true FAKE_GREEN=ahead
[[ "$(cat "$tmp/standalone_red_source.status")" != 0 ]] \
  || fail "standalone publication bypassed source CI admission"
# A checkpoint candidate has no rehearsal, so promotion measures one.
plan no_rehearsal FAKE_CONSUMER=missing
[[ "$(output no_rehearsal promote)" == true && "$(output no_rehearsal requires_rehearsal)" == true ]] \
  || fail "missing rehearsal did not require actual measurement"
[[ "$(output no_rehearsal expected_attempt)" == 1 && "$(output no_rehearsal expected_manifest_id)" == 78 &&
   "$(output no_rehearsal expected_manifest_digest)" =~ ^sha256:[0-9a-f]{64}$ ]] \
  || fail "fresh rehearsal did not capture the actual producer manifest identity"
plan no_rehearsal_other_source FAKE_CONSUMER=missing FAKE_RUN_SHA=1111111111111111111111111111111111111111
[[ "$(cat "$tmp/no_rehearsal_other_source.status")" != 0 && -z "$(output no_rehearsal_other_source promote)" ]] \
  || fail "fresh rehearsal accepted another source's producer"
plan explicit_pair FAKE_CONSUMER=missing CONSUMER_PULL_REQUEST=7
[[ "$(output explicit_pair promote)" == true && "$(output explicit_pair requires_rehearsal)" == true ]] \
  || fail "explicit consumer repair did not require actual measurement"
plan existing_pair CONSUMER_PULL_REQUEST=7
[[ "$(cat "$tmp/existing_pair.status")" != 0 ]] \
  || fail "explicit pair replaced an existing measured rehearsal"
plan verified_repair REQUIRES_CONSUMER_REPAIR=true CONSUMER_PULL_REQUEST=7 FAKE_CONSUMER=failure
[[ "$(cat "$tmp/verified_repair.status")" == 0 && "$(output verified_repair requires_rehearsal)" == true ]] \
  || fail "verified archive repair did not require a fresh consumer measurement"
[[ -z "$(output verified_repair expected_attempt)" ]] \
  || fail "archive repair received an unrelated ordinary-producer identity"
for invalid in no_pull nonnumeric_pull; do
  case "$invalid" in
    no_pull) extras=() ;;
    nonnumeric_pull) extras=(CONSUMER_PULL_REQUEST=other) ;;
  esac
  plan "repair_$invalid" REQUIRES_CONSUMER_REPAIR=true ${extras[@]+"${extras[@]}"}
  [[ "$(cat "$tmp/repair_$invalid.status")" != 0 ]] || fail "repair $invalid was admitted"
done

# A complete page count is not a complete measurement when records cannot
# identify the jobs. Exercise the actual decision, not just a parser.
census='[{"total_count":2,"jobs":[{"id":41,"name":"Build","status":"completed","conclusion":"success"},{"id":42,"name":"Consumer release rehearsal / Consumer canary","status":"completed","conclusion":"success"}]}]'
plan valid_census "FAKE_JOB_CENSUS=$census"
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
  plan "malformed_$defect" "FAKE_JOB_CENSUS=$malformed"
  [[ "$(cat "$tmp/malformed_$defect.status")" != 0 &&
     "$(output "malformed_$defect" promote)" != true &&
     "$(output "malformed_$defect" requires_rehearsal)" != true ]] \
    || fail "malformed $defect census authorized publication"
done

# A tag at this commit with no release yet is a promotion that stopped before
# publishing; it is promoted again.
plan resume "FAKE_TAG_SHA=$head_sha"
[[ "$(output resume promote)" == true ]] || fail "a tag without a release was not resumed: $(cat "$tmp/resume.log") calls: $(cat "$tmp/resume.calls")"

# Negative control: the tag already names another commit.
plan conflict FAKE_TAG_SHA=1111111111111111111111111111111111111111
[[ "$(cat "$tmp/conflict.status")" != 0 ]] || fail "a tag at another commit was not refused"
grep -Fq "not the candidate commit $head_sha" "$tmp/conflict.log" \
  || fail "the tag conflict does not name the candidate: $(cat "$tmp/conflict.log")"

# Main moved on, and the requested commit stays releasable. Historical candidates
# need not contain today's publication helpers: the controller owns those, and the
# candidate supplies only its version. An older commit may not skip any proof.
git clone --quiet --no-hardlinks "$repo" "$tmp/old-source"
rm -rf "$tmp/old-source/scripts"
commit_version 0.10.143 "Hand-declare a later version"
older=(SOURCE_DIRECTORY="$tmp/old-source" HEAD_SHA="$head_sha")
plan older_commit "${older[@]}"
[[ "$(cat "$tmp/older_commit.status")" == 0 && "$(output older_commit promote)" == true &&
   "$(output older_commit tag)" == v0.10.142 ]] \
  || fail "an older commit's unpublished version was refused: $(cat "$tmp/older_commit.log")"
plan older_commit_red "${older[@]}" FAKE_GREEN=ahead
[[ "$(cat "$tmp/older_commit_red.status")" != 0 ]] || fail "an older commit skipped the green check"
plan older_commit_failed_consumer "${older[@]}" FAKE_CONSUMER=failure
[[ "$(cat "$tmp/older_commit_failed_consumer.status")" != 0 ]] \
  || fail "an older commit bypassed its failed consumer rehearsal"
plan older_commit_lower "${older[@]}" FAKE_LATEST=v0.10.143
[[ "$(cat "$tmp/older_commit_lower.status")" != 0 ]] \
  || fail "an older commit published below the latest release"

# Main retired the version on purpose: its changelog records that, and promotion
# does not resurrect it. The record's absence is read, not assumed.
printf '# Changelog\n\n## v0.10.142 (never published)\n\n- Retired.\n\n## v0.10.141\n\n- Earlier.\n' > "$repo/CHANGELOG.md"
plan retired "${older[@]}"
[[ "$(cat "$tmp/retired.status")" != 0 && "$(output retired promote)" != true ]] \
  || fail "a retired version was resurrected: $(cat "$tmp/retired.log")"
grep -Fq 'retired as never published' "$tmp/retired.log" || fail 'retirement was not the refusal reason'
rm "$repo/CHANGELOG.md"
plan changelog_unread "${older[@]}"
[[ "$(cat "$tmp/changelog_unread.status")" != 0 ]] || fail "an unread changelog passed as not retired"

echo "release_promotion_plan_test: ok"
