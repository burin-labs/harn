#!/usr/bin/env bash
set -euo pipefail

usage() {
  echo "usage: $0 <owner/repository> <workflow-file> <commit-sha>" >&2
}

fail_closed() {
  echo "::notice::merge-group proof unavailable: $1" >&2
  printf 'false\n'
  exit 0
}

if [[ $# -ne 3 ]]; then
  usage
  exit 2
fi

repository=$1
workflow_file=$2
commit_sha=$3

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
  .schema_version == "harn.release_audit_contract.v1"
    and (.merge_group_jobs | type == "array" and length > 0)
    and all(.merge_group_jobs[]; .name | type == "string" and length > 0)
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
  --data-urlencode "status=success" \
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
  --arg path "$workflow_path" '
    .workflow_runs[]
    | select(
      .head_sha == $sha
        and .path == $path
        and .event == "merge_group"
        and .status == "completed"
        and .conclusion == "success"
        and (.id | type == "number")
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
  # The release-audit contract owns all reusable audit proof names. Only the
  # two non-audit lanes needed for safe pruning remain local to this boundary.
  # shellcheck disable=SC2016 # $response, $required, and $name are jq variables.
  if "$jq_bin" -e --slurpfile contract "$contract_path" '
    . as $response
    | (($contract[0].merge_group_jobs | map(.name)) + ["Audit scripts", "Windows cross-compile check"])
      as $required
    | all(
        $required[];
        . as $name
        | any(
            $response.jobs[];
            .name == $name
              and .status == "completed"
              and .conclusion == "success"
          )
      )
  ' "$jobs_response" >/dev/null; then
    printf 'true\n'
    exit 0
  fi
done < "$run_ids"

printf 'false\n'
