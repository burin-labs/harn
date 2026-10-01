#!/usr/bin/env bash
# The canary must never report green without a consumer run that concluded
# success, and must rehearse exactly the pairing it was given.
set -euo pipefail
root=$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)
scratch=$(mktemp -d "${TMPDIR:-/tmp}/harn-consumer-canary.XXXXXX")
trap 'rm -rf "$scratch"' EXIT

# A stand-in `gh` that answers from files, so each case sets the consumer's
# reply without a network.
mkdir -p "$scratch/bin"
cat > "$scratch/bin/gh" <<'EOF'
#!/usr/bin/env bash
case "$1 $2" in
  "api repos/acme/widget-host") echo main ;;
  "api repos/acme/widget-host/pulls/"*) cat "$STUB/pull" ;;
  "api repos/acme/widget-host/actions/runs/"*) cat "$STUB/run" ;;
  "workflow run") echo "$*" > "$STUB/dispatched"; cat "$STUB/dispatch" ;;
  "api repos/acme/harn/actions/workflows/"*) [[ -f "$STUB/history" ]] || exit 1; cat "$STUB/history" ;;
  "api repos/acme/harn/actions/runs/"*/jobs) id=${2#repos/acme/harn/actions/runs/}; cat "$STUB/jobs-${id%/jobs}" ;;
  *) echo "unexpected gh $*" >&2; exit 2 ;;
esac
EOF
chmod +x "$scratch/bin/gh"

canary() {
  local workspace_version=${2:-1.2.4-dev}
  PATH="$scratch/bin:$PATH" STUB="$scratch" CANARY_REPOSITORY=acme/widget-host \
    CANARY_WORKFLOW=rehearsal.yml SOURCE_REVISION=0123456789abcdef0123456789abcdef01234567 \
    WORKSPACE_VERSION="$workspace_version" CANARY_POLL_SECONDS=0 PAIRING_TEXT="${1:-}" \
    GITHUB_OUTPUT="$scratch/gho" \
    bash "$root/scripts/ci/consumer_canary.sh" > "$scratch/out" 2>&1
  local status=$?
  # This output is public; the consumer's name or a link to it never is.
  if grep -qE 'widget-host|github\.com' "$scratch/out"; then
    echo "canary output names the consumer:" >&2
    cat "$scratch/out" >&2
    exit 1
  fi
  return "$status"
}

# `! canary` would not stop the test under errexit, so a refusal is asserted
# explicitly, with its reason.
refuses() {
  local reason=$1
  shift
  if canary "$@"; then
    echo "expected refusal $reason, got green" >&2
    exit 1
  fi
  grep -q "reason=$reason" "$scratch/out"
}

run_url=https://github.com/acme/widget-host/actions/runs/42
echo "$run_url" > "$scratch/dispatch"

# Only a completed success is green, and the output names the verdict, the link
# and the wall time.
echo "completed success" > "$scratch/run"
: > "$scratch/gho"
canary
grep -q "verdict=pass conclusion=success run=42 wall_seconds=" "$scratch/out"
grep -qx "verdict=pass" "$scratch/gho"
grep -q "consumer=configured secret=CONSUMER_CANARY_REPOSITORY" "$scratch/out"
grep -q -- '--ref main ' "$scratch/dispatched"
grep -q -- '-f target=v1.2.3 ' "$scratch/dispatched"

# The current development identity maps to the latest published tag, which is
# the version the consumer accepts as its repin floor.
echo "completed success" > "$scratch/run"
canary "" 0.10.153-dev
grep -q -- '-f target=v0.10.152 ' "$scratch/dispatched"

# A prerelease that is not the workspace's canonical development identity has
# no published target and must fail before contacting or dispatching to the
# consumer.
rm -f "$scratch/dispatched"
refuses workspace_version_unpublished "" 1.2.4-rc.1
[[ ! -e "$scratch/dispatched" ]]

# Every other terminal state is red by name, and is still a settled verdict.
for conclusion in failure cancelled timed_out none; do
  echo "completed $conclusion" > "$scratch/run"
  : > "$scratch/gho"
  refuses consumer_rehearsal_failed
  grep -qx "verdict=fail" "$scratch/gho"
done

# A run that never concludes is unmeasured: red by name, and no verdict.
echo "in_progress none" > "$scratch/run"
: > "$scratch/gho"
if PATH="$scratch/bin:$PATH" STUB="$scratch" CANARY_REPOSITORY=acme/widget-host \
  CANARY_WORKFLOW=rehearsal.yml SOURCE_REVISION=0123456789abcdef0123456789abcdef01234567 \
  WORKSPACE_VERSION=1.2.4-dev CANARY_POLL_SECONDS=0 CANARY_DEADLINE_SECONDS=-1 \
  GITHUB_OUTPUT="$scratch/gho" \
  bash "$root/scripts/ci/consumer_canary.sh" > "$scratch/out" 2>&1; then
  echo "an unconcluded consumer run reported green" >&2
  exit 1
