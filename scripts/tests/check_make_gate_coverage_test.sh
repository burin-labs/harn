#!/usr/bin/env bash
#
# The gate-coverage ratchet is itself a gate, so it needs the one control that
# distinguishes a working check from a check that can never fail: inject an
# orphan target and prove the checker goes red on it.
set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
checker="$repo_root/scripts/check_make_gate_coverage.sh"

work="$(mktemp -d)"
trap 'rm -rf "$work"' EXIT

fail() { echo "check_make_gate_coverage_test: $*" >&2; exit 1; }

# --- 1. NEGATIVE CONTROL: the real tree passes -------------------------------
if ! "$checker" >/dev/null 2>&1; then
  "$checker" || true
  fail "the checked-in tree does not satisfy its own gate-coverage registry"
fi

# --- fixture: a miniature repo the checker can be pointed at -----------------
mkdir -p "$work/.github/workflows" "$work/scripts"
cat > "$work/Makefile" <<'MK'
all: fmt
	@$(HARN_BIN_ASSIGN); \
	$(MAKE) HARN_BIN="$$harn_bin" wired-gate fanout-gate local-gate
MK
cat > "$work/.github/workflows/ci.yml" <<'WF'
jobs:
  a:
    steps:
      - run: make wired-gate
WF
cat > "$work/scripts/audit_gates.sh" <<'AG'
GATES=(
  fanout-gate
)
AG
registry() { cat > "$work/.github/make-gate-coverage.json"; }

run_checker() {
  ( REPO_ROOT="$work" REGISTRY=".github/make-gate-coverage.json" MAKEFILE="Makefile" \
      AUDIT_GATES="scripts/audit_gates.sh" WORKFLOW_DIR=".github" bash "$checker" )
}

# --- 2. fixture passes when the one unreached target is registered -----------
registry <<'J'
{"schema_version":1,"intentionally_local":[{"target":"local-gate","reason":"local only, on purpose"}]}
J
run_checker >/dev/null 2>&1 || fail "fixture should pass when the orphan is registered"

# --- 3. POSITIVE CONTROL: unregister it and the checker MUST fail ------------
registry <<'J'
{"schema_version":1,"intentionally_local":[]}
J
if run_checker >/dev/null 2>&1; then
  fail "checker passed an UNREGISTERED orphan target — it cannot detect the defect it exists for"
fi
out=$(run_checker 2>&1 || true)
grep -q 'local-gate' <<<"$out" || fail "failure output does not name the offending target: $out"

# --- 4. a reason-less registry row is rejected -------------------------------
registry <<'J'
{"schema_version":1,"intentionally_local":[{"target":"local-gate","reason":""}]}
J
run_checker >/dev/null 2>&1 && fail "empty reason should be rejected"

# --- 5. a stale row (target IS reached) is rejected --------------------------
registry <<'J'
{"schema_version":1,"intentionally_local":[{"target":"local-gate","reason":"ok"},{"target":"wired-gate","reason":"stale row"}]}
J
run_checker >/dev/null 2>&1 && fail "a registry row for an already-reached target should be rejected"

# --- 6. a row for a target `make all` no longer runs is rejected -------------
registry <<'J'
{"schema_version":1,"intentionally_local":[{"target":"local-gate","reason":"ok"},{"target":"deleted-gate","reason":"no longer in make all"}]}
J
run_checker >/dev/null 2>&1 && fail "a registry row for a target outside \`make all\` should be rejected"

# --- 7. multi-target `make a b` credits every target on the line -------------
cat > "$work/.github/workflows/ci.yml" <<'WF'
jobs:
  a:
    steps:
      - run: make wired-gate local-gate
WF
registry <<'J'
{"schema_version":1,"intentionally_local":[]}
J
run_checker >/dev/null 2>&1 || fail "a target named second on a \`make a b\` line was not credited"

echo "check_make_gate_coverage_test: ok"
