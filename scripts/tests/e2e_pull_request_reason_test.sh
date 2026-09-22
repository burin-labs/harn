#!/usr/bin/env bash
# The detector's decision procedure, exercised rather than read.
set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
script="${repo_root}/scripts/ci/e2e_pull_request_reason.sh"
work="$(mktemp -d)"
trap 'rm -rf "${work}"' EXIT

run_case() {
  local name="$1" labelled="$2" touched="$3" files="$4"
  local out="${work}/${name}.out" summary="${work}/${name}.summary" gh_out="${work}/${name}.github"
  : >"${summary}"
  : >"${gh_out}"
  LABELLED="${labelled}" TOUCHED="${touched}" TOUCHED_FILES="${files}" \
    GITHUB_STEP_SUMMARY="${summary}" GITHUB_OUTPUT="${gh_out}" \
    bash "${script}" >"${out}" 2>&1
  printf '%s|%s' "$(grep -o 'should_run=[a-z]*' "${gh_out}" | head -1)" "${summary}"
}

expect_output() {
  local name="$1" expected="$2" got="$3"
  if [[ "${got}" != "${expected}" ]]; then
    printf 'case %s: expected %s, got %s\n' "${name}" "${expected}" "${got}" >&2
    exit 1
  fi
}

# Neither reason: the suite stays opt-in and the decision is still reported.
result="$(run_case neither false false '')"
expect_output neither 'should_run=false' "${result%%|*}"
summary="${result#*|}"
grep -Fq 'NOT running the slow E2E suite' "${summary}" \
  || { echo 'the no-run case did not report its decision' >&2; exit 1; }
grep -Fq 'Watched files in this diff: none.' "${summary}" \
  || { echo 'the no-run case did not say that nothing matched' >&2; exit 1; }

# The label alone, which is the pre-existing opt-in and must keep working.
result="$(run_case labelled true false '')"
expect_output labelled 'should_run=true' "${result%%|*}"
grep -Fq 'label is on this pull request' "${result#*|}" \
  || { echo 'the label case did not name the label as its reason' >&2; exit 1; }

# A watched path with no label, which is the defect this closes.
result="$(run_case touched false true 'crates/harn-serve/src/adapters/acp/core.rs')"
expect_output touched 'should_run=true' "${result%%|*}"
summary="${result#*|}"
grep -Fq 'the diff touches the E2E suite or the ACP adapter' "${summary}" \
  || { echo 'the touched case did not name the path as its reason' >&2; exit 1; }
grep -Fq 'crates/harn-serve/src/adapters/acp/core.rs' "${summary}" \
  || { echo 'the touched case did not list the file it matched' >&2; exit 1; }

# Both reasons: reported as both, not silently collapsed to one.
result="$(run_case both true true 'crates/harn-cli/tests/harn_cli_e2e/acp_server_cli.rs')"
expect_output both 'should_run=true' "${result%%|*}"
summary="${result#*|}"
grep -Fq 'label is on this pull request' "${summary}" \
  || { echo 'the both case dropped the label reason' >&2; exit 1; }
grep -Fq 'the diff touches the E2E suite or the ACP adapter' "${summary}" \
  || { echo 'the both case dropped the path reason' >&2; exit 1; }

echo 'E2E pull-request reason contract passed'
