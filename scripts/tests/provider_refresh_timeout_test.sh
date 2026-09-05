#!/usr/bin/env bash
# The #8014 falsifier, against the real command rather than its seam.
#
# `harn provider catalog refresh` waited on its workflow child with no bound at
# all, so an unreachable source hung a repin rehearsal for five minutes with
# nothing said about what it was waiting on. This runs the command against a
# workflow that never finishes and requires it to terminate inside the bound
# and say it timed out.
set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd -P)"
cd "$repo_root"

harn_bin="${HARN_BIN:-}"
if [[ -z "$harn_bin" ]]; then
  harn_bin="$(HARN_BIN='' HARN_BIN_NO_BUILD=0 ./scripts/harn_bin.sh --print)"
fi

script="$repo_root/.provider-refresh-timeout-probe.harn"
cleanup() { rm -f "$script"; }
trap cleanup EXIT

# Stands in for a source that accepts the connection and never answers.
cat > "$script" <<'HARN'
fn main(harness: Harness) {
  harness.clock.sleep_ms(600000)
}
HARN

started=$(date +%s)
set +e
out="$("$harn_bin" provider catalog refresh --script "$script" --timeout-secs 3 2>&1)"
status=$?
set -e
elapsed=$(( $(date +%s) - started ))

fail() {
  echo "provider_refresh_timeout_test: $1" >&2
  echo "--- elapsed=${elapsed}s status=${status} ---" >&2
  echo "$out" >&2
  exit 1
}

[[ "$status" -ne 0 ]] || fail "a refresh that never finished reported success"

# The bound is the point. Allow generous slack for process startup, but not so
# much that an unbounded wait could pass.
[[ "$elapsed" -lt 60 ]] || fail "the refresh was not bounded (took ${elapsed}s against a 3s limit)"

grep -q "timed out after 3s" <<<"$out" \
  || fail "the terminal message must name the bound it exceeded"
grep -q "not an empty catalog" <<<"$out" \
  || fail "a timeout must stay distinguishable from an export that found nothing"

# The negative control. Without it this passes for a command that always times
# out, which would be a worse defect than the unbounded wait.
set +e
ok_out="$("$harn_bin" provider catalog refresh --script "$script" --timeout-secs 0 --help 2>&1)"
ok_status=$?
set -e
[[ "$ok_status" -eq 0 ]] \
  || fail "a refresh invocation that does no waiting must still succeed: $ok_out"

echo "provider_refresh_timeout_test: ok"
