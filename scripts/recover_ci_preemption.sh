#!/usr/bin/env bash
set -euo pipefail

usage() {
  cat <<'USAGE'
Usage:
  scripts/recover_ci_preemption.sh --repo OWNER/REPO --run-id RUN_ID [--apply]
  scripts/recover_ci_preemption.sh --repo OWNER/REPO --run-id RUN_ID \
    --run-json PATH --workflow PATH [--logs-dir DIR]

Classify a failed GitHub Actions run with the typed Harn recovery policy.

The shell adapter retrieves metadata and raw logs into a private temporary
directory. It never prints log contents. Default mode emits one JSON receipt
without mutating GitHub. With --apply, the adapter executes only the bounded
action selected by policy after every substantive failed job has exact,
retry-safe evidence.
USAGE
}

die() {
  printf 'recover_ci_preemption: %s\n' "$*" >&2
  exit 2
}

repo=""
run_id=""
input_run_json=""
input_logs_dir=""
input_workflow=""
input_policy=""
apply=false
emit_summary=false
max_attempts=1

while [[ $# -gt 0 ]]; do
  case "$1" in
    --repo)
      repo="${2:-}"
      shift 2
      ;;
    --run-id)
      run_id="${2:-}"
      shift 2
      ;;
    --run-json)
      input_run_json="${2:-}"
      shift 2
      ;;
    --logs-dir)
      input_logs_dir="${2:-}"
      shift 2
      ;;
    --workflow)
      input_workflow="${2:-}"
      shift 2
      ;;
    --policy)
      input_policy="${2:-}"
      shift 2
      ;;
    --apply)
      apply=true
      shift
      ;;
    --emit-summary)
      emit_summary=true
      shift
      ;;
    --max-attempts)
      max_attempts="${2:-}"
      shift 2
      ;;
    -h|--help)
      usage
      exit 0
      ;;
    *)
      die "unknown argument: $1"
      ;;
  esac
done

[[ "$repo" =~ ^[^/[:space:]]+/[^/[:space:]]+$ ]] || die "--repo must be OWNER/REPO"
[[ "$run_id" =~ ^[1-9][0-9]*$ ]] || die "--run-id must be a positive integer"
[[ "$max_attempts" =~ ^[0-9]+$ ]] || die "--max-attempts must be an integer"

repo_root=$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)
policy_source="${input_policy:-$repo_root/.github/ci-preemption-policy.json}"
[[ -f "$policy_source" ]] || die "recovery policy not found: $policy_source"

harn_bin="${HARN_BIN:-}"
if [[ -z "$harn_bin" ]]; then
  harn_bin=$(command -v harn || true)
fi
[[ -x "$harn_bin" ]] || die "HARN_BIN must name an executable Harn binary"

umask 077
tmp_dir=$(mktemp -d)
cleanup() {
  rm -rf "$tmp_dir"
}
trap cleanup EXIT
logs_dir="$tmp_dir/logs"
mkdir -p "$logs_dir"
run_json="$tmp_dir/run.json"
workflow="$tmp_dir/workflow.yml"
policy="$tmp_dir/policy.json"
cp -- "$policy_source" "$policy"

emit_receipt_summary() {
  local receipt="$1"
  if [[ "$emit_summary" != "true" || -z "${GITHUB_STEP_SUMMARY:-}" ]]; then
    return 0
  fi
  {
    printf '## CI recovery\n\n'
    printf -- '- classification: %s\n' "$(jq -r '.classification' <<< "$receipt")"
    printf -- '- failed jobs: %s\n' "$(jq -r '.failed_job_count' <<< "$receipt")"
    printf -- '- retry-safe jobs: %s\n' "$(jq -r '.retry_safe_job_count' <<< "$receipt")"
    printf -- '- terminal failures: %s\n' "$(jq -r '.terminal_failure_job_count' <<< "$receipt")"
    printf -- '- unknown jobs: %s\n' "$(jq -r '.unknown_job_count' <<< "$receipt")"
    printf -- '- evidence complete: %s\n' "$(jq -r '.evidence_complete' <<< "$receipt")"
    jq -r '.jobs[] | "- job: " + (.job_name | @json) + " (" + .kind + ")"' <<< "$receipt"
    printf -- '- action: %s\n' "$(jq -r '.planned_action' <<< "$receipt")"
    jq -r '.run_url | select(length > 0) | "- run: " + .' <<< "$receipt"
  } >> "$GITHUB_STEP_SUMMARY"
}

