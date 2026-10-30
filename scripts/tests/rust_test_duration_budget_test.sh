#!/usr/bin/env bash
set -euo pipefail
root="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
source "$root/scripts/ci/run_rust_test_lane.sh"

normal=$(report_duration_budget 3599 75 7168)
boundary=$(report_duration_budget 3600 75 7168)
slow=$(report_duration_budget 3601 75 7168)
large=$(report_duration_budget 3601 75 14336)
owned=$(report_duration_budget 2161 45 24576)
missing=$(report_duration_budget 3601 75 unmeasured)
invalid=$(report_duration_budget -1 75 7168)
feedback_boundary=$(report_duration_budget 1200 75 7168 1200)
feedback_slow=$(report_duration_budget 1201 75 7168 1200)
invalid_feedback=$(report_duration_budget 1 75 7168 0)
unreachable_feedback=$(report_duration_budget 1 75 7168 4501)

[[ "$normal" != *'::warning::'* && "$boundary" != *'::warning::'* ]]
[[ "$slow" == *'::warning::'* && "$slow" == *'3601s of 4500s'* ]]
[[ "$slow" == *'80%'* && "$slow" == *'7168 MiB'* ]]
[[ "$large" == *'14336 MiB'* && "$owned" == *'2161s of 2700s'* ]]
[[ "$missing" == *'RUST_TEST_DURATION_UNMEASURED'* ]]
[[ "$invalid" == *'RUST_TEST_DURATION_UNMEASURED'* ]]
[[ "$feedback_boundary" != *'::warning::'* ]]
[[ "$feedback_slow" == *'::warning::'* && "$feedback_slow" == *'feedback budget 1200s'* ]]
[[ "$invalid_feedback" == *'RUST_TEST_DURATION_UNMEASURED'* ]]
[[ "$unreachable_feedback" == *'RUST_TEST_DURATION_UNMEASURED'* ]]

# The reporter's budget is a checked projection of the actual job fuse.
workflow="$root/.github/workflows/macos-nightly.yml"
job_limit=$(sed -n 's/^    timeout-minutes: //p' "$workflow")
reported_limit=$(sed -n 's/^          RUST_TEST_JOB_LIMIT_MINUTES: //p' "$workflow")
[[ -n "$job_limit" && "$job_limit" == "$reported_limit" ]]
echo 'rust_test_duration_budget_test: ok'
