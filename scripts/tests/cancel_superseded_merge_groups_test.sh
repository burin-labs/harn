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
      ;;
  esac
  exit 0
fi
if [[ "$1" == "api" && "$2" == "--method" && "$3" == "POST" ]]; then
  if [[ "${FAKE_NORMAL_CANCEL_FAILURE:-0}" == "1" && "$4" == */cancel ]]; then
    exit 1
  fi
  exit 0
fi
if [[ "$1" == "api" && "$2" == *'/runs/104/jobs?per_page=1' ]]; then
  printf '0\n'
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
grep -Fq 'summary current_heads=2 stale_runs=1 cancelled=1 zombies=0 apply=true' <<< "$output"
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
grep -Fq 'summary current_heads=2 stale_runs=1 cancelled=1 zombies=0 apply=true' <<< "$forced_output"
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
grep -Fq 'summary current_heads=0 stale_runs=3 cancelled=3 zombies=0 apply=true' <<< "$empty_output"
for run_id in 101 102 103; do
  grep -Fq "api --method POST repos/burin-labs/harn/actions/runs/$run_id/cancel" "$calls"
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
grep -Fq 'summary current_heads=2 stale_runs=2 cancelled=1 zombies=1 apply=true' <<< "$zombie_output"
if grep -Fq 'runs/104/cancel' "$calls"; then
  echo "ancient jobless queue metadata reached cancellation" >&2
  exit 1
fi

echo "cancel_superseded_merge_groups_test: ok"
