#!/usr/bin/env bash
set -euo pipefail

repo_root=$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)
proof_script="$repo_root/scripts/ci_merge_group_proof.sh"

tmp_root=$(mktemp -d)
trap 'rm -rf "$tmp_root"' EXIT

fake_curl="$tmp_root/curl"
cat > "$fake_curl" <<'SH'
#!/usr/bin/env bash
set -euo pipefail
url=${!#}
request=runs
if [[ "$url" == */jobs ]]; then
  request=jobs
fi
if [[ "${FAKE_CURL_FAIL:-}" == "$request" ]]; then
  exit 22
fi
if [[ "$request" == "jobs" ]]; then
  cat "${FAKE_CURL_JOBS_RESPONSE:?FAKE_CURL_JOBS_RESPONSE is required}"
else
  cat "${FAKE_CURL_RUNS_RESPONSE:?FAKE_CURL_RUNS_RESPONSE is required}"
fi
SH
chmod +x "$fake_curl"

sha=79ba0e28d76f018f5001cfd8a579d7c87c0cb6f9

run_proof() {
  GITHUB_TOKEN=test-token \
    CURL_BIN="$fake_curl" \
    FAKE_CURL_RUNS_RESPONSE="$1" \
    FAKE_CURL_JOBS_RESPONSE="$2" \
    FAKE_CURL_FAIL="${3:-}" \
    "$proof_script" burin-labs/harn ci.yml "$sha" 2>/dev/null
}

write_response() {
  local path=$1
  local runs=$2
  printf '{"total_count":1,"workflow_runs":%s}\n' "$runs" > "$path"
}

successful_jobs="$tmp_root/successful-jobs.json"
printf '%s\n' '{"total_count":8,"jobs":[{"name":"Format check","status":"completed","conclusion":"success"},{"name":"Package audit","status":"completed","conclusion":"success"},{"name":"Rust lint","status":"completed","conclusion":"success"},{"name":"Rust test","status":"completed","conclusion":"success"},{"name":"Rust security proof","status":"completed","conclusion":"success"},{"name":"Harn conformance + audit","status":"completed","conclusion":"success"},{"name":"Audit scripts","status":"completed","conclusion":"success"},{"name":"Windows cross-compile check","status":"completed","conclusion":"success"}]}' > "$successful_jobs"

success_response="$tmp_root/success.json"
write_response "$success_response" "[{\"id\":123,\"head_sha\":\"$sha\",\"path\":\".github/workflows/ci.yml\",\"event\":\"merge_group\",\"status\":\"completed\",\"conclusion\":\"success\"}]"
[[ "$(run_proof "$success_response" "$successful_jobs")" == "true" ]] \
  || { echo "exact successful merge-group proof was not accepted" >&2; exit 1; }

missing_harn_jobs="$tmp_root/missing-harn-jobs.json"
jq 'del(.jobs[] | select(.name == "Harn conformance + audit")) | .total_count = 7' \
  "$successful_jobs" > "$missing_harn_jobs"
[[ "$(run_proof "$success_response" "$missing_harn_jobs")" == "false" ]] \
  || { echo "merge-group proof accepted missing Harn authority" >&2; exit 1; }

invalid_contract="$tmp_root/invalid-contract.json"
printf '%s\n' '{}' > "$invalid_contract"
[[ "$(RELEASE_AUDIT_CONTRACT_PATH="$invalid_contract" run_proof "$success_response" "$successful_jobs")" == "false" ]] \
  || { echo "merge-group proof accepted an invalid owning contract" >&2; exit 1; }

pruned_jobs="$tmp_root/pruned-jobs.json"
printf '%s\n' '{"total_count":2,"jobs":[{"name":"Format check","status":"completed","conclusion":"success"},{"name":"Windows cross-compile check","status":"completed","conclusion":"success"}]}' > "$pruned_jobs"
[[ "$(run_proof "$success_response" "$pruned_jobs")" == "false" ]] \
  || { echo "successful workflow with pruned heavy lanes was accepted" >&2; exit 1; }

empty_response="$tmp_root/empty.json"
write_response "$empty_response" '[]'
[[ "$(run_proof "$empty_response" "$successful_jobs")" == "false" ]] \
  || { echo "empty proof response did not fail closed" >&2; exit 1; }

mismatch_response="$tmp_root/mismatch.json"
write_response "$mismatch_response" '[{"head_sha":"0000000000000000000000000000000000000000","path":".github/workflows/ci.yml","event":"merge_group","status":"completed","conclusion":"success"}]'
[[ "$(run_proof "$mismatch_response" "$successful_jobs")" == "false" ]] \
  || { echo "mismatched SHA did not fail closed" >&2; exit 1; }

wrong_workflow_response="$tmp_root/wrong-workflow.json"
write_response "$wrong_workflow_response" "[{\"head_sha\":\"$sha\",\"path\":\".github/workflows/release.yml\",\"event\":\"merge_group\",\"status\":\"completed\",\"conclusion\":\"success\"}]"
[[ "$(run_proof "$wrong_workflow_response" "$successful_jobs")" == "false" ]] \
  || { echo "different workflow proof did not fail closed" >&2; exit 1; }

malformed_response="$tmp_root/malformed.json"
printf '{"workflow_runs":{}}\n' > "$malformed_response"
[[ "$(run_proof "$malformed_response" "$successful_jobs")" == "false" ]] \
  || { echo "malformed API response did not fail closed" >&2; exit 1; }

[[ "$(run_proof "$success_response" "$successful_jobs" runs)" == "false" ]] \
  || { echo "workflow-runs HTTP failure did not fail closed" >&2; exit 1; }
[[ "$(run_proof "$success_response" "$successful_jobs" jobs)" == "false" ]] \
  || { echo "jobs HTTP failure did not fail closed" >&2; exit 1; }

invalid_sha_result=$(GITHUB_TOKEN=test-token "$proof_script" burin-labs/harn ci.yml invalid 2>/dev/null)
[[ "$invalid_sha_result" == "false" ]] \
  || { echo "invalid SHA did not fail closed" >&2; exit 1; }

echo "ci_merge_group_proof_test: ok"
