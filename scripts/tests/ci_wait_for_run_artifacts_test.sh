#!/usr/bin/env bash
set -euo pipefail
repo_root=$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)
fixture_root=$(mktemp -d)
trap 'rm -rf "$fixture_root"' EXIT

# Exercise the real shell entrypoint with paginated API fixtures. Keep this
# transport fixture in the existing shell test boundary; jq owns JSON encoding.
cat > "$fixture_root/gh" <<'SH'
#!/usr/bin/env bash
set -euo pipefail
if [[ $# == 4 && $1 == api && $2 == rate_limit && $3 == --jq ]]; then
  # The reset is already past, except when the limit never lifts.
  now=$(date +%s)
  [[ $FIXTURE_SCENARIO == rate_limited_forever ]] && echo $((now + 100)) || echo $((now - 1))
  exit 0
fi
[[ $# == 4 && $1 == api && $3 == --paginate && $4 == --slurp ]]
case "$2" in
  /repos/burin-labs/harn/actions/runs/123/artifacts?per_page=100) kind=artifacts ;;
  /repos/burin-labs/harn/actions/runs/123/attempts/2/jobs?per_page=100) kind=jobs ;;
  *) exit 2 ;;
esac
count=0
if [[ -f "$FIXTURE_ROOT/$kind" ]]; then read -r count < "$FIXTURE_ROOT/$kind"; fi
count=$((count + 1))
printf '%s\n' "$count" > "$FIXTURE_ROOT/$kind"
if [[ $FIXTURE_SCENARIO == api_error ]]; then
  echo 'HTTP 502: Bad Gateway' >&2
  exit 1
fi
if [[ $FIXTURE_SCENARIO == rate_limited_forever ]] \
  || { [[ $FIXTURE_SCENARIO == rate_limited ]] && (( count <= 3 )); }; then
  echo 'gh: API rate limit exceeded for installation ID 1. (HTTP 403)' >&2
  exit 1
fi
if [[ $kind == artifacts ]]; then
  jq -cn --arg scenario "$FIXTURE_SCENARIO" --argjson count "$count" '
    {name:"harn-cli.tar.zst", expired:false} as $cli |
    if $scenario == "malformed_artifact" then [{artifacts:[$cli + {expired:"false"}]}]
    elif $scenario == "multiple" then
      [{artifacts:[$cli]}, {artifacts:(if $count >= 3 then [{name:"harn-security.tar.zst",expired:false}] else [] end)}]
    elif $scenario == "expired" then [{artifacts:[$cli + {expired:true}]}]
    else [{artifacts:[]}, {artifacts:(
      if $scenario == "early" or ($scenario == "delayed" and $count >= 3)
        or ($scenario == "rate_limited" and $count >= 5)
        or ($scenario == "race" and $count >= 2) or ($scenario == "queued_late" and $count >= 5)
      then [$cli] else [] end)}] end'
else
  jq -cn --arg scenario "$FIXTURE_SCENARIO" --argjson count "$count" '
    {id:12,name:"Rust workspace tests",status:"completed",
     conclusion:(if $scenario | startswith("terminal_") then $scenario | ltrimstr("terminal_") else "success" end)} |
    if (["delayed","multiple","running","rate_limited"] | index($scenario)) != null
      then . + {status:"in_progress",conclusion:null} else . end |
    if $scenario == "queued_late" then . + {status:(if $count <= 3 then "queued" else "in_progress" end),conclusion:null} else . end |
    if $scenario == "running" and $count >= 4 then . + {status:"completed",conclusion:"success"} else . end |
    if $scenario == "unknown_status" then .status = "future_terminal" else . end |
    if $scenario == "unknown_conclusion" then .conclusion = "future_success" else . end |
    if $scenario == "absent" then .name = "unrelated completed job" else . end |
    if $scenario == "malformed_job" then del(.id) else . end |
    (if $scenario == "duplicate" then [., . + {id:13}] else [.] end) as $jobs |
    if $scenario == "empty_pages" then []
    else [{jobs:[{id:1,name:"other",status:"completed"}]}, {jobs:$jobs}] end'
fi
SH
chmod 700 "$fixture_root/gh"

run_case() {
  scenario=$1
  shift
  rm -f "$fixture_root/artifacts" "$fixture_root/jobs"
  result=0
  PATH="$fixture_root:$PATH" FIXTURE_ROOT="$fixture_root" FIXTURE_SCENARIO="$scenario" \
    GITHUB_REPOSITORY=burin-labs/harn GITHUB_RUN_ID=123 GITHUB_RUN_ATTEMPT=2 \
    HARN_EXT_ARTIFACT_PRODUCER_JOB='Rust workspace tests' \
    HARN_ARTIFACT_WAIT_MAX_ATTEMPTS=3 HARN_ARTIFACT_WAIT_INTERVAL_SECONDS=0 \
    HARN_ARTIFACT_WAIT_RATE_LIMIT_MAX_SECONDS=30 \
    bash "$repo_root/scripts/ci/wait_for_run_artifacts.sh" "$@" \
      > "$fixture_root/stdout" 2> "$fixture_root/stderr" || result=$?
  artifact_reads=0
  job_reads=0
  if [[ -f "$fixture_root/artifacts" ]]; then read -r artifact_reads < "$fixture_root/artifacts"; fi
  if [[ -f "$fixture_root/jobs" ]]; then read -r job_reads < "$fixture_root/jobs"; fi
}

assert_result() {
  if [[ $result != "$1" || $artifact_reads != "$2" || $job_reads != "$3" ]]; then
    printf '%s: exit=%s artifact_reads=%s job_reads=%s; expected %s/%s/%s\n' \
      "$scenario" "$result" "$artifact_reads" "$job_reads" "$1" "$2" "$3" >&2
    cat "$fixture_root/stdout" "$fixture_root/stderr" >&2
    exit 1
  fi
}

assert_output() {
  if ! grep -Fq -- "$2" "$fixture_root/$1"; then
    printf '%s: missing %s in %s\n' "$scenario" "$2" "$1" >&2
    cat "$fixture_root/stdout" "$fixture_root/stderr" >&2
    exit 1
  fi
}

for scenario in early delayed race; do
  run_case "$scenario" harn-cli.tar.zst
  case "$scenario" in
    early) assert_result 0 1 0 ;;
    delayed) assert_result 0 3 2 ;;
    race) assert_result 0 2 1 ;;
  esac
  assert_output stdout 'run artifacts ready: harn-cli.tar.zst'
