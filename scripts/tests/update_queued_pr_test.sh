#!/usr/bin/env bash
set -euo pipefail

repo_root=$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)
script="$repo_root/scripts/update_queued_pr.sh"
tmp_root=$(mktemp -d)
trap 'rm -rf "$tmp_root"' EXIT

old_oid=1111111111111111111111111111111111111111

make_fixture() {
  local fixture="$1"
  local fanout_mode="$2"
  mkdir -p "$fixture/bin" "$fixture/state"
  printf '%s\n' "$fanout_mode" > "$fixture/state/fanout-mode"
  printf 'queued\n' > "$fixture/state/queue"
  printf '%s\n' "$old_oid" > "$fixture/state/remote-oid"

  apply_mock_patch "$fixture"
}

apply_mock_patch() {
  local fixture="$1"
  cp "$tmp_root/mock-gh" "$fixture/bin/gh"
  cp "$tmp_root/mock-git" "$fixture/bin/git"
  chmod +x "$fixture/bin/gh" "$fixture/bin/git"
}

cat > "$tmp_root/mock-gh" <<'MOCK_GH'
#!/usr/bin/env bash
set -euo pipefail
printf 'gh %q' "$1" >> "$FIXTURE_STATE/calls"
printf ' %q' "${@:2}" >> "$FIXTURE_STATE/calls"
printf '\n' >> "$FIXTURE_STATE/calls"

if [[ "$1 $2" == "pr view" ]]; then
  if [[ " $* " == *" --json number "* ]]; then
    printf '5949\n'
  elif [[ " $* " == *" --json headRefOid "* ]]; then
    cat "$FIXTURE_STATE/remote-oid"
  else
    oid=$(cat "$FIXTURE_STATE/remote-oid")
    printf '{"id":"PR_5949","number":5949,"state":"OPEN","headRefName":"fix/5949-queued-pr-update","headRefOid":"%s","isCrossRepository":false}\n' "$oid"
  fi
  exit 0
fi

if [[ "$1 $2" == "api graphql" ]]; then
  if [[ " $* " == *"dequeuePullRequest"* ]]; then
    [[ " $* " == *" -f pullRequestId=PR_5949 "* ]]
    [[ " $* " != *" -f entryId="* ]]
    printf 'dequeued\n' > "$FIXTURE_STATE/queue"
    printf '{"data":{"dequeuePullRequest":{"clientMutationId":null}}}\n'
    exit 0
  fi
  queue=$(cat "$FIXTURE_STATE/queue")
  if [[ "$queue" == "queued" ]]; then
    printf '{"data":{"repository":{"pullRequest":{"mergeQueueEntry":{"id":"MQE_1","state":"AWAITING_CHECKS"},"autoMergeRequest":null}}}}\n'
  elif [[ "$queue" == "requeued" ]]; then
    printf '{"data":{"repository":{"pullRequest":{"mergeQueueEntry":null,"autoMergeRequest":{"enabledAt":"2026-08-02T00:00:00Z"}}}}}\n'
  else
    printf '{"data":{"repository":{"pullRequest":{"mergeQueueEntry":null,"autoMergeRequest":null}}}}\n'
  fi
  exit 0
fi

if [[ "$1" == "api" && "$2" == "/repos/burin-labs/harn/actions/runs" ]]; then
  mode=$(cat "$FIXTURE_STATE/fanout-mode")
  if [[ "$mode" == "immediate" || -f "$FIXTURE_STATE/reopened" ]]; then
    printf '{"workflow_runs":[{"head_sha":"2222222222222222222222222222222222222222","event":"pull_request"}]}\n'
  else
    printf '{"workflow_runs":[]}\n'
  fi
  exit 0
fi

if [[ "$1 $2" == "pr close" ]]; then
  touch "$FIXTURE_STATE/closed"
  exit 0
fi
if [[ "$1 $2" == "pr reopen" ]]; then
  [[ -f "$FIXTURE_STATE/closed" ]]
  touch "$FIXTURE_STATE/reopened"
  exit 0
fi
if [[ "$1 $2" == "pr merge" ]]; then
  printf 'requeued\n' > "$FIXTURE_STATE/queue"
  exit 0
fi

printf 'unexpected gh invocation: %q ' "$@" >&2
exit 90
MOCK_GH

