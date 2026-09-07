#!/usr/bin/env bash
# The measurement floor and the discovery threshold must stay two numbers.
#
# They were one. That made a budgeted frame which shrank past the number leave
# the census, and an absent budgeted file is refused, so making a frame smaller
# could not go green. This asserts the collector measures strictly below the
# budget's discovery threshold, and below every budget it has to keep visible,
# so re-unifying them fails here rather than on somebody's pull request.
set -euo pipefail

repo_root=$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)
collector="$repo_root/scripts/ci/collect_stack_frames.sh"
budget="$repo_root/scripts/stack-frame-budget.json"

floor=$(sed -n 's/^threshold="\${2:-\([0-9]*\)}"$/\1/p' "$collector")
if [[ -z "$floor" ]]; then
  echo "FAIL - could not read the default measurement floor from $collector" >&2
  exit 1
fi

discovery=$(jq -r '.threshold_bytes' "$budget")
smallest=$(jq -r '[.files[].max_bytes] | min' "$budget")
if [[ -z "$discovery" || "$discovery" == "null" || -z "$smallest" || "$smallest" == "null" ]]; then
  echo "FAIL - could not read the discovery threshold or the banked budgets from $budget" >&2
  exit 1
fi

failures=0
report() {
  if [[ "$2" == "ok" ]]; then printf 'ok - %s\n' "$1"; else printf 'FAIL - %s\n' "$1"; failures=$((failures + 1)); fi
}

if (( floor < discovery )); then
  report "the measurement floor ($floor) is below the discovery threshold ($discovery)" ok
else
  report "the measurement floor ($floor) is below the discovery threshold ($discovery)" bad
fi

if (( floor < smallest )); then
  report "the measurement floor ($floor) is below the smallest banked budget ($smallest)" ok
else
  report "the measurement floor ($floor) is below the smallest banked budget ($smallest)" bad
fi

# The floor is a floor, not zero. A census of every frame in the workspace is
# not a measurement anyone can read, and the collector's own non-null control
# would stop meaning anything.
if (( floor > 0 )); then
  report "the measurement floor ($floor) is a floor, not zero" ok
else
  report "the measurement floor ($floor) is a floor, not zero" bad
fi

printf '\n%d failed\n' "$failures"
[[ "$failures" -eq 0 ]]
