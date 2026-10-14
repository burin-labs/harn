#!/usr/bin/env bash
set -euo pipefail
root=$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)
scratch=$(mktemp -d)
trap 'rm -rf "$scratch"' EXIT
mkdir -p "$scratch/bin" "$scratch/repo" "$scratch/issues"
# The gh stub keeps issues on disk so repeats across runs see earlier filings.
cat > "$scratch/bin/gh" <<'SH'
#!/usr/bin/env bash
set -euo pipefail
printf '%s\n' "$*" >> "$FIXTURE/calls"
issues="$FIXTURE/issues"
case "$*" in
  'api --help') echo '      --allow-escape-sequences   Allow printing terminal escape sequences' ;;
  *actions/jobs/*/logs)
    [[ "${LOG_FAIL:-false}" != true ]] || exit 1
    id=${*%/logs}; id=${id##*/}
    cat "$FIXTURE/log-$id" 2>/dev/null || cat "$FIXTURE/joblog"
    ;;
  *attempts/1/jobs*|*/jobs\?filter=latest*)
    first_job="${FIRST_JOB:-Rust workspace tests}"
    first_step="${FIRST_STEP:-Run Rust workspace tests}"
    if [[ "$*" == *attempts/1/jobs* ]]; then
      jobs=$(jq -n --arg job "$first_job" --arg step "$first_step" '[{id: 101, name: $job, step: $step, conclusion: "failure"}]')
      [[ -z "${SECOND_FIRST_JOB:-}" ]] || jobs=$(jq --arg job "$SECOND_FIRST_JOB" '. + [{id: 103, name: $job, step: "Run it", conclusion: "failure"}]' <<< "$jobs")
    else
      retry=failure; [[ "$OUTCOME" == failure ]] || retry=success
      jobs=$(jq -n --arg job "${RETRY_JOB:-$first_job}" --arg step "${RETRY_STEP:-$first_step}" --arg c "$retry" '[{id: 102, name: $job, step: $step, conclusion: $c}]')
      [[ -z "${SECOND_FIRST_JOB:-}" ]] || jobs=$(jq --arg job "$SECOND_FIRST_JOB" '. + [{id: 104, name: $job, step: "Run it", conclusion: "success"}]' <<< "$jobs")
    fi
    jq -n --argjson jobs "$jobs" --arg status "${JOB_STATUS:-completed}" \
      --arg missing "${MISSING_STEPS:-false}" --argjson extra "${JOB_COUNT_EXTRA:-0}" \
      --arg malformed "${MALFORMED:-}" --arg empty "${NO_JOBS:-false}" '
      [{total_count: (($jobs | length) + $extra), jobs: [$jobs[] | {id, name, status: $status, conclusion,
        steps: (if $missing == "true" then null else [{name: .step, status: "completed", conclusion: .conclusion}] end)}]}]
      | if $empty == "true" then .[0].total_count = 0 | .[0].jobs = []
        elif $malformed == "duplicate" then .[0].total_count += 1 | .[0].jobs += [.[0].jobs[0]]
        elif $malformed == "name" then .[0].jobs[0].name = null
        elif $malformed == "id" then .[0].jobs[0].id = "101"
        elif $malformed == "steps" then .[0].jobs[0].steps = "unreported"
        elif $malformed == "step" then .[0].jobs[0].steps[0].status = false
        elif $malformed == "jobs" then .[0].jobs = null
        elif $malformed == "fraction" then .[0].total_count += 0.5
        else . end'
    ;;
  'api repos/fixture/repo/actions/runs/'*)
    jq -n --arg sha "$FAILED_SHA" --arg conclusion "$OUTCOME" --arg event "${EVENT:-push}" --argjson attempt "$ATTEMPT" \
      '{name: "CI", event: $event, head_branch: "main", head_repository: {full_name: "fixture/repo"}, status: "completed", head_sha: $sha, conclusion: $conclusion, run_attempt: $attempt}'
    ;;
  *'repos/fixture/repo/issues?state=open'*)
    for f in "$issues"/*.state; do
      [[ -e "$f" && "$(cat "$f")" == open ]] || continue
      n=$(basename "$f" .state)
      jq -n --argjson n "$n" --rawfile body "$issues/$n.body" '{number: $n, body: $body, pull_request: null}'
    done | jq -s '[.]'
    ;;
  *repos/fixture/repo/compare/*) echo "${COMPARE:-ahead}" ;;
  'issue list'*)
    hash=$(grep -oE 'main-ci-signature:[0-9a-f]+' <<< "$*")
    for f in "$issues"/*.state; do
      [[ -e "$f" && "$(cat "$f")" == closed ]] || continue
      n=$(basename "$f" .state)
      ! grep -Fq "$hash" "$issues/$n.body" || echo "$n"
    done | head -1
    ;;
  'issue create'*)
    n=$(( $(find "$issues" -name '*.state' | wc -l) + 1 ))
    while [[ $# -gt 0 ]]; do
      case "$1" in --title) echo "$2" > "$issues/$n.title" ;; --body-file) cp "$2" "$issues/$n.body" ;; esac
      shift
    done
    echo open > "$issues/$n.state"; : > "$issues/$n.comments"
    echo "https://github.com/fixture/repo/issues/$n"
    ;;
  'issue view'*) cat "$issues/$3.body" ;;
  'issue edit'*) cp "$7" "$issues/$3.body" ;;
  'issue comment'*) { cat "$7"; echo '---'; } >> "$issues/$3.comments" ;;
  'issue reopen'*) echo open > "$issues/$3.state" ;;
  'issue close'*) echo closed > "$issues/$3.state"; printf '%s\n---\n' "$7" >> "$issues/$3.comments" ;;
  'run rerun'*) : ;;
  *) echo "Unexpected gh call: $*" >&2; exit 9 ;;
esac
SH
chmod +x "$scratch/bin/gh"
export PATH="$scratch/bin:$PATH" FIXTURE="$scratch" GH_REPO=fixture/repo GH_TOKEN=fixture-token
cd "$scratch/repo"

reset_issues() { rm -f "$scratch/issues"/*; }
count() { find "$scratch/issues" -name '*.state' | wc -l | tr -d ' '; }
state() { cat "$scratch/issues/$1.state"; }
comments() { grep -c '^---$' "$scratch/issues/$1.comments" || true; }
run_case() {
  : > "$scratch/calls"
  bash "${HARN_RECOVERY_SCRIPT:-$root/scripts/ci/recover_main_ci.sh}" > "$scratch/log" 2>&1
}
absent() {
  if grep -Eq "$1" "$scratch/calls"; then
    echo "Unexpected recovery action: $1" >&2
    exit 1
  fi
}
expect() { [[ "$2" == "$3" ]] || { echo "$1: expected $3, got $2" >&2; cat "$scratch/log" >&2; exit 1; }; }
log_fail() {
  printf '2026-10-10T18:%02d:29.7677757Z \033[31mFAIL  %s\033[0m\n' "$2" "$1"
  printf '2026-10-10T18:%02d:29.78Z FAIL: shard(s) %s of slice 4/4 failed (%ss)\n' "$2" "$2" "$2"
  printf "2026-10-10T18:%02d:29.79Z make[2]: *** [Makefile:%s: check-app-host] Error 1\n" "$2" "$((200 + $2))"
  printf '2026-10-10T18:%02d:29.80Z ##[error]Process completed with exit code 2.\n' "$2"
}
SHA_A=$(printf 'a%.0s' {1..40}) SHA_B=$(printf 'b%.0s' {1..40}) SHA_C=$(printf 'c%.0s' {1..40})
export FAILED_SHA=$SHA_A RUN_ID=42
log_fail check-app-host 14 > "$scratch/joblog"

ATTEMPT=1 OUTCOME=success run_case
absent 'rerun|issue create'
ATTEMPT=1 OUTCOME=failure run_case
grep -Fxq 'run rerun 42 --repo fixture/repo --failed' "$scratch/calls"
absent 'issue create'

# Falsifier: two SHAs whose retries fail the same test file one issue. Their
# logs differ in timestamps, durations, Makefile line, and shard numbers.
log_fail check-app-host 14 > "$scratch/log-102"
ATTEMPT=2 OUTCOME=failure run_case
expect 'first persistent failure files' "$(count)" 1
grep -Fq 'api --allow-escape-sequences repos/fixture/repo/actions/jobs/102/logs' "$scratch/calls"
grep -Fq '[CI] Main fails in Rust workspace tests: FAIL  check-app-host' "$scratch/issues/1.title"
grep -Fq 'persistent occurrence' "$scratch/issues/1.body"
grep -Fxq 'step: Run Rust workspace tests' "$scratch/issues/1.body"
grep -Fxq 'make target check-app-host' "$scratch/issues/1.body"
log_fail check-app-host 37 > "$scratch/log-102"
FAILED_SHA=$SHA_B RUN_ID=43 ATTEMPT=2 OUTCOME=failure run_case
expect 'repeat on a second SHA' "$(count)" 1
expect 'repeat is a comment' "$(comments 1)" 1
grep -Fxq "<!-- main-ci-last:persistent 43 $SHA_B -->" "$scratch/issues/1.body"
FAILED_SHA=$SHA_B RUN_ID=43 ATTEMPT=2 OUTCOME=failure run_case
expect 'replayed run is idempotent' "$(comments 1)" 1
absent 'issue (create|comment|edit)'

# A different failing test in the same job is a different cause.
log_fail check-docs-links 5 > "$scratch/log-102"
FAILED_SHA=$SHA_B RUN_ID=44 ATTEMPT=2 OUTCOME=failure run_case
expect 'different test files separately' "$(count)" 2

# A green run on a source that does not descend from the failure closes nothing.
COMPARE=behind FAILED_SHA=$SHA_C RUN_ID=45 ATTEMPT=1 OUTCOME=success run_case
expect 'unrelated green keeps the issue open' "$(state 1)" open
# A green descendant closes every persistent cause, and a recurrence reopens it.
FAILED_SHA=$SHA_C RUN_ID=46 ATTEMPT=1 OUTCOME=success run_case
expect 'green main closes cause 1' "$(state 1)" closed
expect 'green main closes cause 2' "$(state 2)" closed
log_fail check-app-host 51 > "$scratch/log-102"
FAILED_SHA=$SHA_C RUN_ID=47 ATTEMPT=2 OUTCOME=failure run_case
expect 'recurrence reuses the closed issue' "$(count)" 2
expect 'recurrence reopens it' "$(state 1)" open

# Mixed run: the retry clears one job and keeps another red. The cleared job is
# a flake and its issue survives later green runs; the red one is persistent.
reset_issues
log_fail flaky-thing 3 > "$scratch/log-103"
SECOND_FIRST_JOB='Binary size signal' RUN_ID=48 ATTEMPT=2 OUTCOME=failure run_case
expect 'mixed run files each cause' "$(count)" 2
grep -l 'flaky occurrence' "$scratch/issues"/*.body | grep -q .
grep -l 'persistent occurrence' "$scratch/issues"/*.body | grep -q .
flaky=$(basename "$(grep -l 'main-ci-job:Binary size signal' "$scratch/issues"/*.body)" .body)
FAILED_SHA=$SHA_C RUN_ID=49 ATTEMPT=1 OUTCOME=success run_case
expect 'green closes nothing flaky' "$(state "$flaky")" open
SECOND_FIRST_JOB='Binary size signal' RUN_ID=50 ATTEMPT=2 OUTCOME=success run_case
expect 'flake repeat stays on one issue' "$(comments "$flaky")" 1

# Fail closed: an unreadable log still files under the job and step signature.
reset_issues
LOG_FAIL=true FIRST_JOB='Binary size signal' FIRST_STEP="Fetch main's last debug measurement" ATTEMPT=2 OUTCOME=failure run_case
expect 'unreadable log still files' "$(count)" 1
grep -Fq "[CI] Main fails in Binary size signal: Fetch main's last debug measurement" "$scratch/issues/1.title"
grep -Fq 'Job log: unavailable' "$scratch/issues/1.body"

reset_issues
for malformed in duplicate name id steps step jobs fraction; do
  if ATTEMPT=2 OUTCOME=failure MALFORMED="$malformed" run_case; then
    echo "Malformed $malformed census accepted" >&2; exit 1
  fi
  absent 'issue create'
done
if ATTEMPT=2 OUTCOME=failure JOB_COUNT_EXTRA=1 run_case; then
  echo 'Partial job census accepted' >&2; exit 1
fi
absent 'issue create'
ATTEMPT=2 OUTCOME=failure MISSING_STEPS=true run_case
ATTEMPT=2 OUTCOME=failure JOB_STATUS=in_progress RUN_ID=51 run_case
if ATTEMPT=2 OUTCOME=success NO_JOBS=true run_case; then
  echo 'No measured failure was accepted as a flake' >&2; exit 1
fi
if ATTEMPT=1 OUTCOME=failure EVENT=pull_request run_case; then
  echo 'Untrusted event was accepted' >&2; exit 1
fi
echo 'Main recovery: one issue per failure cause across SHAs, repeat comments, idempotent replay, ancestry-gated close, reopen, flake retention, unreadable-log filing, census controls passed.'
