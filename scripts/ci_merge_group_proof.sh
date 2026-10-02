#!/usr/bin/env bash
set -euo pipefail

usage() {
  echo "usage: $0 <owner/repository> <workflow-file> <commit-sha> [--require-job <job-name>] [--allow-pending-job <job-name>]" >&2
}

fail_closed() {
  echo "::notice::merge-group proof unavailable: $1" >&2
  printf 'false\n'
  exit 0
}

if [[ $# -lt 3 ]]; then
  usage
  exit 2
fi

repository=$1
workflow_file=$2
commit_sha=$3
shift 3
required_job=""
# A job the merge verdict does not wait for may still be running in an
# otherwise proven run. Only the push router passes this: it decides whether to
# re-run heavy lanes, and the pending job's check lands on this same commit
# either way. Release certification never does, so its proof stays strict.
pending_job=""
while [[ $# -gt 0 ]]; do
  if [[ $# -lt 2 || -z "$2" || "$2" == *$'\n'* ]]; then
    usage
    exit 2
  fi
  case "$1" in
    --require-job) [[ -z "$required_job" ]] || { usage; exit 2; }; required_job=$2 ;;
    --allow-pending-job) [[ -z "$pending_job" ]] || { usage; exit 2; }; pending_job=$2 ;;
    *) usage; exit 2 ;;
  esac
  shift 2
done

if [[ ! "$repository" =~ ^[A-Za-z0-9_.-]+/[A-Za-z0-9_.-]+$ ]]; then
  fail_closed "invalid repository identifier"
fi
if [[ ! "$workflow_file" =~ ^[A-Za-z0-9_.-]+\.ya?ml$ ]]; then
  fail_closed "invalid workflow filename"
fi
if [[ ! "$commit_sha" =~ ^[0-9a-f]{40}$ ]]; then
  fail_closed "invalid commit SHA"
fi
if [[ -z "${GITHUB_TOKEN:-}" ]]; then
  fail_closed "GITHUB_TOKEN is unset"
fi

api_url=${GITHUB_API_URL:-https://api.github.com}
curl_bin=${CURL_BIN:-curl}
jq_bin=${JQ_BIN:-jq}
repo_root=$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)
contract_path=${RELEASE_AUDIT_CONTRACT_PATH:-$repo_root/scripts/release_audit_contract.json}
tmp_dir=$(mktemp -d)
trap 'rm -rf "$tmp_dir"' EXIT
runs_response="$tmp_dir/runs.json"
run_ids="$tmp_dir/run-ids.txt"

if ! "$jq_bin" -e '
  type == "object"
    and .schema_version == "harn.release_audit_contract.v1"
    and (.merge_group_jobs | type == "array" and length > 0)
    and ([.merge_group_jobs[].name]
      | all(.[]; type == "string" and length > 0)
      and (length == (unique | length)))
' "$contract_path" >/dev/null 2>&1; then
  fail_closed "release-audit contract is absent or invalid"
fi

github_api_get() {
  local output=$1
  shift
  "$curl_bin" --fail-with-body --silent --show-error --location \
    --header "Accept: application/vnd.github+json" \
    --header "Authorization: Bearer ${GITHUB_TOKEN}" \
    --header "X-GitHub-Api-Version: 2022-11-28" \
    "$@" > "$output"
}

if ! github_api_get "$runs_response" --get \
  --data-urlencode "event=merge_group" \
  --data-urlencode "head_sha=${commit_sha}" \
  --data-urlencode "per_page=100" \
  "${api_url}/repos/${repository}/actions/workflows/${workflow_file}/runs"; then
  fail_closed "GitHub Actions API request failed"
fi

if ! "$jq_bin" -e '
  type == "object"
    and (.workflow_runs | type == "array")
' "$runs_response" >/dev/null 2>&1; then
  fail_closed "GitHub Actions API response has an unexpected shape"
fi

workflow_path=".github/workflows/${workflow_file}"
# shellcheck disable=SC2016 # $sha and $path are jq variables, not shell expansions.
"$jq_bin" -r \
  --arg sha "$commit_sha" \
  --arg path "$workflow_path" \
  --arg pending_job "$pending_job" '
    .workflow_runs[]
    | select(
      .head_sha == $sha
        and .path == $path
        and .event == "merge_group"
        and (.id | type == "number")
        and (
          (.status == "completed" and .conclusion == "success")
            or ($pending_job != "" and (.status == "in_progress" or .status == "queued"))
        )
    )
    | .id
  ' "$runs_response" > "$run_ids"

while IFS= read -r run_id; do
  [[ "$run_id" =~ ^[0-9]+$ ]] || continue
  jobs_response="$tmp_dir/jobs-${run_id}.json"
  if ! github_api_get "$jobs_response" --get \
    --data-urlencode "filter=latest" \
    --data-urlencode "per_page=100" \
    "${api_url}/repos/${repository}/actions/runs/${run_id}/jobs"; then
    fail_closed "GitHub Actions jobs API request failed"
  fi

  if ! "$jq_bin" -e '
    type == "object"
      and (.total_count | type == "number")
      and (.jobs | type == "array")
      and (.total_count == (.jobs | length))
  ' "$jobs_response" >/dev/null 2>&1; then
    fail_closed "GitHub Actions jobs response is incomplete or malformed"
  fi

  # A successful workflow conclusion is not sufficient: merge-group docs-only
  # tails intentionally skip the expensive lanes. Reuse proof only when every
  # lane this push plans to prune actually completed successfully for this run.
  # shellcheck disable=SC2016 # $response, $required, and $name are jq variables.
  # A run that is still going is accepted only through --allow-pending-job:
  # nothing may have failed, every other required job has passed, and the
  # pending one has passed or is still queued or running.
  if "$jq_bin" -e --slurpfile contract "$contract_path" \
    --arg required_job "$required_job" --arg pending_job "$pending_job" '
    . as $response
    | (($contract[0].merge_group_jobs | map(.name))
        + ["Check repository policies", "Windows cross-compile check", "Write CI timing report"]
        + (if $required_job == "" then [] else [$required_job] end))
      as $required
    | ([$response.jobs[] | select(.status == "completed"
          and (.conclusion == "failure" or .conclusion == "cancelled"
            or .conclusion == "timed_out"))] | length == 0)
    and all(
        $required[];
        . as $name
        | any(
            $response.jobs[];
            .name == $name
              and (
                (.status == "completed" and .conclusion == "success")
                  or ($pending_job != "" and .name == $pending_job
                    and (.status == "queued" or .status == "in_progress"))
              )
          )
      )
  ' "$jobs_response" >/dev/null; then
    printf 'true\n'
    exit 0
  fi
done < "$run_ids"

printf 'false\n'
