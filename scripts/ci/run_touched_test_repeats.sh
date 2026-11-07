#!/usr/bin/env bash
set -uo pipefail

script_dir=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
repo_root=$(cd "$script_dir/../.." && pwd)
ceiling=${FLAKE_ITERATIONS_CEILING:?}
floor=${FLAKE_ITERATIONS_FLOOR:?}
deadline=$(( ${FLAKE_JOB_START_EPOCH:?} + ${FLAKE_JOB_BUDGET_MINUTES:?} * 60 ))
summary=${GITHUB_STEP_SUMMARY:-/dev/null}
inventory="${RUNNER_TEMP:?}/harn-flake-inventory.json"

{
  echo '## Touched-test repeatability'
  echo "- Filter: \`${NEXTEST_FILTER:?}\`"
  echo "- Scope: workspace default features, flake-detection profile"
  echo "- Budget: ${FLAKE_JOB_BUDGET_MINUTES} min wall-clock from job start"
  echo "- Iteration target: ${floor} (floor) / ${ceiling} (ceiling)"
} >> "$summary"

if ! cargo-nextest nextest list --locked --workspace --profile flake-detection \
    -E "$NEXTEST_FILTER" --message-format json > "$inventory"; then
  echo '::error::Touched-test discovery failed; no repeatability iteration started.'
  echo '- Result: FAILED, test discovery unmeasured.' >> "$summary"
  exit 2
fi
if ! selected=$(HARN_BIN_NO_BUILD=1 "$repo_root/scripts/harn_bin.sh" run --standalone \
    --read-only-root "$inventory" "$script_dir/nextest_inventory.harn" -- "$inventory"); then
  echo '::error::Touched-test inventory is invalid or empty; no repeatability iteration started.'
  echo '- Result: FAILED, selected-test census invalid.' >> "$summary"
  exit 2
fi
echo "- Selected tests per iteration: $selected" >> "$summary"
if [[ -n "${GITHUB_OUTPUT:-}" ]]; then
  echo "inventory=$inventory" >> "$GITHUB_OUTPUT"
fi

completed=0
max_iter_secs=0
while :; do
  now=$(date +%s)
  remaining=$(( deadline - now ))
  if (( remaining <= 0 && completed == 0 )); then
    echo '::error::Touched-test budget expired before its first iteration.'
    echo '- Result: FAILED, no repeatability iteration fit the budget.' >> "$summary"
    exit 1
  fi
  if (( completed > 0 )); then
    est=$max_iter_secs
    ((est >= 1)) || est=1
    if (( completed < floor )); then
      needed=$(( (floor - completed) * est ))
      if (( remaining < needed )); then
        echo "::error::Touched-test repeatability cannot reach its ${floor}-iteration floor within budget (completed ${completed}; slowest ${est}s; ${remaining}s left). Shard the touched-test surface or raise its declared budget."
        echo "- Result: FAILED, floor ${floor} unreachable after ${completed}/${ceiling} iterations." >> "$summary"
        exit 1
      fi
    elif (( remaining < est )); then
      echo "::warning::Touched-test repeatability stopped at ${completed}/${ceiling} iterations within budget; coverage is degraded but above floor ${floor}."
      break
    fi
  fi
  ((completed < ceiling)) || break
  iter=$(( completed + 1 ))
  printf -- '--- Iteration %d/%d (%ds budget left) ---\n' "$iter" "$ceiling" "$remaining"
  start=$(date +%s)
  if cargo-nextest nextest run --locked --workspace --profile flake-detection \
      --test-threads 8 --no-tests fail -E "$NEXTEST_FILTER"; then
    status=0
  else
    status=$?
  fi
  if ((status != 0)); then
    if ((status == 100)); then
      reason='test failure'
    else
      reason="runner/discovery error (exit ${status})"
    fi
    echo "::error::Touched-test ${reason} on iteration ${iter}; repeatability did not pass."
    echo "- Result: FAILED, ${reason} on iteration ${iter}; ${completed}/${ceiling} completed." >> "$summary"
    exit "$status"
  fi
  end=$(date +%s)
  dur=$(( end - start ))
  completed=$(( completed + 1 ))
  ((dur <= max_iter_secs)) || max_iter_secs=$dur
  printf -- 'Iteration %d completed in %ds\n' "$iter" "$dur"
done
{
  echo "- Iterations completed: ${completed}/${ceiling}"
  echo "- Slowest iteration: ${max_iter_secs}s"
  if ((completed >= ceiling)); then
    echo "- Result: PASSED, ${completed}/${ceiling} iterations with no test failure."
  else
    echo "- Result: PASSED (degraded), ${completed}/${ceiling} iterations; floor ${floor} met."
  fi
} >> "$summary"
