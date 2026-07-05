#!/usr/bin/env bash
set -euo pipefail

repo_root=$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)
recover_script="$repo_root/scripts/recover_ci_preemption.sh"

tmp_root=$(mktemp -d)
trap 'rm -rf "$tmp_root"' EXIT

write_run_json() {
  local path="$1"
  local event="$2"
  local conclusion="$3"
  local status="$4"
  local head_branch="$5"
  local attempt="$6"
  cat > "$path" <<JSON
{
  "databaseId": 28757331133,
  "event": "$event",
  "conclusion": "$conclusion",
  "status": "$status",
  "headBranch": "$head_branch",
  "headSha": "0ee2f9a266c7228b31702776621593fa11e0c33f",
  "url": "https://github.com/burin-labs/harn/actions/runs/28757331133",
  "attempt": $attempt,
  "workflowName": "CI",
  "jobs": [
    {"databaseId": 101, "name": "Rust test", "conclusion": "failure"},
    {"databaseId": 102, "name": "Rust lint", "conclusion": "cancelled"},
    {"databaseId": 103, "name": "CI status", "conclusion": "failure"}
  ]
}
JSON
}

run_recover() {
  local run_json="$1"
  local logs_dir="$2"
  "$recover_script" \
    --repo burin-labs/harn \
    --run-id 28757331133 \
    --run-json "$run_json" \
    --logs-dir "$logs_dir"
}

assert_contains() {
  local haystack="$1"
  local needle="$2"
  if ! grep -Fq "$needle" <<< "$haystack"; then
    printf 'expected output to contain %q, got:\n%s\n' "$needle" "$haystack" >&2
    exit 1
  fi
}

fixture_dir="$tmp_root/merge-group"
mkdir -p "$fixture_dir/logs"
write_run_json \
  "$fixture_dir/run.json" \
  "merge_group" \
  "cancelled" \
  "completed" \
  "gh-readonly-queue/main/pr-4113-024b5b8702481b5563c1504aac0a33fd92d93eac" \
  1
cat > "$fixture_dir/logs/101.log" <<'LOG'
make: *** wait: No child processes.  Stop.
##[error]Process completed with exit code 143.
##[error]The runner has received a shutdown signal. This can happen when the runner service is stopped, or a manually started runner is canceled.
LOG
cat > "$fixture_dir/logs/102.log" <<'LOG'
The operation was canceled.
LOG

merge_output="$(run_recover "$fixture_dir/run.json" "$fixture_dir/logs")"
assert_contains "$merge_output" "classification=runner_preemption"
assert_contains "$merge_output" "event=merge_group"
assert_contains "$merge_output" "pr_number=4113"
assert_contains "$merge_output" "planned_action=requeue_merge_queue"
assert_contains "$merge_output" "planned_command=gh pr merge 4113 --repo burin-labs/harn --auto --squash"

pr_dir="$tmp_root/pull-request"
mkdir -p "$pr_dir/logs"
write_run_json \
  "$pr_dir/run.json" \
  "pull_request" \
  "failure" \
  "completed" \
  "codex/artifact-manifest-emit" \
  1
cat > "$pr_dir/logs/101.log" <<'LOG'
make test-affected: cargo nextest run -p harn-vm
make: *** [Makefile:101: test-affected] Terminated
##[error]Process completed with exit code 143.
##[error]The runner has received a shutdown signal. This can happen when the runner service is stopped, or a manually started runner is canceled.
LOG

pr_output="$(run_recover "$pr_dir/run.json" "$pr_dir/logs")"
assert_contains "$pr_output" "classification=runner_preemption"
assert_contains "$pr_output" "event=pull_request"
assert_contains "$pr_output" "planned_action=rerun_failed_jobs"
assert_contains "$pr_output" "planned_command=gh run rerun 28757331133 --repo burin-labs/harn --failed"

code_failure_dir="$tmp_root/code-failure"
mkdir -p "$code_failure_dir/logs"
write_run_json \
  "$code_failure_dir/run.json" \
  "pull_request" \
  "failure" \
  "completed" \
  "feature/code-failure" \
  1
cat > "$code_failure_dir/logs/101.log" <<'LOG'
error: unused import: `std::fmt`
##[error]Process completed with exit code 101.
LOG

code_output="$(run_recover "$code_failure_dir/run.json" "$code_failure_dir/logs")"
assert_contains "$code_output" "classification=unknown_failure"
assert_contains "$code_output" "planned_action=none"

second_attempt_dir="$tmp_root/second-attempt"
mkdir -p "$second_attempt_dir/logs"
write_run_json \
  "$second_attempt_dir/run.json" \
  "pull_request" \
  "failure" \
  "completed" \
  "feature/preempted-again" \
  2
cat > "$second_attempt_dir/logs/101.log" <<'LOG'
##[error]Process completed with exit code 143.
##[error]The runner has received a shutdown signal. This can happen when the runner service is stopped, or a manually started runner is canceled.
LOG

second_output="$(run_recover "$second_attempt_dir/run.json" "$second_attempt_dir/logs")"
assert_contains "$second_output" "classification=runner_preemption"
assert_contains "$second_output" "attempt=2"
assert_contains "$second_output" "planned_action=none_max_attempts_reached"

shutdown_without_143_dir="$tmp_root/shutdown-without-143"
mkdir -p "$shutdown_without_143_dir/logs"
write_run_json \
  "$shutdown_without_143_dir/run.json" \
  "pull_request" \
  "cancelled" \
  "completed" \
  "feature/manual-cancel" \
  1
cat > "$shutdown_without_143_dir/logs/101.log" <<'LOG'
##[error]The runner has received a shutdown signal. This can happen when the runner service is stopped, or a manually started runner is canceled.
LOG

manual_cancel_output="$(run_recover "$shutdown_without_143_dir/run.json" "$shutdown_without_143_dir/logs")"
assert_contains "$manual_cancel_output" "classification=unknown_failure"
assert_contains "$manual_cancel_output" "planned_action=none"

echo "ci_preemption_recover_test: ok"
