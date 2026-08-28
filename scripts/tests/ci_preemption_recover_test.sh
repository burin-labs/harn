#!/usr/bin/env bash
set -euo pipefail

repo_root=$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)
recover_script="$repo_root/scripts/recover_ci_preemption.sh"

tmp_root=$(mktemp -d)
trap 'rm -rf "$tmp_root"' EXIT

harn_bin="${HARN_BIN:-$(command -v harn || true)}"
[[ -x "$harn_bin" ]]

workflow="$tmp_root/ci.yml"
cat > "$workflow" <<'YAML'
name: CI
jobs:
  tests:
    name: Tests
    timeout-minutes: ${{ (github.event_name == 'pull_request') && 20 || 40 }}
  lint:
    name: Lint
    timeout-minutes: 10
  status:
    name: CI status
YAML

write_run_json() {
  local path="$1"
  local attempt="$2"
  local test_conclusion="$3"
  local completed_at="$4"
  cat > "$path" <<JSON
{
  "databaseId": 7001,
  "event": "pull_request",
  "conclusion": "failure",
  "status": "completed",
  "headBranch": "feature/ci-recovery",
  "headSha": "0123456789abcdef",
  "url": "https://example.test/actions/runs/7001",
  "attempt": $attempt,
  "workflowName": "CI",
  "jobs": [
    {
      "databaseId": 101,
      "name": "Tests",
      "status": "completed",
      "conclusion": "$test_conclusion",
      "startedAt": "2026-08-28T18:03:30Z",
      "completedAt": "$completed_at",
      "steps": []
    },
    {
      "databaseId": 199,
      "name": "CI status",
      "status": "completed",
      "conclusion": "failure",
      "startedAt": "$completed_at",
      "completedAt": "$completed_at",
      "steps": []
    }
  ]
}
JSON
}

assert_receipt() {
  local receipt="$1"
  local expression="$2"
  if ! jq -e "$expression" <<< "$receipt" >/dev/null; then
    jq . <<< "$receipt" >&2
    exit 1
  fi
}

transport_dir="$tmp_root/transport"
mkdir -p "$transport_dir/bin"
write_run_json "$transport_dir/run.json" 1 cancelled "2026-08-28T18:23:48Z"
cat > "$transport_dir/bin/gh" <<'SH'
#!/usr/bin/env bash
set -euo pipefail

if [[ "${1:-}" == "api" ]]; then
  allowed=false
  endpoint=""
  for arg in "$@"; do
    [[ "$arg" == "--allow-escape-sequences" ]] && allowed=true
    [[ "$arg" == /repos/*/actions/jobs/*/logs ]] && endpoint="$arg"
  done
  [[ "$allowed" == "true" ]]
  case "$endpoint" in
    */101/logs)
      printf '\033[31m##[error]The operation was canceled.\033[0m\n'
      ;;
    */199/logs)
      printf 'aggregate failure\n'
      ;;
    *)
      exit 1
      ;;
  esac
  exit 0
fi

if [[ "${1:-}" == "run" && "${2:-}" == "rerun" ]]; then
  printf '%s\n' "$@" > "$ACTION_RECEIPT"
  exit 0
fi

if [[ "${1:-}" == "pr" && "${2:-}" == "merge" ]]; then
  printf '%s\n' "$@" > "$ACTION_RECEIPT"
  exit 0
fi

exit 1
SH
chmod +x "$transport_dir/bin/gh"

transport_receipt=$(
  PATH="$transport_dir/bin:$PATH" \
    HARN_BIN="$harn_bin" \
    ACTION_RECEIPT="$transport_dir/action" \
    "$recover_script" \
      --repo burin-labs/harn \
      --run-id 7001 \
      --run-json "$transport_dir/run.json" \
      --workflow "$workflow" \
      --apply
)
assert_receipt "$transport_receipt" '
  .classification == "configured_timeout"
  and .failed_job_count == 1
  and .inspected_job_count == 1
  and .retry_safe_job_count == 1
  and .terminal_failure_job_count == 0
  and .unknown_job_count == 0
  and .evidence_complete == true
  and .jobs[0].kind == "configured_timeout"
  and .jobs[0].configured_timeout_minutes == 20
  and .planned_action == "rerun_failed_jobs"
'
cat > "$transport_dir/expected-action" <<'EOF'
run
rerun
7001
--repo
burin-labs/harn
--failed
EOF
cmp "$transport_dir/expected-action" "$transport_dir/action"

merge_dir="$tmp_root/merge-group"
mkdir -p "$merge_dir/logs"
jq '
  .event = "merge_group"
  | .headBranch = "gh-readonly-queue/main/pr-4113-0123456789abcdef"
