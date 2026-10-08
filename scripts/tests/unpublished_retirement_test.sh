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

cat > "$fixture/bin/git" <<'EOF'
#!/usr/bin/env bash
set -euo pipefail
case "$*" in
  'status --porcelain --untracked-files=normal'|'fetch --quiet origin main') exit 0 ;;
  'show origin/main:Cargo.toml') printf '[workspace.package]\nversion = "1.2.4"\n'; exit 0 ;;
  'ls-remote --refs origin refs/heads/main '* )
    printf '%040d\trefs/heads/main\n' 1
    [[ "${MUTATION:-}" != conflicting_attempt ]] || printf '%040d\trefs/heads/release-attempt/v1.2.4/%040d\n' 2 2
    exit 0 ;;
esac
[[ "$*" == 'ls-remote --tags origin' ]] || exit 2
[[ "${MUTATION:-}" != tags_unreadable ]] || exit 1
[[ "${MUTATION:-}" == known_tag_absent ]] || printf '%040d\trefs/tags/v1.2.3\n' 1
if [[ "${MUTATION:-}" == tag_present || -e "$FIXTURE_ROOT/prepared" ]]; then
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
if [[ "$1 $2" == 'run download' ]]; then
  while [[ "$1" != --dir ]]; do shift; done
  mkdir -p "$2"
  jq -n --arg source "${RETIRE_SOURCE_SHA}" '
    {schemaVersion:"burin-labs.candidate_manifest.v1",repository:"example/harn",
     sourceCommit:$source,runId:"3",runAttempt:"1",artifacts:
     (["aarch64-apple-darwin","aarch64-unknown-linux-gnu","x86_64-apple-darwin",
       "x86_64-pc-windows-msvc","x86_64-unknown-linux-gnu"] | map({kind:"archive",target:.,
       sha256:("a" * 64),attestationPredicateType:"https://harnlang.com/attestations/release-archive/v1"}))}
  ' > "$2/candidate-manifest.json"
  if [[ "${MUTATION:-}" == manifest_mismatch ]]; then
    sed 's/"runId": "3"/"runId": "99"/' "$2/candidate-manifest.json" > "$2/changed"
    mv "$2/changed" "$2/candidate-manifest.json"
  fi
  exit 0
fi
[[ "$1" == api ]] || exit 2
for arg in "$@"; do [[ "$arg" != repos/* ]] || endpoint="$arg"; done
case "${endpoint:?}" in
  */pulls/2)
    source="$RETIRE_SOURCE_SHA"; [[ "${MUTATION:-}" != wrong_pr_merge ]] || source=bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb
    jq -n --arg source "$source" --arg head "$RETIRE_SOURCE_SHA" '
      {number:2,state:"closed",merged:true,merged_at:"2026-10-04",merge_commit_sha:$source,
       base:{ref:"main",repo:{full_name:"example/harn"}},head:{sha:$head,repo:{full_name:"example/harn"}}}' ;;
  */compare/*) jq -n --arg source "$RETIRE_SOURCE_SHA" '{status:"ahead",merge_base_commit:{sha:$source}}' ;;
  */contents/Cargo.toml\?*)
    [[ "${MUTATION:-}" != source_version_mismatch ]] || { printf '[workspace.package]\nversion = "1.2.5"\n'; exit 0; }
    printf '[workspace.package]\nversion = "1.2.4"\n' ;;
  */actions/runs/3/artifacts\?*)
    count=1; [[ "${MUTATION:-}" != duplicate_manifest ]] || count=2
    jq -n --arg source "$RETIRE_SOURCE_SHA" --argjson count "$count" '
      {total_count:$count,artifacts:[{id:41,name:("candidate-manifest-"+$source),expired:false,
        workflow_run:{id:3,head_sha:$source}}]}' ;;
  */actions/runs/3)
    sha="$RETIRE_SOURCE_SHA"; [[ "${MUTATION:-}" != producer_wrong_source ]] || sha=bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb
    jq -n --arg sha "$sha" '{id:3,repository:{full_name:"example/harn"},head_repository:{full_name:"example/harn"},
      path:".github/workflows/build-release-binaries.yml",event:"merge_group",status:"completed",conclusion:"success",head_sha:$sha,run_attempt:1}' ;;
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
        printf '%s\n' '##[group]Run CANARY_REPOSITORY="$CANARY_OWNER/$CANARY_NAME" \' 'env:' \
          "  SOURCE_REVISION: $RETIRE_SOURCE_SHA" '  CANARY_WORKFLOW: harn-repin-rehearsal.yml' '##[endgroup]' \
          'CONSUMER_CANARY dispatched run=9 ref=default' \
          '##[group]Run CANARY_REPOSITORY="$CANARY_OWNER/$CANARY_NAME" bash scripts/ci/consumer_canary.sh --observe' \
          'env:' '  CANARY_RUN_ID: 9' '##[endgroup]' \
          'CONSUMER_CANARY verdict=fail conclusion=cancelled run=9 wall_seconds=30' \
          '##[error]Process completed with exit code 1.' ;;
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
    case "$id" in
      6) name='Resolve certified source'; conclusion=success ;;
      7) name='Recover missing consumer rehearsal / Consumer canary' ;;
      8) name='Require measured consumer completion' ;;
      10) name='Prove the candidate against the harn-linked TUI suite'; run=9; repo=example/consumer ;;
      *) exit 2 ;;
    esac
    [[ "${MUTATION:-}" != wrong_job_run ]] || run=11
    [[ "${MUTATION:-}" != successful_authorizer || "$id" != 8 ]] || conclusion=success
    jq -n --argjson id "$id" --argjson run "$run" --arg repo "$repo" --arg name "$name" --arg conclusion "$conclusion" '
      {id:$id,run_id:$run,run_attempt:1,name:$name,status:"completed",conclusion:$conclusion,
       html_url:("https://github.com/"+$repo+"/actions/runs/"+($run|tostring)+"/job/"+($id|tostring)),
       steps:[{name:"Run the harn-linked TUI suite against the candidate",status:"completed",conclusion:"failure"}]}' ;;
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
# The automatic promotion owner is the workflow_run trigger; its failed run
# carries the same evidence as a manual dispatch.
: > "$GITHUB_OUTPUT"
PARENT_EVENT=workflow_run bash "$root/scripts/plan_development_bump.sh" > "$fixture/workflow-run.log"
grep -Fxq reason=failed_unpublished_identity_retired "$GITHUB_OUTPUT"
for mutation in tags_unreadable known_tag_absent tag_present conflicting_attempt release_unreadable empty_releases \
  release_present live_publisher partial_publishers empty_publishers manifest_mismatch duplicate_manifest \
  source_version_mismatch producer_wrong_source live_parent wrong_job_run successful_authorizer wrong_pr_merge foreign_parent_event; do
  : > "$GITHUB_OUTPUT"
  if MUTATION="$mutation" bash "$root/scripts/plan_development_bump.sh" > "$fixture/$mutation.log" 2>&1; then
    echo "FAIL: accepted $mutation" >&2; exit 1
  fi
  [[ ! -s "$GITHUB_OUTPUT" ]] || { echo "FAIL: emitted authority for $mutation" >&2; exit 1; }
done
if RETIRE_SOURCE_SHA='' bash "$root/scripts/plan_development_bump.sh" > "$fixture/partial.log" 2>&1; then
  echo 'FAIL: partial retirement request authorized' >&2; exit 1
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
echo 'Unpublished retirement: exact failed candidate advances; 20 false authorities refuse; the automatic promotion trigger is accepted; actual opener rechecks publication after preparation.'
