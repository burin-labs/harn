#!/usr/bin/env bash
# The historical required check certifies every strict lint graph in this attempt.
set -euo pipefail
: "${GH_REPO:?repository required}"
: "${GITHUB_RUN_ID:?run required}"
: "${GITHUB_RUN_ATTEMPT:?attempt required}"
[[ "$GITHUB_RUN_ID" =~ ^[0-9]+$ && "$GITHUB_RUN_ATTEMPT" =~ ^[0-9]+$ ]] || exit 1
root=$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)
legs=$(bash "$root/scripts/ci/run_rust_lint_lane.sh" --list-legs)
jobs=$(gh api --paginate --slurp \
  "repos/$GH_REPO/actions/runs/$GITHUB_RUN_ID/attempts/$GITHUB_RUN_ATTEMPT/jobs?per_page=100")
expected=$(printf '%s\n' "$legs" | jq -Rsc 'split("\n") | map(select(length > 0))')
jq -er --argjson expected "$expected" '
  if type != "array" or length == 0 or any(.[]; (.jobs | type) != "array")
  then error("unmeasured macOS job census")
  elif ([.[].total_count] | unique | length) != 1 then error("inconsistent macOS job census")
  else . as $pages | [.[].jobs[]]
    | if length == $pages[0].total_count then . else error("partial macOS job census") end end
  | . as $jobs
  | $expected | map(. as $leg
    | ("Rust on macOS strict lint (" + $leg + ")") as $name
    | [$jobs[] | select(.name == $name)] as $matches
    | if ($matches | length) != 1 then {name:$name,status:"missing-or-duplicate",conclusion:null,measured:false}
      else $matches[0] | {name,status,conclusion,measured:true} end)
  | . as $legs
  | "macOS lint census: expected=\($legs | length) observed=\([$legs[] | select(.measured)] | length) pending=\([$legs[] | select(.status != "completed")] | length) bad=\([$legs[] | select(.status == "completed" and .conclusion != "success")] | length)",
    ($legs[] | "\(.name): \(.status)/\(.conclusion // "unreported")"),
    (if all($legs[]; .status == "completed" and .conclusion == "success")
      then "All macOS strict lint legs succeeded"
      else error("macOS lint proof incomplete or failed") end)
' <<< "$jobs"
[[ ${MATRIX_RESULT:-unreported} == success ]] || {
  echo "error: macOS matrix result ${MATRIX_RESULT:-unreported} is not success" >&2
  exit 1
}
