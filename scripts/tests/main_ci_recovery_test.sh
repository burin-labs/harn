#!/usr/bin/env bash
set -euo pipefail
root=$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)
scratch=$(mktemp -d)
trap 'rm -rf "$scratch"' EXIT
mkdir -p "$scratch/bin" "$scratch/repo/scripts/ci"
cp "$root/scripts/ci/require_full_suite.sh" "$scratch/repo/scripts/ci/"
cat > "$scratch/bin/gh" <<'SH'
#!/usr/bin/env bash
set -euo pipefail
printf '%s\n' "$*" >> "$FIXTURE/calls"
case "$*" in
  'api repos/fixture/repo/actions/runs/42')
    jq -n --arg sha "$FAILED_SHA" --arg conclusion "$OUTCOME" --arg event "${EVENT:-push}" --argjson attempt "$ATTEMPT" \
      '{name: "CI", event: $event, head_branch: "main", head_repository: {full_name: "fixture/repo"}, status: "completed", head_sha: $sha, conclusion: $conclusion, run_attempt: $attempt}'
    ;;
  *attempts/1/jobs*|*runs/42/jobs*)
    job="${FIRST_JOB:-Rust workspace tests}"
    step="${FIRST_STEP:-Run Rust workspace tests}"
    if [[ "$*" != *attempts/1/jobs* ]]; then
      job="${RETRY_JOB:-$job}"
      step="${RETRY_STEP:-$step}"
    fi
    if [[ "$*" == *--slurp* ]]; then
      jq -n --arg job "$job" --arg step "$step" --arg status "${JOB_STATUS:-completed}" \
        --arg missing "${MISSING_STEPS:-false}" --argjson count "${JOB_COUNT:-1}" \
        --arg malformed "${MALFORMED:-}" --arg empty "${NO_JOBS:-false}" \
        '[{total_count: $count, jobs: [{id: 101, name: $job, status: $status, conclusion: "failure", steps: (if $missing == "true" then null else [{name: $step, status: "completed", conclusion: "failure"}] end)}]}]
        | if $empty == "true" then .[0].total_count = 0 | .[0].jobs = []
          elif $malformed == "duplicate" then .[0].total_count = 2 | .[0].jobs += .[0].jobs
          elif $malformed == "name" then .[0].jobs[0].name = null
          elif $malformed == "id" then .[0].jobs[0].id = "101"
          elif $malformed == "steps" then .[0].jobs[0].steps = "unreported"
          elif $malformed == "step" then .[0].jobs[0].steps[0].status = false
          elif $malformed == "jobs" then .[0].jobs = null
          else . end'
    elif [[ "${NO_JOBS:-false}" != true ]]; then
      echo "$job"
    fi
    ;;
  *workflows/ci.yml/runs*)
    if [[ "${PARENT_RED:-false}" != true ]]; then
      printf '71\t%s\tcompleted\tsuccess\n' "$SOURCE_SHA"
    fi
    ;;
  *runs/71/jobs*)
    for name in 'CI status' 'Verify publishable crates' 'Stack frame budget' 'Rust workspace tests' 'Run Linux sandbox tests' 'Windows cross-compile check' 'Rust on macOS (deny-warnings build + lint)'; do
      printf '%s\tcompleted\tsuccess\n' "$name"
    done
    ;;
  'issue list'*) [[ "${DUPLICATE:-false}" != true ]] || echo 99 ;;
  'issue create'*)
    while [[ "$1" != --body-file ]]; do shift; done
    cp "$2" "$FIXTURE/issue-body"
    echo https://github.com/fixture/repo/issues/99
    ;;
  'pr list'*) : ;;
  'pr create'*) echo https://github.com/fixture/repo/pull/100 ;;
  'pr edit'*|'run rerun'*) : ;;
  *) echo "Unexpected gh call: $*" >&2; exit 9 ;;
