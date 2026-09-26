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
  "api repos/acme/consumer") echo main ;;
  "api repos/acme/consumer/pulls/"*) cat "$STUB/pull" ;;
  "api repos/acme/consumer/actions/runs/"*) cat "$STUB/run" ;;
  "workflow run") echo "$*" > "$STUB/dispatched"; cat "$STUB/dispatch" ;;
  *) echo "unexpected gh $*" >&2; exit 2 ;;
esac
EOF
chmod +x "$scratch/bin/gh"

canary() {
  PATH="$scratch/bin:$PATH" STUB="$scratch" CANARY_REPOSITORY=acme/consumer \
    CANARY_WORKFLOW=rehearsal.yml SOURCE_REVISION=0123456789abcdef0123456789abcdef01234567 \
    TARGET_VERSION=1.2.3-dev CANARY_POLL_SECONDS=0 PAIRING_TEXT="${1:-}" \
    bash "$root/scripts/ci/consumer_canary.sh" > "$scratch/out" 2>&1
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

run_url=https://github.com/acme/consumer/actions/runs/42
echo "$run_url" > "$scratch/dispatch"

# Only a completed success is green, and the output names the verdict, the link
# and the wall time.
echo "completed success" > "$scratch/run"
canary
grep -q "verdict=pass conclusion=success run=$run_url wall_seconds=" "$scratch/out"
grep -q -- '--ref main ' "$scratch/dispatched"

# Every other terminal state is red by name.
for conclusion in failure cancelled timed_out none; do
  echo "completed $conclusion" > "$scratch/run"
  refuses consumer_rehearsal_failed
done

# A dispatch that names no run is not a pass.
: > "$scratch/dispatch"
refuses dispatch_returned_no_run
echo "$run_url" > "$scratch/dispatch"

# A pairing rehearses the paired pull request's branch.
echo "completed success" > "$scratch/run"
echo "open acme/consumer fix-flag" > "$scratch/pull"
canary $'Body\n\nPairs-with: consumer#7'
grep -q -- '--ref fix-flag ' "$scratch/dispatched"
grep -q "ref=pull-7" "$scratch/out"

# A pairing it cannot honour fails by name instead of rehearsing the default.
echo "open someone/fork fix-flag" > "$scratch/pull"
refuses paired_pull_from_fork 'Pairs-with: consumer#7'
refuses "pairing_ambiguous pulls=7,8" $'Pairs-with: consumer#7\nPairs-with: consumer#8'
refuses pairing_unrecognized 'Pairs-with: other#7'

echo "Consumer canary: pass, red terminal states, missing run and pairing controls passed"