done

run_case queued_late harn-cli.tar.zst
assert_result 0 5 4
assert_output stdout 'run artifacts ready: harn-cli.tar.zst'

run_case multiple harn-cli.tar.zst harn-security.tar.zst
assert_result 0 3 2
assert_output stdout 'run artifacts ready: harn-cli.tar.zst harn-security.tar.zst'
assert_output stdout 'poll 1, producer in_progress): harn-security.tar.zst'

for conclusion in failure cancelled skipped success timed_out; do
  run_case "terminal_$conclusion" harn-cli.tar.zst
  assert_result 1 2 1
  assert_output stderr "producer 'Rust workspace tests' completed ($conclusion)"
  assert_output stderr harn-cli.tar.zst
done

for scenario in api_error absent duplicate malformed_job empty_pages unknown_status unknown_conclusion; do
  run_case "$scenario" harn-cli.tar.zst
  assert_result 1 3 3
  assert_output stderr "producer 'Rust workspace tests' state unmeasured after 3 polls"
done

run_case api_error harn-cli.tar.zst
assert_output stderr 'producer state unreadable (poll 1): HTTP 502: Bad Gateway'

# Three refused polls would exhaust the unmeasured budget of 3; a rate limit
# is waited out instead, and the artifact lands on the fifth read.
run_case rate_limited harn-cli.tar.zst
assert_result 0 5 4
assert_output stderr 'the GitHub API is rate limited; waiting 0s for the reset: gh: API rate limit exceeded'
assert_output stdout 'run artifacts ready: harn-cli.tar.zst'

run_case rate_limited_forever harn-cli.tar.zst
assert_result 1 1 1
assert_output stderr 'beyond the 30s this wait allows'

for scenario in malformed_artifact expired; do
  run_case "$scenario" harn-cli.tar.zst
  assert_result 1 2 1
  assert_output stderr "producer 'Rust workspace tests' completed (success)"
done

run_case running harn-cli.tar.zst
assert_result 1 5 4
assert_output stderr "producer 'Rust workspace tests' completed (success)"

echo 'ci_wait_for_run_artifacts_test: 23 scenarios passed'