esac
SH
cat > "$scratch/bin/harn" <<'SH'
#!/usr/bin/env bash
set -euo pipefail
[[ "$(cat source)" == good && "$(cat later)" == keep ]]
[[ "$HARN_BRANCH_COMMIT_BRANCH" == "automation/revert-main-$FAILED_SHA" ]]
echo signed-publish >> "$FIXTURE/calls"
SH
chmod +x "$scratch/bin/gh" "$scratch/bin/harn"
export PATH="$scratch/bin:$PATH" FIXTURE="$scratch" GH_REPO=fixture/repo
export GH_TOKEN=fixture-token RUN_ID=42 HARN_BIN="$scratch/bin/harn"
cd "$scratch/repo"
git init -q -b main
git config user.name Fixture
git config user.email fixture@example.com
git config commit.gpgsign false
echo good > source
git add .
git commit -qm base
echo bad > source
git add source
git commit -qm regression
FAILED_SHA=$(git rev-parse HEAD)
export FAILED_SHA
echo keep > later
git add later
git commit -qm subsequent

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
ATTEMPT=1 OUTCOME=success run_case
absent 'rerun|issue create|signed-publish'
ATTEMPT=1 OUTCOME=failure run_case
grep -Fxq 'run rerun 42 --repo fixture/repo --failed' "$scratch/calls"
absent 'issue create|signed-publish'
ATTEMPT=2 OUTCOME=success run_case
grep -Fq '[CI] Flaky main suite' "$scratch/calls"
absent 'run rerun|signed-publish'
ATTEMPT=2 OUTCOME=failure PARENT_RED=true run_case
grep -Fq '[CI] Persistent main failure' "$scratch/calls"
absent signed-publish
ATTEMPT=2 OUTCOME=failure RETRY_JOB='Windows cross-compile check' run_case
grep -Fq 'issue create' "$scratch/calls"
absent signed-publish
ATTEMPT=2 OUTCOME=failure FIRST_JOB='Binary size signal' FIRST_STEP="Fetch main's last debug measurement" run_case
absent 'signed-publish|pr create|pr edit'
grep -Fq '[CI] Persistent main failure' "$scratch/calls"
for malformed in duplicate name id steps step jobs; do
  if ATTEMPT=2 OUTCOME=failure MALFORMED="$malformed" run_case; then
    echo "Malformed $malformed census accepted" >&2; exit 1
  fi
  absent 'issue create|signed-publish|pr create|pr edit'
done
if ATTEMPT=2 OUTCOME=failure JOB_COUNT=1.5 run_case; then
  echo 'Fractional census count accepted' >&2; exit 1
fi
absent 'issue create|signed-publish|pr create|pr edit'
ATTEMPT=2 OUTCOME=failure FIRST_STEP='Download workspace test archive' run_case
absent 'signed-publish|pr create|pr edit'
ATTEMPT=2 OUTCOME=failure RETRY_STEP='Download workspace test archive' run_case
absent 'signed-publish|pr create|pr edit'
ATTEMPT=2 OUTCOME=failure MISSING_STEPS=true run_case
absent 'signed-publish|pr create|pr edit'
ATTEMPT=2 OUTCOME=failure JOB_STATUS=in_progress run_case
absent 'signed-publish|pr create|pr edit'
if ATTEMPT=2 OUTCOME=failure JOB_COUNT=2 run_case; then
  echo 'Partial job census authorized recovery' >&2; exit 1
fi
absent 'signed-publish|pr create|pr edit'
ATTEMPT=2 OUTCOME=failure DUPLICATE=true run_case
absent 'run rerun|issue create|signed-publish'
if ATTEMPT=2 OUTCOME=success NO_JOBS=true run_case; then
  echo 'No measured failure was accepted as a flake' >&2; exit 1
fi
if ATTEMPT=1 OUTCOME=failure EVENT=pull_request run_case; then
  echo 'Untrusted event was accepted' >&2; exit 1
fi
ATTEMPT=2 OUTCOME=failure run_case
grep -Fq '[CI] Persistent main failure' "$scratch/calls"
grep -Fq 'Run Rust workspace tests' "$scratch/issue-body"
grep -Fq '"measured_jobs": 1' "$scratch/issue-body"
grep -Fq '"source_cause": "unestablished"' "$scratch/issue-body"
absent 'run rerun|signed-publish|pr create|pr edit'
[[ "$(cat source)" == bad && "$(cat later)" == keep ]]
echo 'Main recovery: retry, failure census, source preservation, measurement/setup/missing/pending/partial controls passed.'
