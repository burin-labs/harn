#!/usr/bin/env bash
set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd -P)"
script="$repo_root/scripts/cancel_superseded_merge_groups.sh"
fixture_root="$(mktemp -d)"
trap 'rm -rf "$fixture_root"' EXIT
mkdir -p "$fixture_root/bin"

current_a="aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"
current_b="bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb"
stale="cccccccccccccccccccccccccccccccccccccccc"
calls="$fixture_root/calls"

cat > "$fixture_root/bin/gh" <<'SH'
#!/usr/bin/env bash
set -euo pipefail
printf '%s\n' "$*" >> "$FAKE_GH_CALLS"
if [[ "${FAKE_QUEUE_FAILURE:-0}" == "1" && "$1" == "api" && "$2" == "graphql" ]]; then
  exit 1
fi
if [[ "$1" == "api" && "$2" == "graphql" ]]; then
  printf '%s\n' "$FAKE_QUEUE_JSON"
  exit 0
fi
if [[ "$1" == "api" && "$2" == "--paginate" ]]; then
  created_epoch="$(date +%s)"
  case "$*" in
    *status=in_progress*)
      printf '%s\n' $'101\taaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa\tin_progress\tCI\t'"$created_epoch"
      printf '%s\n' $'102\tcccccccccccccccccccccccccccccccccccccccc\tin_progress\tCI\t'"$created_epoch"
      ;;
    *status=queued*)
      printf '%s\n' $'103\tbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb\tqueued\tPR gates\t'"$created_epoch"
      if [[ "${FAKE_STALE_QUEUED_ZOMBIE:-0}" == "1" ]]; then
        printf '%s\n' $'104\tdddddddddddddddddddddddddddddddddddddddd\tqueued\tSDK codegen\t0'
      fi
      # An ancient queued run that DOES carry a job row. Unkillable in exactly
      # the same way, but invisible to the jobless pre-check.
      if [[ "${FAKE_UNCANCELLABLE_ANCIENT:-0}" == "1" ]]; then
        printf '%s\n' $'105\teeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeee\tqueued\tLean embedding surface\t0'
      fi
      # The same refusal on a RECENT run, which must still fail the job.
      if [[ "${FAKE_UNCANCELLABLE_RECENT:-0}" == "1" ]]; then
        printf '%s\n' $'106\tffffffffffffffffffffffffffffffffffffffff\tqueued\tLean embedding surface\t'"$created_epoch"
      fi
      ;;
  esac
  exit 0
fi
if [[ "$1" == "api" && "$2" == "--method" && "$3" == "POST" ]]; then
  # Runs 105 and 106 refuse BOTH cancellation endpoints, as GitHub does for a
  # queued record whose merge-group ref has been destroyed.
  if [[ "$4" == *'/runs/105/'* || "$4" == *'/runs/106/'* ]]; then
    exit 1
  fi
  if [[ "${FAKE_NORMAL_CANCEL_FAILURE:-0}" == "1" && "$4" == */cancel ]]; then
    exit 1
  fi
  exit 0
fi
if [[ "$1" == "api" && "$2" == *'/runs/104/jobs?per_page=1' ]]; then
  printf '0\n'
  exit 0
fi
if [[ "$1" == "api" && "$2" == *'/jobs?per_page=1' ]]; then
  # Carries a job row, so the jobless pre-check does not classify it.
  printf '1\n'
  exit 0
fi
if [[ "$1" == "api" && ( "$2" == *'/runs/105' || "$2" == *'/runs/106' ) ]]; then
  printf 'queued\n'
  exit 0
fi
echo "unexpected fake gh call: $*" >&2
exit 1
SH
chmod +x "$fixture_root/bin/gh"

queue_json="$(jq -cn --arg a "$current_a" --arg b "$current_b" '{data:{repository:{mergeQueue:{entries:{pageInfo:{hasNextPage:false},nodes:[{headCommit:{oid:$a}},{headCommit:{oid:$b}}]}}}}}')"

output="$(
  PATH="$fixture_root/bin:$PATH" \
  FAKE_GH_CALLS="$calls" \
  FAKE_QUEUE_JSON="$queue_json" \
    "$script" --repo burin-labs/harn --expected-sha "$current_a" --apply
)"
grep -Fq "preserve run=101 sha=$current_a" <<< "$output"
grep -Fq "preserve run=103 sha=$current_b" <<< "$output"
grep -Fq "stale run=102 sha=$stale" <<< "$output"
grep -Fq 'summary queue_state=configured current_heads=2 active_runs=3 stale_runs=1 cancelled=1 zombies=0 apply=true' <<< "$output"
grep -Fq 'repos/burin-labs/harn/actions/runs?event=merge_group&status=in_progress' "$calls"
grep -Fq 'api --method POST repos/burin-labs/harn/actions/runs/102/cancel' "$calls"
if grep -Eq 'runs/(101|103)/cancel' "$calls"; then
  echo "current exact queue heads were canceled" >&2
  exit 1
