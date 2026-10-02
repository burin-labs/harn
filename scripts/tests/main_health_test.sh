#!/usr/bin/env bash
# Exercise scripts/ci/main_health.sh against a stubbed `gh` and Harn.
#
# The property under test is that measuring nothing never posts success: a
# failed registry command, an empty registry, a failed history or job request,
# an empty history and a skipped judged job each post something other than
# success, while fully measured healthy suites still post success.
set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
script="$repo_root/scripts/ci/main_health.sh"
work="$(mktemp -d)"
trap 'rm -rf "$work"' EXIT
mkdir -p "$work/bin"

# Scenario knobs, read by the stubs:
#   REGISTRY        lines printed by `harn ... --health-suites`
#   REGISTRY_FAIL   make the registry command fail
#   WORKFLOWS       workflow names the repository has
#   HISTORY_<n>     run history for suite n ("id conclusion [event]" lines;
#                   conclusion `-` is in flight, event defaults to schedule)
#   HISTORY_FAIL    suite whose run-history request fails
#   JOB_<id>        judged-job conclusion for run id (empty = skipped)
#   JOBPAGE2_<id>   judged-job conclusion on the second page of run id's jobs
#   JOBS_FAIL       make every job request fail
cat > "$work/bin/harn" <<'EOF'
#!/usr/bin/env bash
[[ -z "${REGISTRY_FAIL:-}" ]] || exit 3
printf '%b' "${REGISTRY:-}"
EOF
cat > "$work/bin/gh" <<'EOF'
#!/usr/bin/env bash
args="$*"
key() { tr -c 'A-Za-z0-9\n' '_' <<< "$1"; }
case "$args" in
  *"/statuses/"*)
    for arg in "$@"; do
      case "$arg" in state=*|description=*) printf '%s\n' "$arg" >> "$POSTS" ;; esac
    done ;;
  *"commits/main"*) echo 0123456789abcdef0123456789abcdef01234567 ;;
  *"actions/workflows"*) printf '%b' "${WORKFLOWS:-}" ;;
  *"/jobs"*)
    [[ -z "${JOBS_FAIL:-}" ]] || exit 1
    id="$(sed -E 's#.*/runs/([0-9]+)/jobs.*#\1#' <<< "$args")"
    var="JOB_$id"; printf '%s\n' "${!var:-}"
    # JOBPAGE2_<id> is the judged job's conclusion on the run's second page of
    # jobs; like `gh api --paginate --jq`, that page prints its own line, and
    # only when every page is requested.
    var="JOBPAGE2_$id"
    if [[ -n "${!var:-}" && "$args" == *--paginate* ]]; then printf '%s\n' "${!var}"; fi ;;
  "run list"*)
    suite=""; prev=""
    for arg in "$@"; do [[ "$prev" == --workflow ]] && suite="$arg"; prev="$arg"; done
    [[ "$suite" != "${HISTORY_FAIL:-}" ]] || exit 1
    echo "$args" >> "$CALLS"
    filter=""; prev=""
    for arg in "$@"; do [[ "$prev" == --jq ]] && filter="$arg"; prev="$arg"; done
    var="HISTORY_$(key "$suite" | tr -d '\n')"
    # History lines are `id conclusion [event]`; `-` is an empty conclusion
    # and the event defaults to schedule. The real --jq filter is applied.
    printf '%b\n' "${!var:-}" | while read -r id conclusion event; do
      [[ -n "$id" ]] || continue
      [[ "$conclusion" != "-" ]] || conclusion=""
      printf '{"databaseId":%s,"conclusion":"%s","event":"%s"}\n' "$id" "$conclusion" "${event:-schedule}"
    done | jq -s . | jq -r "$filter" ;;
  *) echo "unexpected gh call: $args" >&2; exit 99 ;;
esac
EOF
chmod +x "$work/bin/harn" "$work/bin/gh"

