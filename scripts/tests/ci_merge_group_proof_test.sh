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
    "$proof_script" burin-labs/harn ci.yml "$sha" "${@:4}" 2>/dev/null
}

write_response() {
  local path=$1
  local runs=$2
  printf '{"total_count":1,"workflow_runs":%s}\n' "$runs" > "$path"
}

successful_jobs="$tmp_root/successful-jobs.json"
printf '%s\n' '{"total_count":20,"jobs":[{"name":"Format check","status":"completed","conclusion":"success"},{"name":"Verify publishable crates","status":"completed","conclusion":"success"},{"name":"Check Rust code","status":"completed","conclusion":"success"},{"name":"Check Rust code (lean LSP features)","status":"completed","conclusion":"success"},{"name":"Check Rust code (freshness checker)","status":"completed","conclusion":"success"},{"name":"Rust workspace tests","status":"completed","conclusion":"success"},{"name":"Rust test","status":"completed","conclusion":"success"},{"name":"Run Linux sandbox tests","status":"completed","conclusion":"success"},{"name":"Run Harn conformance tests (1/4)","status":"completed","conclusion":"success"},{"name":"Run Harn conformance tests (2/4)","status":"completed","conclusion":"success"},{"name":"Run Harn conformance tests (3/4)","status":"completed","conclusion":"success"},{"name":"Run Harn conformance tests (4/4)","status":"completed","conclusion":"success"},{"name":"Check Harn documentation","status":"completed","conclusion":"success"},{"name":"Run Harn script tests","status":"completed","conclusion":"success"},{"name":"Check Harn sources and generated files","status":"completed","conclusion":"success"},{"name":"Build shared Harn CLI","status":"completed","conclusion":"success"},{"name":"Check repository policies","status":"completed","conclusion":"skipped"},{"name":"Check repository shell gates","status":"completed","conclusion":"success"},{"name":"Windows cross-compile check","status":"completed","conclusion":"success"},{"name":"Write CI timing report","status":"completed","conclusion":"success"}]}' > "$successful_jobs"

success_response="$tmp_root/success.json"
write_response "$success_response" "[{\"id\":123,\"run_attempt\":2,\"head_sha\":\"$sha\",\"path\":\".github/workflows/ci.yml\",\"event\":\"merge_group\",\"status\":\"completed\",\"conclusion\":\"success\"}]"
[[ "$(run_proof "$success_response" "$successful_jobs")" == "true" ]] \
  || { echo "exact successful merge-group proof was not accepted" >&2; exit 1; }

# The push router downloads the proven run's artifacts, so the proof names the
# run and the attempt it read.
run_output="$tmp_root/run-output.txt"
[[ "$(run_proof "$success_response" "$successful_jobs" "" --run-output "$run_output")" == "true" ]] \
  || { echo "proof with a run output was not accepted" >&2; exit 1; }
[[ "$(cat "$run_output")" == $'run_id=123\nrun_attempt=2' ]] \
  || { echo "proof did not name the proven run: $(cat "$run_output")" >&2; exit 1; }
unproven_output="$tmp_root/unproven-output.txt"
[[ "$(run_proof "$success_response" "$tmp_root/no-such-jobs.json" "" --run-output "$unproven_output" || true)" != "true" ]] \
  || { echo "an unreadable jobs response was accepted" >&2; exit 1; }
[[ ! -s "$unproven_output" ]] \
  || { echo "a failed proof still named a run" >&2; exit 1; }

# Repository policies run only on main pushes, so a merge group always skips
# them. Requiring them made this proof false on every push from 2026-09-06.
[[ "$(jq -r '.jobs[] | select(.name == "Check repository policies") | .conclusion' "$successful_jobs")" == "skipped" ]] \
  || { echo "fixture no longer models the skipped main-only policy job" >&2; exit 1; }
no_policy_jobs="$tmp_root/no-policy-jobs.json"
jq 'del(.jobs[] | select(.name == "Check repository policies")) | .total_count = (.jobs | length)' \
  "$successful_jobs" > "$no_policy_jobs"
[[ "$(run_proof "$success_response" "$no_policy_jobs")" == "true" ]] \
  || { echo "a run without the main-only policy job was refused" >&2; exit 1; }