fi

: > "$calls"
forced_output="$(
  PATH="$fixture_root/bin:$PATH" \
  FAKE_GH_CALLS="$calls" \
  FAKE_QUEUE_JSON="$queue_json" \
  FAKE_NORMAL_CANCEL_FAILURE=1 \
    "$script" --repo burin-labs/harn --apply 2> /dev/null
)"
grep -Fq 'force-cancelled run=102' <<< "$forced_output"
grep -Fq 'summary queue_state=configured current_heads=2 active_runs=3 stale_runs=1 cancelled=1 zombies=0 apply=true' <<< "$forced_output"
grep -Fq 'api --method POST repos/burin-labs/harn/actions/runs/102/force-cancel' "$calls"

: > "$calls"
if PATH="$fixture_root/bin:$PATH" \
  FAKE_GH_CALLS="$calls" \
  FAKE_QUEUE_JSON="$queue_json" \
  FAKE_QUEUE_FAILURE=1 \
    "$script" --repo burin-labs/harn --apply > /dev/null 2>&1; then
  echo "queue API failure did not fail closed" >&2
  exit 1
fi
if grep -Fq '/cancel' "$calls"; then
  echo "queue API failure reached cancellation" >&2
  exit 1
fi

: > "$calls"
if PATH="$fixture_root/bin:$PATH" \
  FAKE_GH_CALLS="$calls" \
  FAKE_QUEUE_JSON="$queue_json" \
    "$script" --repo burin-labs/harn --expected-sha "$stale" --apply > /dev/null 2>&1; then
  echo "superseded event head was accepted as authoritative" >&2
  exit 1
fi
if grep -Fq '/cancel' "$calls"; then
  echo "superseded event head reached cancellation" >&2
  exit 1
fi

: > "$calls"
empty_queue_json='{"data":{"repository":{"mergeQueue":{"entries":{"pageInfo":{"hasNextPage":false},"nodes":[]}}}}}'
empty_output="$(
  PATH="$fixture_root/bin:$PATH" \
  FAKE_GH_CALLS="$calls" \
  FAKE_QUEUE_JSON="$empty_queue_json" \
    "$script" --repo burin-labs/harn --apply
)"
grep -Fq 'summary queue_state=configured current_heads=0 active_runs=3 stale_runs=3 cancelled=3 zombies=0 apply=true' <<< "$empty_output"
for run_id in 101 102 103; do
  grep -Fq "api --method POST repos/burin-labs/harn/actions/runs/$run_id/cancel" "$calls"
done

: > "$calls"
# GitHub returns an explicit null mergeQueue when the queue is disabled. That
# is distinct from the configured queue's measured empty nodes list above and
# cannot authorize even an Actions inventory read.
null_queue_json='{"data":{"repository":{"mergeQueue":null}}}'
null_output="$(
  PATH="$fixture_root/bin:$PATH" \
  FAKE_GH_CALLS="$calls" \
  FAKE_QUEUE_JSON="$null_queue_json" \
    "$script" --repo burin-labs/harn --expected-sha "$current_a" --apply
)"
grep -Fxq 'summary queue_state=disabled active_runs=not_queried action=none reason=nothing_to_supersede apply=true' <<< "$null_output"
if grep -Fq 'actions/runs' "$calls"; then
  echo "disabled merge queue reached the Actions API" >&2
  exit 1
fi

: > "$calls"
null_plan_output="$(
  PATH="$fixture_root/bin:$PATH" \
  FAKE_GH_CALLS="$calls" \
  FAKE_QUEUE_JSON="$null_queue_json" \
    "$script" --repo burin-labs/harn
)"
grep -Fxq 'summary queue_state=disabled active_runs=not_queried action=none reason=nothing_to_supersede apply=false' <<< "$null_plan_output"
if grep -Fq 'actions/runs' "$calls"; then
  echo "disabled merge queue plan reached the Actions API" >&2
  exit 1
fi

# NEGATIVE CONTROL. A missing field is not the same observation as an explicit
# null queue. It must still fail before any cancellation request is sent.
: > "$calls"
missing_queue_json='{"data":{"repository":{}}}'
if PATH="$fixture_root/bin:$PATH" \
  FAKE_GH_CALLS="$calls" \
  FAKE_QUEUE_JSON="$missing_queue_json" \
    "$script" --repo burin-labs/harn --apply > /dev/null 2>&1; then
  echo "missing mergeQueue field was accepted as an empty queue" >&2
  exit 1
fi
if grep -Fq '/cancel' "$calls"; then
  echo "missing mergeQueue field reached cancellation" >&2
  exit 1
fi

