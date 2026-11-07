#!/usr/bin/env bash
set -euo pipefail
repo_root=$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)
: "${HARN_BIN:?provide the existing Harn executable}"
tmp=$(mktemp -d)
trap 'rm -rf "$tmp"' EXIT
mkdir "$tmp/bin"
cat > "$tmp/inventory.json" <<'JSON'
{"test-count":3,"rust-suites":{"demo::tests":{"package-name":"demo","binary-name":"tests","status":"listed","testcases":{"first":{"ignored":false,"filter-match":{"status":"matches"}},"second":{"ignored":false,"filter-match":{"status":"matches"}},"ignored":{"ignored":true,"filter-match":{"status":"matches"}}}},"unsupported":{"status":"skipped"}}}
JSON
printf '%s\n' '{"test-count":0,"rust-suites":{}}' > "$tmp/empty.json"
cat > "$tmp/bin/cargo-nextest" <<'SH'
#!/usr/bin/env bash
set -eu
printf '%s\n' "$2" >> "$TEST_CALLS"
case "$2" in
  list) cat "$TEST_INVENTORY"; exit "${TEST_LIST_STATUS:-0}" ;;
  run) exit "${TEST_RUN_STATUS:-0}" ;;
  *) exit 99 ;;
esac
SH
cat > "$tmp/bin/date" <<'SH'
#!/usr/bin/env bash
set -eu
value=$(cat "$TEST_CLOCK_STATE")
echo $(( ${TEST_NOW:-0} + value * ${TEST_CLOCK_TICK:-0} ))
echo $((value + 1)) > "$TEST_CLOCK_STATE"
SH
chmod +x "$tmp/bin/cargo-nextest" "$tmp/bin/date"
export PATH="$tmp/bin:$PATH" RUNNER_TEMP="$tmp" NEXTEST_FILTER='package(demo)'
export FLAKE_ITERATIONS_CEILING=3 FLAKE_ITERATIONS_FLOOR=2
export FLAKE_JOB_BUDGET_MINUTES=1 FLAKE_JOB_START_EPOCH=0
export TEST_CALLS="$tmp/calls" TEST_CLOCK_STATE="$tmp/clock"
export TEST_INVENTORY="$tmp/inventory.json"
export GITHUB_STEP_SUMMARY="$tmp/summary" GITHUB_OUTPUT="$tmp/outputs"

run_case() {
  local name=$1 expected_status=$2 expected_runs=$3 message=$4 status=0 runs
  : > "$TEST_CALLS"
  : > "$GITHUB_STEP_SUMMARY"
  : > "$GITHUB_OUTPUT"
  echo 0 > "$TEST_CLOCK_STATE"
  "$repo_root/scripts/ci/run_touched_test_repeats.sh" > "$tmp/$name.stdout" 2> "$tmp/$name.stderr" || status=$?
  [[ "$status" == "$expected_status" ]] || { cat "$tmp/$name.stdout" "$tmp/$name.stderr"; echo "$name: expected exit$expected_status, got$status" >&2; exit 1; }
  runs=$(grep -c '^run$' "$TEST_CALLS" || true)
  [[ "$runs" == "$expected_runs" ]] || { echo "$name: expected$expected_runs actual iterations, got$runs" >&2; exit 1; }
  grep -Fq "$message" "$tmp/$name.stdout" || { cat "$tmp/$name.stdout" "$tmp/$name.stderr"; exit 1; }
  ! grep -Fq 'Flake detected' "$tmp/$name.stdout"
}

run_case complete 0 3 'Iteration 3 completed'
grep -Fq 'Selected tests per iteration: 2' "$GITHUB_STEP_SUMMARY"
grep -Fq 'eligible=2 excluded=1 ignored=1 unlisted_suites=["unsupported"]' "$tmp/complete.stderr"
grep -Fq "inventory=$tmp/harn-flake-inventory.json" "$GITHUB_OUTPUT"
TEST_CLOCK_TICK=10 run_case degraded 0 2 'coverage is degraded'
TEST_CLOCK_TICK=20 run_case floor 1 1 'floor within budget'
TEST_NOW=61 run_case expired 1 0 'budget expired'
TEST_LIST_STATUS=70 run_case discovery 2 0 'discovery failed'
TEST_INVENTORY="$tmp/empty.json" run_case empty 2 0 'inventory is invalid or empty'
TEST_RUN_STATUS=100 run_case failed 100 1 'test failure on iteration 1'
TEST_RUN_STATUS=70 run_case runner 70 1 'runner/discovery error (exit 70)'
echo 'Touched-test repeatability: 8 owning controls passed; no real clock or Cargo build used.'
