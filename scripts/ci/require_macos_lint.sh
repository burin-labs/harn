#!/usr/bin/env bash
set -euo pipefail
root=$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)
: "${GITHUB_REPOSITORY:?repository required}"
: "${GITHUB_RUN_ID:?run required}"
: "${GITHUB_RUN_ATTEMPT:?attempt required}"
: "${GITHUB_SHA:?source required}"
: "${GITHUB_WORKFLOW_REF:?workflow required}"
[[ "$GITHUB_RUN_ID" =~ ^[1-9][0-9]*$ && "$GITHUB_RUN_ATTEMPT" =~ ^[1-9][0-9]*$ ]] || exit 1
[[ "$GITHUB_SHA" =~ ^[0-9a-f]{40}$ ]] || exit 1
[[ $(git -C "$root" rev-parse HEAD) == "$GITHUB_SHA" ]] || {
  echo 'error: macOS lint evidence source differs from checkout' >&2
  exit 1
}
git -C "$root" diff --quiet HEAD -- || {
  echo 'error: macOS lint evidence requires an unchanged checkout' >&2
  exit 1
}
expected=$(bash "$root/scripts/ci/run_rust_lint_lane.sh" --list-legs | jq -Rsc 'split("\n") | map(select(length > 0))')
jq -e 'length > 0 and (unique | length) == length' <<<"$expected" >/dev/null

case ${1:-verify} in
record)
  : "${GITHUB_OUTPUT:?output channel required}"
  : "${LINT_LEG:?lint role required}"
  [[ ${LINT_OUTCOME:-unreported} == success && ${GITHUB_JOB:-} == macos-lint ]] || {
    echo 'error: macOS lint receipt requires a reached successful lint step' >&2
    exit 1
  }
  jq -e --arg leg "$LINT_LEG" 'index($leg) != null' <<<"$expected" >/dev/null
  receipt=$(jq -cn --arg leg "$LINT_LEG" --arg source "$GITHUB_SHA" \
    --arg repository "$GITHUB_REPOSITORY" --arg run "$GITHUB_RUN_ID" \
    --arg workflow "$GITHUB_WORKFLOW_REF" --argjson attempt "$GITHUB_RUN_ATTEMPT" \
    '{schema:"harn.macos-lint.v1",leg:$leg,source:$source,repository:$repository,
        runId:$run,workflowRef:$workflow,producerAttempt:$attempt,producerJob:"macos-lint",conclusion:"success"}')
  printf '%s=%s\n' "$LINT_LEG" "$receipt" >>"$GITHUB_OUTPUT"
  printf 'macOS lint receipt: %s\n' "$receipt"
  ;;
verify)
  proof=$(jq -cn --argjson expected "$expected" --arg raw "${LINT_PROOFS:-}" \
    --arg matrix "${MATRIX_RESULT:-unreported}" --arg source "$GITHUB_SHA" \
    --arg repository "$GITHUB_REPOSITORY" --arg run "$GITHUB_RUN_ID" \
    --arg workflow "$GITHUB_WORKFLOW_REF" --argjson attempt "$GITHUB_RUN_ATTEMPT" '
      (try ($raw | fromjson) catch null) as $outputs
      | [ $expected[] | . as $leg
          | (if ($outputs|type)=="object" then $outputs[$leg] else null end) as $encoded
          | if ($encoded|type)!="string" or $encoded=="" then {leg:$leg,state:"missing"}
            else (try ($encoded|fromjson) catch null) as $r
            | if ($r|type)!="object" then {leg:$leg,state:"invalid"}
              elif $r.schema!="harn.macos-lint.v1" or $r.leg!=$leg
                or $r.source!=$source or $r.repository!=$repository or $r.runId!=$run
                or $r.workflowRef!=$workflow or $r.producerJob!="macos-lint"
                or ($r.producerAttempt|type)!="number" or $r.producerAttempt<1
                or $r.producerAttempt> $attempt or $r.producerAttempt!=($r.producerAttempt|floor)
              then {leg:$leg,state:"foreign"}
              elif $r.conclusion!="success" then {leg:$leg,state:"failed"}
              else {leg:$leg,state:"success",receipt:$r} end end
        ] as $roles
      | (if ($outputs|type)=="object" then ($outputs|keys)-$expected else ["unmeasured-output-map"] end) as $unexpected
      | {expected:($expected|length),observed:([$roles[]|select(.state!="missing")]|length),
          pending:([$roles[]|select(.state=="missing")]|length),
          bad:(([$roles[]|select(.state!="success" and .state!="missing")]|length)+($unexpected|length)+(if $matrix=="success" then 0 else 1 end)),
          matrixResult:$matrix,consumerAttempt:$attempt,unexpected:$unexpected,roles:$roles}
      | .decision=(if .pending==0 and .bad==0 then "passed" else "refused" end)
    ')
  jq -r '"macOS lint census: expected=\(.expected) observed=\(.observed) pending=\(.pending) bad=\(.bad)",
      (.roles[]|"\(.leg): \(.state) producerAttempt=\(.receipt.producerAttempt // "unreported")")' <<<"$proof"
  printf '%s\n' "$proof"
  jq -e '.decision=="passed"' <<<"$proof" >/dev/null
  ;;
*)
  echo 'usage: require_macos_lint.sh [record|verify]' >&2
  exit 2
  ;;
esac
