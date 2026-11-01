#!/usr/bin/env bash
# Run the actual planner against named API/log/manifest observations. No
# candidate is built, published, dispatched or retired by this fixture.
set -euo pipefail
root="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
fixture="$(mktemp -d)"
trap 'rm -rf "$fixture"' EXIT
mkdir -p "$fixture/bin" "$fixture/workspace"
printf '[workspace.package]\nversion = "1.2.4"\n' > "$fixture/workspace/Cargo.toml"
export RETIRE_SOURCE_SHA=aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa
export RETIRE_PRODUCER_RUN=3 RETIRE_PROMOTION_RUN=5 RETIRE_RESOLVER_JOB=6
export RETIRE_RELEASE_PR=2
export RETIRE_CONSUMER_JOB=7 RETIRE_AUTHORIZATION_JOB=8 RETIRE_CONSUMER_RUN=9 RETIRE_FAILED_JOB=10
export RETIRE_CONSUMER_REPOSITORY=example/consumer
export GITHUB_REPOSITORY=example/harn GH_TOKEN=fixture
export FIXTURE_ROOT="$fixture" HARN_RELEASE_ROOT="$fixture/workspace" PUBLISHED_TAG=v1.2.3
export PATH="$fixture/bin:$PATH"
source "$root/scripts/lib/full_suite_jobs.sh"
FULL_SUITE_JOBS="$(printf '%s\n' "${RELEASE_FULL_SUITE_JOBS[@]}")"
export FULL_SUITE_JOBS

cat > "$fixture/bin/git" <<'EOF'
#!/usr/bin/env bash
set -euo pipefail
case "$*" in
  'status --porcelain --untracked-files=normal'|'fetch --quiet origin main') exit 0 ;;
  'show origin/main:Cargo.toml') printf '[workspace.package]\nversion = "1.2.4"\n'; exit 0 ;;
  'rev-parse HEAD') printf '%s\n' "$RETIRE_SOURCE_SHA"; exit 0 ;;
  'ls-remote --refs origin refs/heads/main '* )
    printf '%040d\trefs/heads/main\n' 1
    printf '%s\trefs/heads/release-attempt/v1.2.4/%s\n' "$RETIRE_SOURCE_SHA" "$RETIRE_SOURCE_SHA"
    [[ "${MUTATION:-}" != conflicting_attempt ]] || printf '%040d\trefs/heads/release-attempt/v1.2.4/%040d\n' 2 2
    [[ "${MUTATION:-}" != misnamed_attempt ]] || printf '%040d\trefs/heads/release-attempt/v1.2.4/%040d\n' 2 3
    case "${MUTATION:-}" in
      closed_attempt|merged_closed_attempt|foreign_closed_attempt)
        printf '%s\trefs/heads/release-attempt/v1.2.4/%s\n' "$(printf 'c%.0s' {1..40})" "$(printf 'c%.0s' {1..40})" ;;
    esac
    exit 0 ;;
esac
[[ "$*" == 'ls-remote --tags origin' ]] || exit 2
[[ "${MUTATION:-}" != tags_unreadable ]] || exit 1
[[ "${MUTATION:-}" == known_tag_absent ]] || printf '%040d\trefs/tags/v1.2.3\n' 1
if [[ "${MUTATION:-}" == tag_present || (-e "$FIXTURE_ROOT/prepared" && "${ALLOW_OPEN:-}" != 1) ]]; then
  printf '%040d\trefs/tags/v1.2.4\n' 2
fi
exit 0
EOF
cat > "$fixture/bin/gh" <<'EOF'
#!/usr/bin/env bash
set -euo pipefail
if [[ "$1 $2" == 'release view' ]]; then
  printf '%s\n' '{"tagName":"v1.2.3","isDraft":false,"isPrerelease":false,"publishedAt":"2026-10-03T00:00:00Z"}'
  exit 0