metadata_unavailable() {
  local receipt
  receipt=$(jq -cn \
    --argjson run_id "$run_id" \
    '{
      schema: "harn.ci_preemption_recovery.v2",
      run_id: $run_id,
      classification: "metadata_unavailable",
      workflow: "",
      event: "",
      conclusion: "",
      attempt: 0,
      head_branch: "",
      failed_job_count: 0,
      inspected_job_count: 0,
      retry_safe_job_count: 0,
      terminal_failure_job_count: 0,
      unknown_job_count: 0,
      evidence_complete: false,
      jobs: [],
      planned_action: "none",
      run_url: ""
    }')
  printf '%s\n' "$receipt"
  emit_receipt_summary "$receipt"
}

if [[ -n "$input_run_json" ]]; then
  [[ -f "$input_run_json" ]] || die "run JSON not found: $input_run_json"
  cp -- "$input_run_json" "$run_json"
else
  if ! gh run view "$run_id" \
    --repo "$repo" \
    --json databaseId,event,conclusion,status,headBranch,headSha,url,attempt,workflowName,jobs \
    > "$run_json" 2> "$tmp_dir/run-metadata.err"; then
    metadata_unavailable
    exit 0
  fi
fi

if ! jq -e --argjson expected "$run_id" '.databaseId == $expected' "$run_json" >/dev/null; then
  die "run metadata does not match --run-id"
fi

if [[ -n "$input_workflow" ]]; then
  [[ -f "$input_workflow" ]] || die "workflow not found: $input_workflow"
  cp -- "$input_workflow" "$workflow"
else
  workflow_path=$(gh api "/repos/$repo/actions/runs/$run_id" --jq '.path' \
    2> "$tmp_dir/workflow-metadata.err" || true)
  workflow_path="${workflow_path%@*}"
  if [[ ! "$workflow_path" =~ ^\.github/workflows/[^/]+\.(yml|yaml)$ ]] \
    || [[ ! -f "$repo_root/$workflow_path" ]]; then
    metadata_unavailable
    exit 0
  fi
  cp -- "$repo_root/$workflow_path" "$workflow"
fi

while IFS= read -r job_id; do
  [[ "$job_id" =~ ^[1-9][0-9]*$ ]] || continue
  destination="$logs_dir/$job_id.log"
  if [[ -n "$input_logs_dir" ]]; then
    if [[ -f "$input_logs_dir/$job_id.log" ]]; then
      cp -- "$input_logs_dir/$job_id.log" "$destination"
    elif [[ -f "$input_logs_dir/$job_id.txt" ]]; then
      cp -- "$input_logs_dir/$job_id.txt" "$destination"
    fi
  else
    candidate="$tmp_dir/$job_id.download"
    if gh api --allow-escape-sequences \
      "/repos/$repo/actions/jobs/$job_id/logs" \
      > "$candidate" 2> "$tmp_dir/$job_id.err"; then
      mv -- "$candidate" "$destination"
    else
      rm -f -- "$candidate"
    fi
  fi
done < <(
  jq -r '
    .jobs[]
    | select(.conclusion == "failure" or .conclusion == "cancelled")
    | .databaseId
  ' "$run_json"
)

receipt=$(
  "$harn_bin" run \
    --read-only-root "$tmp_dir" \
    "$repo_root/scripts/ci_preemption_policy.harn" \
    -- \
    --run-json "$run_json" \
    --workflow "$workflow" \
    --logs-dir "$logs_dir" \
    --policy "$policy" \
    --max-attempts "$max_attempts"
)

if ! jq -e '
  .schema == "harn.ci_preemption_recovery.v2"
  and (.run_id | type == "number")
  and (.classification | type == "string")
  and (.planned_action | type == "string")
  and (.jobs | type == "array")
' <<< "$receipt" >/dev/null; then
  die "Harn recovery policy returned an invalid receipt"
fi

printf '%s\n' "$(jq -c . <<< "$receipt")"
emit_receipt_summary "$receipt"

if [[ "$apply" != "true" ]]; then
  exit 0
fi

planned_action=$(jq -r '.planned_action' <<< "$receipt")
case "$planned_action" in
  rerun_failed_jobs)
    gh run rerun "$(jq -r '.run_id' <<< "$receipt")" --repo "$repo" --failed
    ;;
  requeue_merge_queue)
    pr_number=$(jq -r '.pr_number // empty' <<< "$receipt")
    [[ "$pr_number" =~ ^[1-9][0-9]*$ ]] || die "policy selected requeue without a PR number"
    gh pr merge "$pr_number" --repo "$repo" --auto --squash
    ;;
  none|none_max_attempts_reached|manual_merge_group_recovery|manual_recovery)
    ;;
  *)
    die "policy selected unsupported action: $planned_action"
    ;;
esac