' "$transport_dir/run.json" > "$merge_dir/run.json"
printf '##[error]The operation was canceled.\n' > "$merge_dir/logs/101.log"
merge_receipt=$(
  PATH="$transport_dir/bin:$PATH" \
    HARN_BIN="$harn_bin" \
    ACTION_RECEIPT="$merge_dir/action" \
    "$recover_script" \
      --repo burin-labs/harn \
      --run-id 7001 \
      --run-json "$merge_dir/run.json" \
      --workflow "$workflow" \
      --logs-dir "$merge_dir/logs" \
      --apply
)
assert_receipt "$merge_receipt" '
  .classification == "configured_timeout"
  and .pr_number == 4113
  and .planned_action == "requeue_merge_queue"
'
cat > "$merge_dir/expected-action" <<'EOF'
pr
merge
4113
--repo
burin-labs/harn
--auto
--squash
EOF
cmp "$merge_dir/expected-action" "$merge_dir/action"

failure_dir="$tmp_root/failure"
mkdir -p "$failure_dir/logs"
write_run_json "$failure_dir/run.json" 1 failure "2026-08-28T18:04:00Z"
printf 'Process completed with exit code 101.\n' > "$failure_dir/logs/101.log"
failure_receipt=$(
  HARN_BIN="$harn_bin" "$recover_script" \
    --repo burin-labs/harn \
    --run-id 7001 \
    --run-json "$failure_dir/run.json" \
    --workflow "$workflow" \
    --logs-dir "$failure_dir/logs"
)
assert_receipt "$failure_receipt" '
  .classification == "job_failure"
  and .jobs[0].kind == "job_failure"
  and .retry_safe_job_count == 0
  and .terminal_failure_job_count == 1
  and .unknown_job_count == 0
  and .planned_action == "none"
'

manual_dir="$tmp_root/manual-cancel"
mkdir -p "$manual_dir/logs"
write_run_json "$manual_dir/run.json" 1 cancelled "2026-08-28T18:13:30Z"
printf '##[error]The operation was canceled.\n' > "$manual_dir/logs/101.log"
manual_receipt=$(
  HARN_BIN="$harn_bin" "$recover_script" \
    --repo burin-labs/harn \
    --run-id 7001 \
    --run-json "$manual_dir/run.json" \
    --workflow "$workflow" \
    --logs-dir "$manual_dir/logs"
)
assert_receipt "$manual_receipt" '
  .classification == "cancellation_unknown"
  and .jobs[0].configured_timeout_minutes == null
  and .planned_action == "none"
'

missing_dir="$tmp_root/missing"
mkdir -p "$missing_dir/logs"
write_run_json "$missing_dir/run.json" 1 cancelled "2026-08-28T18:23:48Z"
missing_receipt=$(
  HARN_BIN="$harn_bin" "$recover_script" \
    --repo burin-labs/harn \
    --run-id 7001 \
    --run-json "$missing_dir/run.json" \
    --workflow "$workflow" \
    --logs-dir "$missing_dir/logs"
)
assert_receipt "$missing_receipt" '
  .classification == "evidence_unavailable"
  and .inspected_job_count == 0
  and .unknown_job_count == 1
  and .evidence_complete == false
  and .planned_action == "none"
'

second_dir="$tmp_root/second-attempt"
mkdir -p "$second_dir/logs"
write_run_json "$second_dir/run.json" 2 cancelled "2026-08-28T18:23:48Z"
printf '##[error]The operation was canceled.\n' > "$second_dir/logs/101.log"
second_receipt=$(
  HARN_BIN="$harn_bin" "$recover_script" \
    --repo burin-labs/harn \
    --run-id 7001 \
    --run-json "$second_dir/run.json" \
    --workflow "$workflow" \
    --logs-dir "$second_dir/logs"
)
assert_receipt "$second_receipt" '
  .classification == "configured_timeout"
  and .attempt == 2
  and .planned_action == "none_max_attempts_reached"
'

api_dir="$tmp_root/api-outage"
mkdir -p "$api_dir/bin"
cat > "$api_dir/bin/gh" <<'SH'
#!/usr/bin/env bash
exit 1
SH
chmod +x "$api_dir/bin/gh"
api_receipt=$(
  PATH="$api_dir/bin:$PATH" \
    HARN_BIN="$harn_bin" \
    "$recover_script" \
      --repo burin-labs/harn \
      --run-id 7001
)
assert_receipt "$api_receipt" '
  .schema == "harn.ci_preemption_recovery.v2"
  and .run_id == 7001
  and .classification == "metadata_unavailable"
  and .failed_job_count == 0
  and .evidence_complete == false
  and .jobs == []
  and .planned_action == "none"
'

echo "ci_preemption_recover_test: ok"
