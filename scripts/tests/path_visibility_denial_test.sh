#!/usr/bin/env bash
# The denial arm of scripts/path_visibility.harn, exercised against a real
# capability-policy refusal rather than a hand-built status dict.
#
# This cannot live in the `harn test` suite: that runner executes without the
# worktree filesystem sandbox, so nothing is refusable in-process and the arm
# would pass by measuring nothing. `harn run` applies the sandbox, so a path
# outside the worktree is genuinely denied and the classifier has something real
# to read.
set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd -P)"
cd "$repo_root"

harn_bin="${HARN_BIN:-}"
if [[ -z "$harn_bin" ]]; then
  harn_bin="$(HARN_BIN='' HARN_BIN_NO_BUILD=0 ./scripts/harn_bin.sh --print)"
fi

probe="$repo_root/.path-visibility-probe.harn"
cleanup() { rm -f "$probe"; }
trap cleanup EXIT

cat > "$probe" <<'HARN'
import { path_visibility } from "scripts/path_visibility.harn"

fn main(harness: Harness) {
  // Outside the worktree, so the sandbox refuses it.
  const denied = path_visibility(harness.fs, "probe gate", "/etc/hosts")
  // Inside the worktree and genuinely absent.
  const missing = path_visibility(harness.fs, "probe gate", "no-such-file-here.txt")
  harness.stdio.println("denied_visible=" + to_string(denied.visible))
  harness.stdio.println("denied_status=" + denied.status)
  harness.stdio.println("denied_line=" + denied.denial)
  harness.stdio.println("missing_status=" + missing.status)
  harness.stdio.println("missing_line=" + missing.denial)
}
HARN

out="$("$harn_bin" run "$probe")"

fail() {
  echo "path_visibility_denial_test: $1" >&2
  echo "--- probe output ---" >&2
  echo "$out" >&2
  exit 1
}

# A denied path must be reported as denied, and must say so in words an operator
# can grep for.
grep -Fxq 'denied_visible=false' <<<"$out" || fail "a denied path reported itself visible"
grep -Fxq 'denied_status=scope_denied' <<<"$out" || fail "expected scope_denied from the sandbox"
grep -Eq '^denied_line=.*denied.*/etc/hosts' <<<"$out" \
  || fail "the denial line must contain the word 'denied' and name the path"

# The negative control. Without this the test passes if the classifier simply
# calls everything denied, which would turn every gate red for no reason.
grep -Fxq 'missing_status=missing' <<<"$out" || fail "an absent in-scope file was not reported missing"
grep -Fxq 'missing_line=' <<<"$out" || fail "an absent in-scope file must not produce a denial line"

echo "path_visibility_denial_test: ok"