failures=0
run_case() { # name expected-state [env assignments...]
  local name="$1" expected="$2"
  shift 2
  : > "$work/posts"
  : > "$work/calls"
  : > "$work/summary"
  (
    cd "$repo_root"
    env -i PATH="$work/bin:$PATH" HOME="$HOME" POSTS="$work/posts" CALLS="$work/calls" \
      GH_REPO=example/repo EVENT_NAME=schedule GITHUB_RUN_ID=1 \
      GITHUB_STEP_SUMMARY="$work/summary" HARN="$work/bin/harn" \
      WORKFLOWS=$'Alpha\nBeta\n' \
      "$@" bash "$script"
  ) > "$work/out" 2>&1 || true
  local state
  state="$(sed -n 's/^state=//p' "$work/posts" | tail -1)"
  if [[ "$state" == "$expected" ]]; then
    echo "ok   $name -> $state"
  else
    echo "FAIL $name: expected state=$expected, posted '${state:-nothing}'"
    sed 's/^/     /' "$work/out" "$work/posts"
    failures=$((failures + 1))
  fi
}

healthy=(REGISTRY=$'Alpha\t3\tschedule\t\nBeta\t3\tschedule\tJudge\n'
  HISTORY_Alpha=$'1 success\n2 failure\n' HISTORY_Beta=$'10 success\n' JOB_10=success)

run_case "fully measured healthy suites pass" success "${healthy[@]}"
run_case "a broken streak fails" failure "${healthy[@]}" \
  HISTORY_Alpha=$'1 failure\n2 failure\n3 failure\n'
run_case "a failed registry command fails" failure "${healthy[@]}" REGISTRY_FAIL=1
run_case "an empty registry fails" failure "${healthy[@]}" REGISTRY=
run_case "a missing workflow fails" failure "${healthy[@]}" WORKFLOWS=$'Alpha\n'
run_case "a failed run-history request fails" failure "${healthy[@]}" HISTORY_FAIL=Alpha
run_case "a failed job request fails" failure "${healthy[@]}" JOBS_FAIL=1
run_case "an empty history is unjudged" pending "${healthy[@]}" HISTORY_Alpha=
run_case "a skipped judged job is unjudged" pending "${healthy[@]}" JOB_10=

window="$(for id in $(seq 10 21); do printf '%s success\n' "$id"; done)"
run_case "a thin all-red judged-job window is unreadable" failure "${healthy[@]}" \
  HISTORY_Beta="$window" JOB_10=failure
run_case "a judged-job red run before a measured pass is judged" success "${healthy[@]}" \
  HISTORY_Beta="$window" JOB_10=failure JOB_11=success
run_case "an in-flight newest run is passed over" success "${healthy[@]}" \
  HISTORY_Alpha=$'1 -\n2 success\n'
run_case "an in-flight run cannot break a red streak" failure "${healthy[@]}" \
  HISTORY_Alpha=$'1 failure\n2 -\n3 failure\n4 failure\n'

run_case "a judged job on a later page of jobs is read" failure "${healthy[@]}" \
  HISTORY_Beta=$'10 success\n11 success\n12 success\n' JOB_10= JOB_11= JOB_12= \
  JOBPAGE2_10=failure JOBPAGE2_11=failure JOBPAGE2_12=failure
dispatched=(REGISTRY=$'Alpha\t2\tschedule,workflow_dispatch\t\nBeta\t3\tschedule\tJudge\n')
run_case "a dispatch run counts for a suite judged on dispatch" success "${healthy[@]}" "${dispatched[@]}" \
  HISTORY_Alpha=$'3 success workflow_dispatch\n1 failure\n2 failure\n'
run_case "a push run never counts for a suite judged on dispatch" failure "${healthy[@]}" "${dispatched[@]}" \
  HISTORY_Alpha=$'3 success push\n1 failure\n2 failure\n'
run_case "a dispatch run never counts for a schedule-only suite" failure "${healthy[@]}" \
  HISTORY_Alpha=$'4 success workflow_dispatch\n1 failure\n2 failure\n3 failure\n'

if (( failures > 0 )); then
  echo "main health: $failures case(s) failed"
  exit 1
fi
echo "main health: all cases passed"