cat > "$tmp_root/mock-git" <<'MOCK_GIT'
#!/usr/bin/env bash
set -euo pipefail
printf 'git %q' "$1" >> "$FIXTURE_STATE/calls"
printf ' %q' "${@:2}" >> "$FIXTURE_STATE/calls"
printf '\n' >> "$FIXTURE_STATE/calls"

case "$1" in
  symbolic-ref)
    printf 'fix/5949-queued-pr-update\n'
    ;;
  rev-parse)
    printf '2222222222222222222222222222222222222222\n'
    ;;
  push)
    printf '2222222222222222222222222222222222222222\n' > "$FIXTURE_STATE/remote-oid"
    ;;
  ls-remote)
    oid=$(cat "$FIXTURE_STATE/remote-oid")
    if [[ -f "$FIXTURE_STATE/forced-ls-remote" ]]; then
      oid=$(cat "$FIXTURE_STATE/forced-ls-remote")
    fi
    printf '%s\trefs/heads/fix/5949-queued-pr-update\n' "$oid"
    ;;
  *)
    printf 'unexpected git invocation: %q ' "$@" >&2
    exit 91
    ;;
esac
MOCK_GIT

run_fixture() {
  local fixture="$1"
  FIXTURE_STATE="$fixture/state" \
    HARN_QUEUED_PR_GH_BIN="$fixture/bin/gh" \
    HARN_QUEUED_PR_GIT_BIN="$fixture/bin/git" \
    "$script" \
      --repo burin-labs/harn \
      --pr 5949 \
      --timeout 0 \
      --poll-interval 0
}

immediate="$tmp_root/immediate"
make_fixture "$immediate" immediate
immediate_output=$(run_fixture "$immediate")
grep -Fq 'result=success' <<<"$immediate_output"
grep -Fq 'head_oid=2222222222222222222222222222222222222222' <<<"$immediate_output"
grep -Fq 'exact_head_pull_request_run=true' <<<"$immediate_output"
grep -Fq 'requeued=true' <<<"$immediate_output"
if grep -Fq 'gh pr close' "$immediate/state/calls"; then
  echo 'immediate fanout should not close the PR' >&2
  exit 1
fi
grep -Fq 'dequeuePullRequest' "$immediate/state/calls"
grep -Fq -- '-f pullRequestId=PR_5949' "$immediate/state/calls"
if grep -Fq -- '-f entryId=' "$immediate/state/calls"; then
  echo 'dequeue mutation must use the pull request node ID, not the queue entry ID' >&2
  exit 1
fi
grep -Fq 'git push --no-verify --force-with-lease=refs/heads/fix/5949-queued-pr-update:1111111111111111111111111111111111111111 origin HEAD:refs/heads/fix/5949-queued-pr-update' "$immediate/state/calls"
grep -Fq 'previous_head_oid=1111111111111111111111111111111111111111' <<<"$immediate_output"

dropped="$tmp_root/dropped"
make_fixture "$dropped" dropped
dropped_output=$(run_fixture "$dropped")
grep -Fq 'result=success' <<<"$dropped_output"
grep -Fq 'closing and reopening PR #5949 once' <<<"$dropped_output"
grep -Fq 'gh pr close 5949' "$dropped/state/calls"
grep -Fq 'gh pr reopen 5949' "$dropped/state/calls"
close_line=$(grep -n 'gh pr close' "$dropped/state/calls" | cut -d: -f1)
merge_line=$(grep -n 'gh pr merge' "$dropped/state/calls" | cut -d: -f1)
[[ "$close_line" -lt "$merge_line" ]]

mismatch="$tmp_root/mismatch"
make_fixture "$mismatch" immediate
printf '3333333333333333333333333333333333333333\n' > "$mismatch/state/forced-ls-remote"
if run_fixture "$mismatch" > "$mismatch/output" 2>&1; then
  echo 'expected remote OID mismatch to fail' >&2
  exit 1
fi
grep -Fq 'remote branch proof failed' "$mismatch/output"
if grep -Fq 'gh pr merge' "$mismatch/state/calls"; then
  echo 'remote mismatch should not requeue the PR' >&2
  exit 1
fi
if grep -Fq 'gh pr close' "$mismatch/state/calls"; then
  echo 'remote mismatch should not start fanout recovery' >&2
  exit 1
fi

grep -Fq 'scripts/update_queued_pr.sh' "$repo_root/.githooks/pre-push"
echo 'update_queued_pr_test: all checks passed'