# The push reuses the merge group's shared CLI, so its producer must have passed.
for conclusion in skipped failure; do
  cli_jobs="$tmp_root/cli-${conclusion}-jobs.json"
  jq --arg conclusion "$conclusion" \
    '(.jobs[] | select(.name == "Build shared Harn CLI") | .conclusion) = $conclusion' \
    "$successful_jobs" > "$cli_jobs"
  [[ "$(run_proof "$success_response" "$cli_jobs")" == "false" ]] \
    || { echo "a $conclusion shared CLI producer was accepted" >&2; exit 1; }
done
missing_attempt_response="$tmp_root/missing-attempt.json"
write_response "$missing_attempt_response" "[{\"id\":123,\"head_sha\":\"$sha\",\"path\":\".github/workflows/ci.yml\",\"event\":\"merge_group\",\"status\":\"completed\",\"conclusion\":\"success\"}]"
[[ "$(run_proof "$missing_attempt_response" "$successful_jobs")" == "false" ]] \
  || { echo "a run without an attempt number was accepted" >&2; exit 1; }

native_jobs="$tmp_root/native-jobs.json"
jq '.jobs += [{"name":"Rust on Windows (build + smoke test)","status":"completed","conclusion":"success"}] | .total_count = (.jobs | length)' \
  "$successful_jobs" > "$native_jobs"
[[ "$(run_proof "$success_response" "$native_jobs" "" --require-job "Rust on Windows (build + smoke test)")" == "true" ]] \
  || { echo "exact successful native Windows proof was not accepted" >&2; exit 1; }
[[ "$(run_proof "$success_response" "$successful_jobs" "" --require-job "Rust on Windows (build + smoke test)")" == "false" ]] \
  || { echo "missing native Windows proof did not fail closed" >&2; exit 1; }

for conclusion in skipped failure cancelled; do
  incomplete_native_jobs="$tmp_root/native-${conclusion}-jobs.json"
  jq --arg conclusion "$conclusion" \
    '(.jobs[] | select(.name == "Rust on Windows (build + smoke test)") | .conclusion) = $conclusion' \
    "$native_jobs" > "$incomplete_native_jobs"
  [[ "$(run_proof "$success_response" "$incomplete_native_jobs" "" --require-job "Rust on Windows (build + smoke test)")" == "false" ]] \
    || { echo "$conclusion native Windows proof did not fail closed" >&2; exit 1; }
done

# Package verification does not hold the merge verdict, so the push router may
# accept a run whose package job is still going; nothing else may be pending,
# nothing may have failed, and the strict mode never accepts a running run.
pkg="Verify publishable crates"
running_response="$tmp_root/running.json"
write_response "$running_response" "[{\"id\":124,\"run_attempt\":1,\"head_sha\":\"$sha\",\"path\":\".github/workflows/ci.yml\",\"event\":\"merge_group\",\"status\":\"in_progress\",\"conclusion\":null}]"
pending_pkg_jobs="$tmp_root/pending-pkg-jobs.json"
jq --arg pkg "$pkg" '(.jobs[] | select(.name == $pkg)) |= (.status = "in_progress" | .conclusion = null)' \
  "$successful_jobs" > "$pending_pkg_jobs"
[[ "$(run_proof "$running_response" "$pending_pkg_jobs" "" --allow-pending-job "$pkg")" == "true" ]] \
  || { echo "a run waiting only on package verification was not accepted" >&2; exit 1; }
[[ "$(run_proof "$running_response" "$pending_pkg_jobs")" == "false" ]] \
  || { echo "strict proof accepted a run that is still in progress" >&2; exit 1; }
[[ "$(run_proof "$success_response" "$successful_jobs" "" --allow-pending-job "$pkg")" == "true" ]] \
  || { echo "allowing a pending job rejected an already complete proof" >&2; exit 1; }
pending_other_jobs="$tmp_root/pending-other-jobs.json"
jq '(.jobs[] | select(.name == "Rust workspace tests")) |= (.status = "in_progress" | .conclusion = null)' \
  "$pending_pkg_jobs" > "$pending_other_jobs"
[[ "$(run_proof "$running_response" "$pending_other_jobs" "" --allow-pending-job "$pkg")" == "false" ]] \
  || { echo "a second pending job was accepted" >&2; exit 1; }