fi
# Manifest archive <id> for this mutation: 41 is the latest upload, 40 an
# earlier attempt's copy of the same name. Only a usable stale copy is good.
manifest_zip() {
  local id="$1" dir="$FIXTURE_ROOT/manifests/${MUTATION:-none}-$1" run_id=3
  if [[ ! -s "$dir.zip" ]]; then
    mkdir -p "$dir"
    [[ "$id" == 41 && "${MUTATION:-}" != manifest_mismatch ]] ||
      [[ "$id" == 40 && "${MUTATION:-}" == rerun_latest_expired ]] || run_id=99
    jq -n --arg source "${RETIRE_SOURCE_SHA}" --arg run "$run_id" '
      {schemaVersion:"burin-labs.candidate_manifest.v1",repository:"example/harn",
       sourceCommit:$source,runId:$run,runAttempt:"1",artifacts:
       (["aarch64-apple-darwin","aarch64-unknown-linux-gnu","x86_64-apple-darwin",
         "x86_64-pc-windows-msvc","x86_64-unknown-linux-gnu"] | map({kind:"archive",target:.,
         sha256:("a" * 64),attestationPredicateType:"https://harnlang.com/attestations/release-archive/v1"}))}
    ' > "$dir/candidate-manifest.json"
    (cd "$dir" && zip -q -X "$dir.zip" candidate-manifest.json)
  fi
  printf '%s\n' "$dir.zip"
}
manifest_artifact() {
  local id="$1" archive digest
  archive="$(manifest_zip "$id")"
  if command -v sha256sum >/dev/null 2>&1; then digest="$(sha256sum "$archive" | cut -d ' ' -f 1)"
  else digest="$(shasum -a 256 "$archive" | cut -d ' ' -f 1)"; fi
  jq -n --argjson id "$id" --arg source "$RETIRE_SOURCE_SHA" --arg digest "sha256:$digest" \
    --argjson size "$(wc -c < "$archive")" '
    {id:$id,name:("candidate-manifest-"+$source),expired:false,size_in_bytes:$size,digest:$digest,
     workflow_run:{id:3,head_sha:$source}}'
}
if [[ "$1 $2" == 'pr list' ]]; then exit 0; fi
if [[ "$1 $2" == 'pr create' ]]; then
  while [[ "$1" != --body-file ]]; do shift; done
  cp "$2" "$FIXTURE_ROOT/opened-body"
  printf 'https://github.com/example/harn/pull/12\n'
  exit 0
