#!/usr/bin/env bash
set -euo pipefail

report_duration_budget() {
  local duration=$1 limit_minutes=$2 memory_mb=$3
  local warning_percent=80
  if [[ ! "$duration" =~ ^[0-9]+$ || ! "$limit_minutes" =~ ^[1-9][0-9]*$ ||
        ! "$memory_mb" =~ ^[1-9][0-9]*$ ]]; then
    echo "::warning::RUST_TEST_DURATION_UNMEASURED duration_seconds=$duration job_limit_minutes=$limit_minutes memory_mb=$memory_mb"
    return
  fi
  local limit_seconds=$((limit_minutes * 60))
  local warning_seconds=$((limit_seconds * warning_percent / 100))
  local warning_label="warning threshold ${warning_percent}%"
  local feedback_seconds=${4:-}
  if [[ -n "$feedback_seconds" ]]; then
    if [[ ! "$feedback_seconds" =~ ^[1-9][0-9]*$ ]] || ((feedback_seconds > limit_seconds)); then
      echo "::warning::RUST_TEST_DURATION_UNMEASURED duration_seconds=$duration job_limit_minutes=$limit_minutes feedback_budget_seconds=$feedback_seconds"
      return
    fi
    warning_seconds=$feedback_seconds
    warning_label="feedback budget ${feedback_seconds}s"
  fi
  local message="Rust build and test lane: ${duration}s of ${limit_seconds}s job limit; ${warning_label}; runner memory ${memory_mb} MiB."
  echo "$message"
  if ((duration > warning_seconds)); then
    echo "::warning::$message"
  fi
  if [[ -n "${GITHUB_STEP_SUMMARY:-}" ]]; then
    printf '\n%s\n' "$message" >> "$GITHUB_STEP_SUMMARY"
  fi
}

# The owning reporter accepts measured values so its threshold controls need
# neither long-running test commands nor a fake clock.
if [[ "${BASH_SOURCE[0]}" != "$0" ]]; then
  return
fi

if [[ $# -eq 0 ]]; then
  echo "usage: $0 <test command> [args...]" >&2
  exit 2
fi

# Run the tests in the one test environment `make test` also uses
# (scripts/harn_test_env.sh): the 16 MiB thread stack the production CLI uses,
# no ambient egress policy or config, an empty user config directory, and a
# fresh session store. This wrapper only adds CI resource reporting.
test_env="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)/harn_test_env.sh"

report_resources() {
  local label="$1"

  echo "::group::$label"
  if command -v nproc >/dev/null 2>&1; then
    echo "nproc=$(nproc)"
  fi
  print_memory
  df -h . || true
  echo "::endgroup::"

  if [[ -n "${GITHUB_STEP_SUMMARY:-}" ]]; then
    {
      echo "### $label"
      echo
      if command -v nproc >/dev/null 2>&1; then
        echo "- \`nproc=$(nproc)\`"
      fi
      echo
      echo '```text'
      print_memory
      echo
      df -h . || true
      echo '```'
      echo
    } >> "$GITHUB_STEP_SUMMARY"
  fi
}

print_memory() {
  if command -v free >/dev/null 2>&1; then
    free -m || true
  elif command -v vm_stat >/dev/null 2>&1; then
    vm_stat || true
  else
    echo "memory report unavailable"
  fi
}

# shellcheck disable=SC2329 # invoked by traps below
report_on_signal() {
  local signal="$1" status=143
  if [[ "$signal" == INT ]]; then
    status=130
  fi
  trap - TERM INT
  if [[ -n "$child_pid" ]]; then
    # The owned test-env Bash child cannot trap an inherited ignored INT.
    kill -s TERM "$child_pid" 2>/dev/null || true
    wait "$child_pid" 2>/dev/null || true
  fi
  report_resources "Rust test resources after SIG${signal}"
  exit "$status"
}

child_pid=""
trap 'report_on_signal TERM' TERM
trap 'report_on_signal INT' INT

report_resources "Rust test resources before"
memory_mb=unmeasured
if [[ -n "${RUST_TEST_JOB_LIMIT_MINUTES:-}" ]]; then
  source "$(dirname "$test_env")/ci/rust_resource_budget.sh"
  memory_mb=$(host_memory_mb) || memory_mb=unmeasured
fi
started=$SECONDS
status=0
if [[ -n "${RUST_TEST_INVENTORY_PATH:-}" ]]; then
  reporter="${RUST_TEST_REPORTER:?captured nextest requires its prepared reporter}"
  events="${RUST_TEST_STDOUT_PATH:?captured nextest requires an event destination}"
  planner="$(dirname "${BASH_SOURCE[0]}")/nextest_inventory.harn"
  plan="$events.plan.json"
  : >"$events"
  "$reporter" run --standalone --no-sandbox "$planner" -- \
    --execution-plan "$RUST_TEST_INVENTORY_PATH" >"$plan"
  count=$(jq -er 'length | select(. > 0)' "$plan")
  no_fail_fast=0
  for argument in "$@"; do
    [[ "$argument" != --no-fail-fast ]] || no_fail_fast=1
  done
  for ((invocation = 0; invocation < count; invocation++)); do
    filter=$(jq -er ".[$invocation].filter" "$plan")
    args=("$@")
    found=0
    for ((index = 0; index < ${#args[@]}; index++)); do
      case "${args[$index]}" in
        -E|--filter-expr)
          ((index += 1))
          args[index]="(${args[$index]}) and ($filter)"
          found=1
          ;;
        --filter-expr=*)
          args[index]="--filter-expr=(${args[$index]#--filter-expr=}) and ($filter)"
          found=1
          ;;
        -E?*)
          args[index]="-E(${args[$index]#-E}) and ($filter)"
          found=1
          ;;
      esac
    done
    ((found)) || args+=(-E "$filter")
    jq -cn --argjson invocation "$(jq -c ".[$invocation]" "$plan")" \
      '{type:"harn-invocation",event:"started",binary_id:$invocation.binary_id}' >>"$events"
    bash "$test_env" --per-test-state "${args[@]}" >>"$events" &
    child_pid=$!
    invocation_status=0
    wait "$child_pid" || invocation_status=$?
    child_pid=""
    if ((invocation_status != 0)); then
      status=$invocation_status
      # Pinned nextest 0.9.132: NextestExitCode::TEST_RUN_FAILED is 100.
      # Infrastructure and signal exits must not launch another group.
      ((no_fail_fast && invocation_status == 100)) || break
    fi
  done
elif [[ -n "${RUST_TEST_STDOUT_PATH:-}" ]]; then
  bash "$test_env" --per-test-state "$@" >"$RUST_TEST_STDOUT_PATH" &
  child_pid=$!
  wait "$child_pid" || status=$?
  child_pid=""
else
  bash "$test_env" --per-test-state "$@" &
  child_pid=$!
  wait "$child_pid" || status=$?
  child_pid=""
fi
duration=$((SECONDS - started))
if [[ -n "${RUST_TEST_JOB_LIMIT_MINUTES:-}" ]]; then
  report_duration_budget "$duration" "$RUST_TEST_JOB_LIMIT_MINUTES" "$memory_mb" "${RUST_TEST_FEEDBACK_BUDGET_SECONDS:-}"
fi
report_resources "Rust test resources after"
echo "rust_test_lane_seconds=${duration}"
if [[ -n "${GITHUB_STEP_SUMMARY:-}" ]]; then
  {
    echo "### Rust build and test lane"
    echo
    echo "- Duration: ${duration}s"
    echo "- Exit status: ${status}"
  } >> "$GITHUB_STEP_SUMMARY"
fi
exit "$status"
