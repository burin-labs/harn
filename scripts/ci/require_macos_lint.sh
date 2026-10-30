#!/usr/bin/env bash
# The historical required check certifies every strict lint graph in this attempt.
set -euo pipefail
: "${GH_REPO:?repository required}"
: "${GITHUB_RUN_ID:?run required}"
: "${GITHUB_RUN_ATTEMPT:?attempt required}"
: "${GITHUB_SHA:?source required}"
[[ "$GITHUB_RUN_ID" =~ ^[0-9]+$ && "$GITHUB_RUN_ATTEMPT" =~ ^[0-9]+$ ]] || exit 1
[[ "$GITHUB_SHA" =~ ^[0-9a-f]{40}$ ]] || exit 1
root=$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)
# shellcheck source=scripts/lib/github_read_retry.sh
source "$root/scripts/lib/github_read_retry.sh"
[[ ${MATRIX_RESULT:-unreported} == success ]] || {
  echo "error: macOS matrix result ${MATRIX_RESULT:-unreported} is not success" >&2
  exit 1
}
legs=$(bash "$root/scripts/ci/run_rust_lint_lane.sh" --list-legs)
expected=$(printf '%s\n' "$legs" | jq -Rsc 'split("\n") | map(select(length > 0))')
jq -e 'length > 0' <<<"$expected" >/dev/null
# Failed-only reruns can publish inherited jobs after their aggregate starts.
# Reconcile whole snapshots of this exact attempt; no older proof is substituted.
deadline=$((SECONDS + 180))
require_budget() {
  if ((SECONDS >= deadline)); then
    echo 'error: macOS metadata reconciliation elapsed budget exhausted' >&2
    return 1
  fi
}
for snapshot in {1..7}; do
  require_budget
  jobs=$(read_api_json \
    "repos/$GH_REPO/actions/runs/$GITHUB_RUN_ID/attempts/$GITHUB_RUN_ATTEMPT/jobs?per_page=100" paginated)
  require_budget
  proof=$(jq -er --argjson expected "$expected" --argjson run "$GITHUB_RUN_ID" \
    --argjson attempt "$GITHUB_RUN_ATTEMPT" --arg source "$GITHUB_SHA" '
  if type != "array" or length == 0 or any(.[]; (.jobs | type) != "array")
  then error("unmeasured macOS job census")
  elif ([.[].total_count] | unique | length) != 1 then error("inconsistent macOS job census")
  else . as $pages | [.[].jobs[]]
    | if length == $pages[0].total_count then . else error("partial macOS job census") end end
  | if any(.[]; .run_id != $run or .run_attempt != $attempt or .head_sha != $source)
    then error("foreign macOS job identity") else . end
  | if any(.[]; (.id | type) != "number" or .id <= 0 or .id != (.id | floor))
      or ([.[].id] | unique | length) != length
    then error("unmeasured or duplicate physical macOS job identity") else . end
  | . as $jobs
  | $expected | map(. as $leg
    | ("Rust on macOS strict lint (" + $leg + ")") as $name
    | [$jobs[] | select(.name == $name)] as $matches
    | if ($matches | length) > 1 then {name:$name,status:"duplicate",conclusion:null,measured:false}
      elif ($matches | length) == 0 then {name:$name,status:"missing",conclusion:null,measured:false}
      else $matches[0] | {name,status,conclusion,measured:true} end)
  | . as $legs
  | {legs:$legs, verdict:(
      if any($legs[]; .status == "duplicate" or (.status == "completed" and .conclusion != "success")) then "refused"
      elif all($legs[]; .status == "completed" and .conclusion == "success") then "passed"
      else "pending" end)}
' <<<"$jobs")
  jq -r --argjson expected "$expected" '
  .legs as $legs
  | "macOS lint census: expected=\($expected | length) observed=\([$legs[] | select(.measured)] | length) pending=\([$legs[] | select(.status != "completed")]|length) bad=\([$legs[] | select(.status == "completed" and .conclusion != "success")]|length)",
    ($legs[] | "\(.name): \(.status)/\(.conclusion // "unreported")")
' <<<"$proof"
  case $(jq -r .verdict <<<"$proof") in
  passed)
    echo 'All macOS strict lint legs succeeded'
    exit 0
    ;;
  refused)
    echo 'error: macOS lint proof failed or duplicated' >&2
    exit 1
    ;;
  esac
  remaining=$((deadline - SECONDS))
  if ((snapshot == 7 || remaining <= 0)); then
    echo 'error: macOS lint proof remained incomplete within its bounded budget' >&2
    exit 1
  fi
  echo "macOS lint metadata pending; snapshot $snapshot/7" >&2
  sleep "$((remaining < 30 ? remaining : 30))"
done