# Every untrusted GraphQL shape must fail before the first Actions read. This
# makes a missing observation distinguishable from a measured configured zero.
malformed_queue_jsons=(
  '{"errors":[{"message":"denied"}],"data":{"repository":{"mergeQueue":null}}}'
  '{"errors":"","data":{"repository":{"mergeQueue":null}}}'
  '{"data":{}}'
  '{"data":{"repository":null}}'
  '{"data":{"repository":{"mergeQueue":{}}}}'
  '{"data":{"repository":{"mergeQueue":{"entries":{}}}}}'
  '{"data":{"repository":{"mergeQueue":{"entries":{"pageInfo":{},"nodes":[]}}}}}'
  '{"data":{"repository":{"mergeQueue":{"entries":{"pageInfo":{"hasNextPage":true},"nodes":[]}}}}}'
  '{"data":{"repository":{"mergeQueue":{"entries":{"pageInfo":{"hasNextPage":false}}}}}}'
  '{"data":{"repository":{"mergeQueue":{"entries":{"pageInfo":{"hasNextPage":false},"nodes":{}}}}}}'
  '{"data":{"repository":{"mergeQueue":{"entries":{"pageInfo":{"hasNextPage":false},"nodes":[{}]}}}}}'
  '{"data":{"repository":{"mergeQueue":{"entries":{"pageInfo":{"hasNextPage":false},"nodes":[{"headCommit":{}}]}}}}}'
  '{"data":{"repository":{"mergeQueue":{"entries":{"pageInfo":{"hasNextPage":false},"nodes":[{"headCommit":{"oid":"short"}}]}}}}}'
)
for malformed_queue_json in "${malformed_queue_jsons[@]}"; do
  : > "$calls"
  if PATH="$fixture_root/bin:$PATH" \
    FAKE_GH_CALLS="$calls" \
    FAKE_QUEUE_JSON="$malformed_queue_json" \
      "$script" --repo burin-labs/harn --apply > /dev/null 2>&1; then
    echo "malformed merge-queue response was accepted" >&2
    exit 1
  fi
  if grep -Fq 'actions/runs' "$calls"; then
    echo "malformed merge-queue response reached the Actions API" >&2
    exit 1
  fi
done

: > "$calls"
zombie_output="$(
  PATH="$fixture_root/bin:$PATH" \
  FAKE_GH_CALLS="$calls" \
  FAKE_QUEUE_JSON="$queue_json" \
  FAKE_STALE_QUEUED_ZOMBIE=1 \
    "$script" --repo burin-labs/harn --apply
)"
grep -Fq 'zombie run=104 sha=dddddddddddddddddddddddddddddddddddddddd status=queued workflow=SDK codegen action=none' <<< "$zombie_output"
grep -Fq 'summary queue_state=configured current_heads=2 active_runs=4 stale_runs=2 cancelled=1 zombies=1 apply=true' <<< "$zombie_output"
if grep -Fq 'runs/104/cancel' "$calls"; then
  echo "ancient jobless queue metadata reached cancellation" >&2
  exit 1
fi

: > "$calls"
uncancellable_output="$(
  PATH="$fixture_root/bin:$PATH" \
  FAKE_GH_CALLS="$calls" \
  FAKE_QUEUE_JSON="$queue_json" \
  FAKE_UNCANCELLABLE_ANCIENT=1 \
    "$script" --repo burin-labs/harn --apply
)"
# It reaches cancellation, unlike the jobless case: unkillability is OBSERVED
# here, not predicted, which is the whole point of the second classification.
grep -Fq 'runs/105/cancel' "$calls"
grep -Fq 'runs/105/force-cancel' "$calls"
grep -Fq 'zombie run=105 sha=eeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeee status=queued workflow=Lean embedding surface action=none reason=uncancellable' <<< "$uncancellable_output"
grep -Fq 'summary queue_state=configured current_heads=2 active_runs=4 stale_runs=2 cancelled=1 zombies=1 apply=true' <<< "$uncancellable_output"

# NEGATIVE CONTROL. The same double refusal on a RECENT run must still fail the
# job. Without this, the branch above could swallow every cancellation failure
# and the controller would report success while cancelling nothing.
: > "$calls"
recent_status=0
recent_output="$(
  PATH="$fixture_root/bin:$PATH" \
  FAKE_GH_CALLS="$calls" \
  FAKE_QUEUE_JSON="$queue_json" \
  FAKE_UNCANCELLABLE_RECENT=1 \
    "$script" --repo burin-labs/harn --apply 2>&1
)" || recent_status=$?
if [[ "$recent_status" -eq 0 ]]; then
  echo "a recent uncancellable run did not fail the controller" >&2
  exit 1
fi
grep -Fq 'failed run=106 latest_status=queued' <<< "$recent_output"
if grep -Fq 'zombie run=106' <<< "$recent_output"; then
  echo "a recent uncancellable run was misclassified as a zombie" >&2
  exit 1
fi

echo "cancel_superseded_merge_groups_test: ok"
