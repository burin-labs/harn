#!/usr/bin/env bash
set -euo pipefail

usage() {
  cat <<'USAGE'
Usage:
  scripts/recover_ci_preemption.sh --repo OWNER/REPO --run-id RUN_ID [--apply]
  scripts/recover_ci_preemption.sh --repo OWNER/REPO --run-id RUN_ID --run-json PATH --logs-dir DIR

Classifies GitHub Actions failures caused by hosted-runner shutdown preemption.

Default mode is a dry run: it prints the classification and the exact recovery
command. With --apply, it executes only first-attempt runner shutdown recovery:
  - pull_request runs: rerun failed jobs
  - merge_group runs: re-arm the parsed PR's merge queue auto-merge

The classifier is intentionally strict. It requires both the GitHub runner
shutdown diagnostic and exit code 143 in a failed/cancelled job log.
USAGE
}

die() {
  printf 'recover_ci_preemption: %s\n' "$*" >&2
  exit 2
}

repo=""
run_id=""
run_json=""
logs_dir=""
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
      run_json="${2:-}"
      shift 2
      ;;
    --logs-dir)
      logs_dir="${2:-}"
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

if [[ -z "$run_json" ]]; then
  [[ -n "$repo" ]] || die "--repo is required when --run-json is omitted"
  [[ -n "$run_id" ]] || die "--run-id is required when --run-json is omitted"
fi

[[ "$max_attempts" =~ ^[0-9]+$ ]] || die "--max-attempts must be an integer"

tmp_dir="$(mktemp -d)"
cleanup() {
  rm -rf "$tmp_dir"
}
trap cleanup EXIT

if [[ -z "$run_json" ]]; then
  run_json="$tmp_dir/run.json"
  if ! gh run view "$run_id" \
    --repo "$repo" \
    --json databaseId,event,conclusion,status,headBranch,headSha,url,attempt,workflowName,jobs \
    > "$run_json"; then
    # This controller is recovery machinery, not another required proof. If
    # GitHub's own API is unavailable, no safe classification or mutation can
    # be made; report that fact and leave the original failed run authoritative
    # instead of creating a second red workflow for every API 5xx.
    printf 'classification=metadata_unavailable\n'
    printf 'planned_action=none\n'
    printf 'run_id=%s\n' "$run_id"
    echo "::warning title=CI preemption repair::GitHub run metadata is unavailable; no recovery action was attempted for run ${run_id}" >&2
    if [[ "$emit_summary" == "true" && -n "${GITHUB_STEP_SUMMARY:-}" ]]; then
      {
        printf '## CI Preemption Repair\n\n'
        printf -- "- classification: \`metadata_unavailable\`\n"
        printf -- "- action: \`none\`\n"
        printf -- "- run id: \`%s\`\n" "$run_id"
      } >> "$GITHUB_STEP_SUMMARY"
    fi
    exit 0
  fi
fi

[[ -f "$run_json" ]] || die "run JSON not found: $run_json"

json_string() {
  jq -r "$1 // \"\"" "$run_json"
}

json_number() {
  jq -r "$1 // 0" "$run_json"
}

run_id="${run_id:-$(json_number '.databaseId')}"
event="$(json_string '.event')"
conclusion="$(json_string '.conclusion')"
status="$(json_string '.status')"
head_branch="$(json_string '.headBranch')"
attempt="$(json_number '.attempt')"
workflow_name="$(json_string '.workflowName')"
run_url="$(json_string '.url')"

ensure_job_log() {
  local job_id="$1"
  local log_path=""

  if [[ -n "$logs_dir" ]]; then
    for candidate in "$logs_dir/$job_id.log" "$logs_dir/$job_id.txt"; do
      if [[ -f "$candidate" ]]; then
        printf '%s\n' "$candidate"
        return 0
      fi
    done
    return 1
  fi

  [[ -n "$repo" ]] || return 1
  log_path="$tmp_dir/$job_id.log"
  if gh api "/repos/$repo/actions/jobs/$job_id/logs" > "$log_path"; then
    printf '%s\n' "$log_path"
    return 0
  fi
  return 1
}

log_has_runner_preemption_signature() {
  local log_path="$1"
  grep -Eqi 'runner has received a shutdown signal|runner service is stopped' "$log_path" \
    && grep -Eqi 'exit code 143' "$log_path"
}