failed_pkg_jobs="$tmp_root/failed-pkg-jobs.json"
jq --arg pkg "$pkg" '(.jobs[] | select(.name == $pkg)) |= (.status = "completed" | .conclusion = "failure")' \
  "$successful_jobs" > "$failed_pkg_jobs"
[[ "$(run_proof "$running_response" "$failed_pkg_jobs" "" --allow-pending-job "$pkg")" == "false" ]] \
  || { echo "a failed package verification was accepted as pending" >&2; exit 1; }
failed_extra_jobs="$tmp_root/failed-extra-jobs.json"
jq '.jobs += [{"name":"Check public Rust API","status":"completed","conclusion":"failure"}] | .total_count = (.jobs | length)' \
  "$pending_pkg_jobs" > "$failed_extra_jobs"
[[ "$(run_proof "$running_response" "$failed_extra_jobs" "" --allow-pending-job "$pkg")" == "false" ]] \
  || { echo "a run with a failed job was accepted while another job was pending" >&2; exit 1; }
[[ "$(run_proof "$running_response" "$pending_pkg_jobs" "" --allow-pending-job "$pkg" --allow-pending-job "$pkg" 2>&1 || true)" != "true" ]] \
  || { echo "a repeated --allow-pending-job was accepted" >&2; exit 1; }

missing_harn_jobs="$tmp_root/missing-harn-jobs.json"
jq 'del(.jobs[] | select(.name == "Run Harn conformance tests (2/4)")) | .total_count = (.jobs | length)' \
  "$successful_jobs" > "$missing_harn_jobs"
[[ "$(run_proof "$success_response" "$missing_harn_jobs")" == "false" ]] \
  || { echo "merge-group proof accepted missing Harn authority" >&2; exit 1; }

# Strict Clippy is three matrix legs; one green leg is not a lint proof.
missing_lint_leg_jobs="$tmp_root/missing-lint-leg-jobs.json"
jq 'del(.jobs[] | select(.name == "Check Rust code (lean LSP features)")) | .total_count = (.jobs | length)' \
  "$successful_jobs" > "$missing_lint_leg_jobs"
[[ "$(run_proof "$success_response" "$missing_lint_leg_jobs")" == "false" ]] \
  || { echo "merge-group proof accepted a missing Clippy leg" >&2; exit 1; }

# The hermetic shell gates run before the merge; a run without them proves
# nothing about the scripts they cover.
missing_shell_gates_jobs="$tmp_root/missing-shell-gates-jobs.json"
jq 'del(.jobs[] | select(.name == "Check repository shell gates")) | .total_count = (.jobs | length)' \
  "$successful_jobs" > "$missing_shell_gates_jobs"
[[ "$(run_proof "$success_response" "$missing_shell_gates_jobs")" == "false" ]] \
  || { echo "merge-group proof accepted a run without the shell gates" >&2; exit 1; }

invalid_contract="$tmp_root/invalid-contract.json"
printf '%s\n' '{}' > "$invalid_contract"
[[ "$(RELEASE_AUDIT_CONTRACT_PATH="$invalid_contract" run_proof "$success_response" "$successful_jobs")" == "false" ]] \
  || { echo "merge-group proof accepted an invalid owning contract" >&2; exit 1; }

changed_policy="$tmp_root/changed-policy.json"
jq '.merge_group_jobs += [{"name":"Changed admission policy"}]' \
  "$repo_root/scripts/release_audit_contract.json" > "$changed_policy"
[[ "$(RELEASE_AUDIT_CONTRACT_PATH="$changed_policy" run_proof "$success_response" "$native_jobs" "" --require-job "Rust on Windows (build + smoke test)")" == "false" ]] \
  || { echo "merge-group proof accepted a run missing the changed policy gate" >&2; exit 1; }

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

fork_response="$tmp_root/fork.json"
write_response "$fork_response" "[{\"id\":123,\"run_attempt\":1,\"head_sha\":\"$sha\",\"path\":\".github/workflows/ci.yml\",\"event\":\"pull_request\",\"status\":\"completed\",\"conclusion\":\"success\"}]"
[[ "$(run_proof "$fork_response" "$native_jobs" "" --require-job "Rust on Windows (build + smoke test)")" == "false" ]] \
  || { echo "pull-request/fork proof was accepted as merge-group admission" >&2; exit 1; }

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
