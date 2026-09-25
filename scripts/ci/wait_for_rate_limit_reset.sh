#!/usr/bin/env bash
# Decide whether a failed API-backed release step may run once more, and wait
# for the token's rate-limit window to reset before it does.
#
# Usage: wait_for_rate_limit_reset.sh <step name>
#
# The caller runs this only after the step failed. The step's own error text is
# not visible to later steps, so the decision reads the token's state instead:
# `GET /rate_limit` does not count against the limit, and an exhausted core
# budget is the rate-limit failure a burst of repository activity causes. Any
# other failure leaves budget behind and is refused here, so it fails the job
# exactly as it did before.
#
# Exit 0: the budget was exhausted and has now reset; run the step again.
# Exit 1: not a rate-limit failure, the reset is further away than the cap, or
#         the budget could not be read.
set -euo pipefail

step="${1:?usage: wait_for_rate_limit_reset.sh <step name>}"
max_wait_seconds="${HARN_RATE_LIMIT_MAX_WAIT_SECONDS:-3900}"
# Below this share of the limit, the budget counts as spent. A step that failed
# for another reason leaves most of the window unused.
exhausted_percent="${HARN_RATE_LIMIT_EXHAUSTED_PERCENT:-5}"

if ! core="$(gh api rate_limit --jq '.resources.core | "\(.limit) \(.remaining) \(.reset)"')"; then
  echo "::error::$step failed and the token's rate limit could not be read; not retrying."
  exit 1
fi
read -r limit remaining reset <<<"$core"
if [[ ! "$limit" =~ ^[0-9]+$ || ! "$remaining" =~ ^[0-9]+$ || ! "$reset" =~ ^[0-9]+$ || "$limit" -eq 0 ]]; then
  echo "::error::$step failed and /rate_limit returned '$core', not a limit, remaining count and reset time; not retrying."
  exit 1
fi

if (( remaining * 100 >= limit * exhausted_percent )); then
  echo "::error::$step failed with $remaining of $limit API requests left, so the failure was not the rate limit; not retrying."
  exit 1
fi

now="$(date +%s)"
wait_seconds=$(( reset - now + 5 ))
(( wait_seconds < 0 )) && wait_seconds=0
if (( wait_seconds > max_wait_seconds )); then
  echo "::error::$step hit the API rate limit ($remaining of $limit left) and the window resets in ${wait_seconds}s, beyond the ${max_wait_seconds}s cap; not retrying."
  exit 1
fi

echo "::warning::$step hit the API rate limit ($remaining of $limit left). Waiting ${wait_seconds}s for the window to reset, then running it once more."
sleep "$wait_seconds"