pr_number=""
if [[ "$head_branch" =~ (^|/)pr-([0-9]+)- ]]; then
  pr_number="${BASH_REMATCH[2]}"
fi

classification="unknown_failure"
preempted_jobs=""
inspected_jobs=0

if [[ "$status" != "completed" ]]; then
  classification="run_not_completed"
elif [[ "$conclusion" != "failure" && "$conclusion" != "cancelled" ]]; then
  classification="run_not_failed"
else
  while IFS=$'\t' read -r job_id job_name job_conclusion; do
    [[ -n "$job_id" ]] || continue
    inspected_jobs=$((inspected_jobs + 1))
    if log_path="$(ensure_job_log "$job_id")" \
      && log_has_runner_preemption_signature "$log_path"; then
      classification="runner_preemption"
      if [[ -n "$preempted_jobs" ]]; then
        preempted_jobs="$preempted_jobs, "
      fi
      preempted_jobs="${preempted_jobs}${job_name} (${job_id}, ${job_conclusion})"
    fi
  done < <(
    jq -r '
      .jobs[]
      | select((.conclusion == "failure" or .conclusion == "cancelled") and .name != "CI status")
      | [.databaseId, .name, .conclusion]
      | @tsv
    ' "$run_json"
  )

  if [[ "$classification" != "runner_preemption" && "$inspected_jobs" -eq 0 ]]; then
    classification="no_failed_jobs"
  fi
fi

planned_action="none"
planned_command=""
declare -a command=()

if [[ "$classification" == "runner_preemption" ]]; then
  if [[ "$attempt" -gt "$max_attempts" ]]; then
    planned_action="none_max_attempts_reached"
  elif [[ "$event" == "pull_request" ]]; then
    planned_action="rerun_failed_jobs"
    planned_command="gh run rerun $run_id --repo $repo --failed"
    command=(gh run rerun "$run_id" --repo "$repo" --failed)
  elif [[ "$event" == "merge_group" && -n "$pr_number" ]]; then
    planned_action="requeue_merge_queue"
    planned_command="gh pr merge $pr_number --repo $repo --auto --squash"
    command=(gh pr merge "$pr_number" --repo "$repo" --auto --squash)
  elif [[ "$event" == "merge_group" ]]; then
    planned_action="manual_merge_group_recovery"
  else
    planned_action="manual_recovery"
  fi
fi

printf 'classification=%s\n' "$classification"
printf 'workflow=%s\n' "$workflow_name"
printf 'event=%s\n' "$event"
printf 'conclusion=%s\n' "$conclusion"
printf 'attempt=%s\n' "$attempt"
printf 'head_branch=%s\n' "$head_branch"
printf 'pr_number=%s\n' "$pr_number"
printf 'preempted_jobs=%s\n' "$preempted_jobs"
printf 'planned_action=%s\n' "$planned_action"
printf 'planned_command=%s\n' "$planned_command"
printf 'run_url=%s\n' "$run_url"

if [[ "$emit_summary" == "true" && -n "${GITHUB_STEP_SUMMARY:-}" ]]; then
  {
    printf '## CI Preemption Repair\n\n'
    printf -- "- classification: \`%s\`\n" "$classification"
    printf -- "- workflow: \`%s\`\n" "$workflow_name"
    printf -- "- event: \`%s\`\n" "$event"
    printf -- "- attempt: \`%s\`\n" "$attempt"
    printf -- "- action: \`%s\`\n" "$planned_action"
    if [[ -n "$preempted_jobs" ]]; then
      printf -- "- preempted jobs: \`%s\`\n" "$preempted_jobs"
    fi
    if [[ -n "$planned_command" ]]; then
      printf -- "- command: \`%s\`\n" "$planned_command"
    fi
    if [[ -n "$run_url" ]]; then
      printf -- '- run: %s\n' "$run_url"
    fi
  } >> "$GITHUB_STEP_SUMMARY"
fi

if [[ "$apply" == "true" && "${#command[@]}" -gt 0 ]]; then
  printf 'applying: %s\n' "$planned_command"
  "${command[@]}"
elif [[ "$apply" == "true" && "$classification" == "runner_preemption" ]]; then
  printf '::warning title=CI preemption repair::runner preemption classified, but no automatic action is safe for event=%s head_branch=%s attempt=%s\n' \
    "$event" "$head_branch" "$attempt"
fi