fi
if [[ "$1 $2" == 'pr edit' ]]; then exit 0; fi
[[ "$1" == api ]] || exit 2
# Main CI retries a first-attempt failure once, so the persistent failure is attempt 2.
CI_ATTEMPT=2; [[ "${MUTATION:-}" != ci_first_attempt_failure ]] || CI_ATTEMPT=1
export CI_ATTEMPT
for arg in "$@"; do [[ "$arg" != repos/* ]] || endpoint="$arg"; done
case "${endpoint:?}" in
  */pulls\?state=closed\&head=example:release/v1.2.4\&*)
    jq -n --arg mutation "${MUTATION:-}" '
      [[{number:1,state:"closed",merged_at:null,base:{ref:"main",repo:{full_name:"example/harn"}},
        head:{sha:("c"*40),ref:"release/v1.2.4",repo:{full_name:"example/harn"}}}] |
       if $mutation == "merged_closed_attempt" then .[0].merged_at="2026-10-04"
       elif $mutation == "foreign_closed_attempt" then .[0].head.repo.full_name="foreign/harn"
       else . end]' ;;
  */pulls/2)
    source="$RETIRE_SOURCE_SHA"; [[ "${MUTATION:-}" != wrong_pr_merge ]] || source=bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb
    jq -n --arg source "$source" --arg head "$RETIRE_SOURCE_SHA" '
      {number:2,state:"closed",merged:true,merged_at:"2026-10-04",merge_commit_sha:$source,
       base:{ref:"main",repo:{full_name:"example/harn"}},head:{sha:$head,repo:{full_name:"example/harn"}}}' ;;
  */git/matching-refs/heads/green) jq -n --arg source "$RETIRE_SOURCE_SHA" '[{ref:"refs/heads/green",object:{sha:$source}}]' ;;
  */compare/*\?per_page=1) echo '{"status":"identical"}' ;;
  */compare/*) jq -n --arg source "$RETIRE_SOURCE_SHA" '{status:"ahead",merge_base_commit:{sha:$source}}' ;;
  */contents/Cargo.toml\?*)
    [[ "${MUTATION:-}" != source_version_mismatch ]] || { printf '[workspace.package]\nversion = "1.2.5"\n'; exit 0; }
    printf '[workspace.package]\nversion = "1.2.4"\n' ;;
  */actions/runs/3/artifacts\?*)
    # A partial census (one listed of two counted) never selects a copy.
    count=1; [[ "${MUTATION:-}" != duplicate_manifest ]] || count=2
    latest="$(manifest_artifact 41)"
    [[ "${MUTATION:-}" != rerun_latest_expired ]] || latest="$(jq '.expired = true' <<< "$latest")"
    case "${MUTATION:-}" in
      rerun_manifest|rerun_latest_expired) census="[$(manifest_artifact 40),$latest]"; count=2 ;;
      *) census="[$latest]" ;;
    esac
    jq -n --argjson count "$count" --argjson census "$census" '{total_count:$count,artifacts:$census}' ;;
  */actions/artifacts/4[01]/zip)
    id="${endpoint%/zip}"; cat "$(manifest_zip "${id##*/}")" ;;
  */actions/runs/3)
    sha="$RETIRE_SOURCE_SHA"; [[ "${MUTATION:-}" != producer_wrong_source ]] || sha=bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb
    conclusion=success
    [[ -z "${RETIRE_SOURCE_CI_RUN:-}" ]] || conclusion=failure
    case "${MUTATION:-}" in
      producer_success|ci_success_missing_family) conclusion=success ;;
      producer_cancelled) conclusion=cancelled ;;
      producer_pending) conclusion=null ;;
    esac
    jq -n --arg sha "$sha" --arg conclusion "$conclusion" '{id:3,repository:{full_name:"example/harn"},head_repository:{full_name:"example/harn"},
      path:".github/workflows/build-release-binaries.yml",event:"merge_group",status:"completed",conclusion:$conclusion,head_sha:$sha,run_attempt:1} |
      if $conclusion == "null" then .status="in_progress" | .conclusion=null else . end' ;;
  */actions/runs/11)
    jq -n --arg sha "$RETIRE_SOURCE_SHA" --arg mutation "${MUTATION:-}" '
      {id:11,repository:{full_name:"example/harn"},head_repository:{full_name:"example/harn"},
       path:".github/workflows/ci.yml",event:"push",head_branch:"main",status:"completed",
       conclusion:"failure",head_sha:$sha,run_attempt:($ENV.CI_ATTEMPT|tonumber)} |
      if $mutation == "ci_success" or $mutation == "ci_success_producer_failed" or
        $mutation == "ci_success_missing_family" then .conclusion="success"
      elif $mutation == "ci_cancelled" then .conclusion="cancelled"
      elif $mutation == "ci_wrong_source" then .head_sha=("b"*40)
      elif $mutation == "ci_foreign_workflow" then .path=".github/workflows/foreign.yml"
      elif $mutation == "ci_foreign_repo" then .head_repository.full_name="foreign/harn"
      elif $mutation == "ci_pending" then .status="in_progress" | .conclusion=null
      elif $mutation == "ci_pr_event" then .event="pull_request"
      else . end' ;;
  */actions/runs/3/jobs\?*|*/actions/runs/11/jobs\?*)
    id="${endpoint#*/actions/runs/}"; id="${id%%/*}"
    jq -n --argjson id "$id" --arg mutation "${MUTATION:-}" '
      [{total_count:2,jobs:[{id:($id*100),run_id:$id,run_attempt:(if $id == 11 then ($ENV.CI_ATTEMPT|tonumber) else 1 end),name:"Owning check",
        status:"completed",conclusion:"failure"},{id:($id*100+1),run_id:$id,run_attempt:(if $id == 11 then ($ENV.CI_ATTEMPT|tonumber) else 1 end),
        name:"Derived verdict",status:"completed",conclusion:"failure"}]}] |
      if $mutation == "empty_jobs" then .[0]={total_count:0,jobs:[]}
      elif $mutation == "partial_jobs" then .[0].total_count=3
      elif $mutation == "pending_jobs" then .[0].jobs[0].status="queued" | .[0].jobs[0].conclusion=null
      elif $mutation == "zero_failures" then .[0].jobs[].conclusion="success"
      elif $mutation == "foreign_job_run" then .[0].jobs[0].run_id=99
      elif $mutation == "replaced_job_attempt" then .[0].jobs[0].run_attempt=3
      elif $mutation == "duplicate_jobs" then .[0].jobs[1].id=.[0].jobs[0].id
      elif $mutation == "aggregate_only_failure" then .[0].jobs[0].name="CI status" | .[0].jobs[1].name="Release candidate verdict"
      elif $id == 11 and ($mutation == "ci_success" or $mutation == "ci_success_producer_failed" or
        $mutation == "ci_success_missing_family") then
        ($ENV.FULL_SUITE_JOBS | split("\n") | map(select(length > 0))) as $names |
        [{total_count:($names|length),jobs:[$names | to_entries[] | {id:(1100+.key),run_id:11,run_attempt:($ENV.CI_ATTEMPT|tonumber),
          name:.value,status:"completed",conclusion:"success"}]}] |
        if $mutation == "ci_success_missing_family" then
          .[0].jobs |= map(if .name == "Windows cross-compile check" then .conclusion="skipped" else . end)
        else . end
      else . end' ;;
  */actions/runs/5|*/actions/runs/9)
    id="${endpoint##*/}"; repo=example/harn; path=promote-release.yml; conclusion=failure
    if [[ "$id" == 9 ]]; then repo=example/consumer; path=harn-repin-rehearsal.yml; conclusion=cancelled; fi
    [[ "${MUTATION:-}" != live_parent || "$id" != 5 ]] || conclusion=null
    event=workflow_dispatch
    [[ "${PARENT_EVENT:-}" == "" || "$id" != 5 ]] || event="$PARENT_EVENT"
    [[ "${MUTATION:-}" != foreign_parent_event || "$id" != 5 ]] || event=pull_request_target
    jq -n --argjson id "$id" --arg repo "$repo" --arg path "$path" --arg conclusion "$conclusion" --arg event "$event" '
      {id:$id,repository:{full_name:$repo},head_repository:{full_name:$repo},path:(".github/workflows/"+$path),
       event:$event,head_branch:"main",status:"completed",conclusion:$conclusion,run_attempt:1}' ;;
  */actions/jobs/*/logs)
    job="${endpoint%/logs}"; job="${job##*/}"
    case "$job" in
      6)
        printf '%s\n' '##[group]Run bash scripts/resolve-release-promotion-source.sh' 'env:' \
          "  CANDIDATE_RUN_ID: $RETIRE_PRODUCER_RUN" "  EXPECTED_SOURCE_SHA: $RETIRE_SOURCE_SHA" '##[endgroup]' ;;
      7)
        receipt='CONSUMER_CANARY failure_receipt=recorded queue_depth=0 pending=0'
        [[ "${MUTATION:-}" != stray_terminal_record ]] || receipt='CONSUMER_CANARY unexpected=1'
        printf '%s\n' '##[group]Run CANARY_REPOSITORY="$CANARY_OWNER/$CANARY_NAME" \' 'env:' \
          "  SOURCE_REVISION: $RETIRE_SOURCE_SHA" '  CANARY_WORKFLOW: harn-repin-rehearsal.yml' '##[endgroup]' \
          'CONSUMER_CANARY dispatched run=9 ref=default started_at=1700000000' \
          '##[group]Run CANARY_REPOSITORY="$CANARY_OWNER/$CANARY_NAME" bash scripts/ci/consumer_canary.sh --observe' \
          'env:' '  CANARY_RUN_ID: 9' '  CANARY_STARTED_AT: 1700000000' \
          '  CANARY_WINDOW_SECONDS: 2700' '  CANARY_DEADLINE_SECONDS: 7200' '##[endgroup]' \
          'CONSUMER_CANARY verdict=fail conclusion=cancelled run=9 wall_seconds=30' \
          'CONSUMER_CANARY timing={"owned":1,"wall_seconds":31}' \
          "$receipt" \
          '##[error]CONSUMER_CANARY reason=consumer_rehearsal_failed run=9 conclusion=cancelled' \
          '##[error]Process completed with exit code 1.' \
          '##[end-action id=observe-1.observe;outcome=failure;conclusion=failure;duration_ms=1]' ;;
      8)
        printf '%s\n' '##[group]Run bash scripts/authorize-release-rehearsal.sh' 'env:' \
          "  SOURCE_SHA: $RETIRE_SOURCE_SHA" '  REQUIRES_REHEARSAL: true' '  REHEARSAL_RESULT: failure' \
          '  REHEARSAL_VERDICT: fail' "  REHEARSAL_SOURCE_SHA: $RETIRE_SOURCE_SHA" '##[endgroup]' \
          '##[error]Process completed with exit code 1.' ;;
      *) exit 2 ;;
    esac
    printf '%s\n' 'Post job cleanup.' 'Cleaning up orphan processes' ;;
  */actions/jobs/*)
    id="${endpoint##*/}"; run=5; repo=example/harn; conclusion=failure
    step='Run the harn-linked TUI suite against the candidate'
    case "$id" in
      6) name='Resolve certified source'; conclusion=success ;;
      7) name='Recover missing consumer rehearsal / Consumer canary' ;;
      8) name='Require measured consumer completion' ;;
      10) name='Prove the candidate against the harn-linked TUI suite'; run=9; repo=example/consumer ;;
      *) exit 2 ;;
    esac
    if [[ "$id" == 10 ]]; then
      [[ "${FAILED_LEG:-}" != gauntlet ]] || {
        name='Prove the candidate against the first-run gauntlet'
        step='Run the first-run gauntlet against the candidate'; }
      [[ "${MUTATION:-}" != leg_step_mismatch ]] || name='Prove the candidate against the first-run gauntlet'
      [[ "${MUTATION:-}" != unknown_leg ]] || name='Rehearse the Harn repin surface'
    fi
    [[ "${MUTATION:-}" != wrong_job_run ]] || run=11
    [[ "${MUTATION:-}" != successful_authorizer || "$id" != 8 ]] || conclusion=success
    jq -n --argjson id "$id" --argjson run "$run" --arg repo "$repo" --arg name "$name" --arg step "$step" --arg conclusion "$conclusion" '
      {id:$id,run_id:$run,run_attempt:1,name:$name,status:"completed",conclusion:$conclusion,
       html_url:("https://github.com/"+$repo+"/actions/runs/"+($run|tostring)+"/job/"+($id|tostring)),
       steps:[{name:$step,status:"completed",conclusion:"failure"}]}' ;;
  */releases\?*)
    [[ "${MUTATION:-}" != release_unreadable ]] || exit 1
    [[ "${MUTATION:-}" != empty_releases ]] || { printf '[[]]\n'; exit 0; }
    jq -n '[ [{id:1,tag_name:"v1.2.3",draft:false,prerelease:false,published_at:"2026-10-03"}] ]' |
      if [[ "${MUTATION:-}" == release_present ]]; then
        jq '.[0] += [{id:2,tag_name:"v1.2.4",draft:true}]'
      else cat; fi ;;
  */actions/workflows/*/runs\?*)
    path="${endpoint#*/actions/workflows/}"; path="${path%%/*}"
    if [[ "$endpoint" == *'?per_page=1' ]]; then
      [[ "${MUTATION:-}" != empty_publishers ]] || { printf '{"total_count":0,"workflow_runs":[]}\n'; exit 0; }
      jq -n --arg path "$path" '{total_count:10000,workflow_runs:[{id:1,repository:{full_name:"example/harn"},path:(".github/workflows/"+$path),status:"completed"}]}'
      exit 0
    fi
    status="${endpoint#*status=}"; status="${status%%&*}"; count=0
    [[ "${MUTATION:-}" != live_publisher ]] || count=1
    [[ "${MUTATION:-}" != partial_publishers ]] || count=2
    [[ "${MUTATION:-}" != empty_publishers ]] || { printf '[{"total_count":0,"workflow_runs":[]}]\n'; exit 0; }
    jq -n --arg path "$path" --arg status "$status" --argjson count "$count" '
      [{total_count:$count,workflow_runs:(if $count == 0 then [] else [{id:1,repository:{full_name:"example/harn"},
       path:(".github/workflows/"+$path),status:$status,conclusion:null}] end)}]' ;;
  *) echo "unexpected endpoint" >&2; exit 2 ;;
esac
EOF
chmod +x "$fixture/bin/gh" "$fixture/bin/git"
export GITHUB_OUTPUT="$fixture/output"
bash "$root/scripts/plan_development_bump.sh" > "$fixture/valid.log"
grep -Fxq required=true "$GITHUB_OUTPUT"
grep -Fxq version=1.2.5-dev "$GITHUB_OUTPUT"
grep -Fxq reason=failed_unpublished_identity_retired "$GITHUB_OUTPUT"
grep -Fxq published_tag=v1.2.3 "$GITHUB_OUTPUT"
: > "$GITHUB_OUTPUT"
MUTATION=rerun_manifest bash "$root/scripts/plan_development_bump.sh" > "$fixture/rerun.log" 2>&1 || {
  cat "$fixture/rerun.log" >&2; echo 'FAIL: rerun manifest census refused' >&2; exit 1; }
grep -Fxq reason=failed_unpublished_identity_retired "$GITHUB_OUTPUT"
# The first-run gauntlet leg retires an identity the same way as the linked suite.
: > "$GITHUB_OUTPUT"
FAILED_LEG=gauntlet bash "$root/scripts/plan_development_bump.sh" > "$fixture/gauntlet-leg.log"
grep -Fxq reason=failed_unpublished_identity_retired "$GITHUB_OUTPUT"
# The automatic promotion owner is the workflow_run trigger; its failed run
# carries the same evidence as a manual dispatch.
: > "$GITHUB_OUTPUT"
PARENT_EVENT=workflow_run bash "$root/scripts/plan_development_bump.sh" > "$fixture/workflow-run.log"
grep -Fxq reason=failed_unpublished_identity_retired "$GITHUB_OUTPUT"
for mutation in tags_unreadable known_tag_absent tag_present conflicting_attempt misnamed_attempt merged_closed_attempt foreign_closed_attempt release_unreadable empty_releases \
  release_present live_publisher partial_publishers empty_publishers manifest_mismatch duplicate_manifest \
  rerun_latest_expired source_version_mismatch producer_wrong_source live_parent wrong_job_run successful_authorizer wrong_pr_merge foreign_parent_event leg_step_mismatch unknown_leg stray_terminal_record; do
  : > "$GITHUB_OUTPUT"
  if MUTATION="$mutation" bash "$root/scripts/plan_development_bump.sh" > "$fixture/$mutation.log" 2>&1; then
    echo "FAIL: accepted $mutation" >&2; exit 1
  fi
  [[ ! -s "$GITHUB_OUTPUT" ]] || { echo "FAIL: emitted authority for $mutation" >&2; exit 1; }
done
if RETIRE_SOURCE_SHA='' bash "$root/scripts/plan_development_bump.sh" > "$fixture/partial.log" 2>&1; then
  echo 'FAIL: partial retirement request authorized' >&2; exit 1
fi

# The actual producer-failure entry point has no downstream IDs or manifest.
producer_plan() {
  RETIRE_SOURCE_CI_RUN=11 RETIRE_PROMOTION_RUN='' RETIRE_RESOLVER_JOB='' \
    RETIRE_CONSUMER_JOB='' RETIRE_AUTHORIZATION_JOB='' RETIRE_CONSUMER_RUN='' RETIRE_FAILED_JOB='' \
    bash "$root/scripts/plan_development_bump.sh"
}
# A source that can never promote retires whatever its producer concluded.
for accepted in '' producer_success producer_cancelled ci_success_missing_family closed_attempt; do
  : > "$GITHUB_OUTPUT"
  MUTATION="$accepted" producer_plan > "$fixture/producer-valid-$accepted.log" 2>&1 || {
    cat "$fixture/producer-valid-$accepted.log" >&2; echo "FAIL: unpromotable source refused: ${accepted:-default}" >&2; exit 1;
  }
  grep -Fxq required=true "$GITHUB_OUTPUT"
  grep -Fxq version=1.2.5-dev "$GITHUB_OUTPUT"
  grep -Fxq reason=failed_unpublished_identity_retired "$GITHUB_OUTPUT"
done
grep -Fq '"reason":"source_ci_failed"' "$fixture/producer-valid-producer_success.log"
grep -Fq '"reason":"source_ci_not_full_suite"' "$fixture/producer-valid-ci_success_missing_family.log"
grep -Fq '"missing_full_suite_jobs":["Windows cross-compile check"]' "$fixture/producer-valid-ci_success_missing_family.log"
# A promotable source (green full suite, with any producer outcome) and every
# unmeasured observation refuse.
for mutation in ci_success ci_success_producer_failed ci_cancelled ci_first_attempt_failure producer_pending \
  producer_wrong_source ci_wrong_source ci_foreign_workflow \
  ci_foreign_repo ci_pending ci_pr_event empty_jobs partial_jobs pending_jobs zero_failures \
  foreign_job_run replaced_job_attempt duplicate_jobs aggregate_only_failure \
  tags_unreadable known_tag_absent tag_present conflicting_attempt misnamed_attempt merged_closed_attempt foreign_closed_attempt release_unreadable empty_releases \
  release_present live_publisher partial_publishers empty_publishers wrong_pr_merge source_version_mismatch; do
  : > "$GITHUB_OUTPUT"
  if MUTATION="$mutation" producer_plan > "$fixture/producer-$mutation.log" 2>&1; then
    echo "FAIL: producer retirement accepted $mutation" >&2; exit 1
  fi
  [[ ! -s "$GITHUB_OUTPUT" ]] || { echo "FAIL: producer emitted authority for $mutation" >&2; exit 1; }
done
if RETIRE_SOURCE_CI_RUN=11 bash "$root/scripts/plan_development_bump.sh" > "$fixture/mixed.log" 2>&1; then
  echo 'FAIL: mixed source-CI and consumer retirement authority accepted' >&2; exit 1
fi

# Reach the actual opener, then change authoritative publication state during
# preparation. Its second read must refuse before signing a branch or opening
# a PR. The mocks never execute a compiler or contact a service.
cat > "$fixture/bin/harn" <<'EOF'
#!/usr/bin/env bash
set -euo pipefail
case "$*" in
  *'/release_metadata.harn -- current '*) sed -n 's/^version = "\([^"]*\)"/\1/p' "$HARN_RELEASE_ROOT/Cargo.toml" ;;
  *'/release_metadata.harn -- development-target '*) printf '1.2.5-dev\n' ;;
  *'/release_metadata.harn -- retire '*)
    printf '[workspace.package]\nversion = "1.2.5-dev"\n' > "$HARN_RELEASE_ROOT/Cargo.toml"
    touch "$FIXTURE_ROOT/prepared" ;;
  *'/sync_protocol_fixture_runtime_versions.harn '*|*'/sync_grammar_fitness_receipt.harn'|'dump-protocol-artifacts') ;;
  *'/bump-driver/publish_branch_commit.harn') touch "$FIXTURE_ROOT/branch_published" ;;
  *) touch "$FIXTURE_ROOT/unexpected_mutation"; exit 2 ;;
esac
EOF
cat > "$fixture/bin/cargo" <<'EOF'
#!/usr/bin/env bash
[[ "$*" == 'metadata --format-version=1' || "$*" == 'metadata --format-version=1 --locked' ]]
EOF
chmod +x "$fixture/bin/harn" "$fixture/bin/cargo"
if HARN_BIN="$fixture/bin/harn" EXPECTED_DEVELOPMENT_VERSION=1.2.5-dev RELEASE_PUBLISHED_VERSION=v1.2.3 \
  bash "$root/scripts/open_development_bump.sh" > "$fixture/action-boundary.log" 2>&1; then
  echo 'FAIL: opener accepted publication that appeared during preparation' >&2; exit 1
fi
[[ -e "$fixture/prepared" && ! -e "$fixture/unexpected_mutation" ]] || {
  cat "$fixture/action-boundary.log" >&2; echo 'FAIL: did not reach safe action-boundary refusal' >&2; exit 1;
}
rm "$fixture/prepared"
printf '[workspace.package]\nversion = "1.2.4"\n' > "$fixture/workspace/Cargo.toml"
ALLOW_OPEN=1 RETIRE_SOURCE_CI_RUN=11 RETIRE_PROMOTION_RUN='' RETIRE_RESOLVER_JOB='' \
  RETIRE_CONSUMER_JOB='' RETIRE_AUTHORIZATION_JOB='' RETIRE_CONSUMER_RUN='' RETIRE_FAILED_JOB='' \
  HARN_BIN="$fixture/bin/harn" EXPECTED_DEVELOPMENT_VERSION=1.2.5-dev RELEASE_PUBLISHED_VERSION=v1.2.3 \
  bash "$root/scripts/open_development_bump.sh" > "$fixture/producer-open.log" 2>&1
[[ -e "$fixture/branch_published" && -s "$fixture/opened-body" ]]
grep -Fq 'Source aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa can never promote: main CI 11' "$fixture/opened-body"
grep -Fxq pr_url=https://github.com/example/harn/pull/12 "$GITHUB_OUTPUT"
echo 'Unpublished retirement: consumer failures and unpromotable sources advance through the actual planner; promotable sources and false authorities refuse; the automatic promotion trigger is accepted; actual opener rechecks publication after preparation.'