fi
grep -q "reason=no_verdict_before_deadline" "$scratch/out"
if [[ -s "$scratch/gho" ]]; then
  echo "an unmeasured run wrote a verdict" >&2
  exit 1
fi

# An unset repository secret arrives as a bare owner and fails by name.
if PATH="$scratch/bin:$PATH" STUB="$scratch" CANARY_REPOSITORY=acme/ \
  CANARY_WORKFLOW=rehearsal.yml SOURCE_REVISION=0123456789abcdef0123456789abcdef01234567 \
  WORKSPACE_VERSION=1.2.4-dev CANARY_POLL_SECONDS=0 \
  bash "$root/scripts/ci/consumer_canary.sh" > "$scratch/out" 2>&1; then
  echo "an unset consumer repository reported green" >&2
  exit 1
fi
grep -q "reason=consumer_repository_unset secret=CONSUMER_CANARY_REPOSITORY" "$scratch/out"

# A dispatch that names no run is not a pass.
: > "$scratch/dispatch"
refuses dispatch_returned_no_run
echo "$run_url" > "$scratch/dispatch"

# A pairing rehearses the paired pull request's branch.
echo "completed success" > "$scratch/run"
echo "open acme/widget-host fix-flag" > "$scratch/pull"
canary $'Body\n\nPairs-with: consumer#7'
grep -q -- '--ref fix-flag ' "$scratch/dispatched"
grep -q "ref=pull-7" "$scratch/out"

# A pairing it cannot honour fails by name instead of rehearsing the default.
echo "open someone/fork fix-flag" > "$scratch/pull"
refuses paired_pull_from_fork 'Pairs-with: consumer#7'
refuses "pairing_ambiguous pulls=7,8" $'Pairs-with: consumer#7\nPairs-with: consumer#8'
refuses pairing_unrecognized 'Pairs-with: other#7'

# Negative control for the identity guard: a line naming the consumer is
# refused instead of printed.
if (
  source "$root/scripts/ci/consumer_canary.sh"
  CANARY_SECRET_NAME=widget-host
  canary_say "run=https://github.com/acme/widget-host/actions/runs/42"
) > "$scratch/out" 2>&1; then
  echo "identity guard printed the consumer's name" >&2
  exit 1
fi
grep -q "reason=identity_in_output" "$scratch/out"
if grep -q widget-host "$scratch/out"; then
  echo "identity guard echoed the consumer's name" >&2
  exit 1
fi

# The decide step skips a scheduled run only when main has not moved since the
# last settled verdict, and names what it compared either way.
main_sha=0123456789abcdef0123456789abcdef01234567
old_sha=89abcdef0123456789abcdef0123456789abcdef
decide() {
  : > "$scratch/gho"
  PATH="$scratch/bin:$PATH" STUB="$scratch" EVENT_NAME=$1 SOURCE_REVISION=$main_sha \
    GITHUB_REPOSITORY=acme/harn CURRENT_RUN_ID=900 GITHUB_OUTPUT="$scratch/gho" \
    bash "$root/scripts/ci/consumer_canary.sh" --decide > "$scratch/out" 2>&1
}
# Newest first: this run, an unsettled run at main, then a settled one.
printf '900 %s\n901 %s\n902 %s\n' "$main_sha" "$main_sha" "$old_sha" > "$scratch/history"
echo 0 > "$scratch/jobs-901"
echo 1 > "$scratch/jobs-902"
decide schedule
grep -qx "run=true" "$scratch/gho"
grep -q "run reason=main_moved main=$main_sha last_settled=$old_sha last_settled_run=902" "$scratch/out"

echo 1 > "$scratch/jobs-901"
decide schedule
grep -qx "run=false" "$scratch/gho"
grep -q "skipped reason=main_unchanged main=$main_sha last_settled=$main_sha last_settled_run=901" "$scratch/out"

# A dispatch always rehearses, even at an already settled commit.
decide workflow_dispatch
grep -qx "run=true" "$scratch/gho"
grep -q "run reason=explicit_workflow_dispatch main=$main_sha" "$scratch/out"

echo 0 > "$scratch/jobs-901"
echo 0 > "$scratch/jobs-902"
decide schedule
grep -qx "run=true" "$scratch/gho"
grep -q "run reason=no_settled_verdict main=$main_sha" "$scratch/out"

# Unreadable history is a named failure, never a skip.
rm "$scratch/history"
if decide schedule; then
  echo "unreadable history decided anyway" >&2
  exit 1
fi
grep -q "reason=settled_history_unreadable" "$scratch/out"
if [[ -s "$scratch/gho" ]]; then
  echo "unreadable history wrote a decision" >&2
  exit 1
fi

echo "Consumer canary: pass, red terminal states, missing run and pairing controls passed"
