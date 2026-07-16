#!/usr/bin/env bash
# Parallel runner for the Harn conformance + audit gate battery.
#
# The CI "Harn conformance + audit" lane historically ran a long SERIAL chain
# of `make` targets. Every target after `make conformance` reuses the same
# already-warm `target/debug/harn` binary and is INDEPENDENT of its siblings,
# yet they ran one after another — and with `HARN_BIN` unset each `cargo run`
# also re-walked cargo's build graph on every invocation.
#
# This mirrors the proven shape of `release_gate.sh audit` while avoiding a
# serial conformance-then-audit tail:
#   1. Build the harn CLI ONCE up front.
#   2. Export HARN_BIN so conformance and every downstream gate reuse it.
#   3. Run the conformance suite and the independent `make -j` audit gates in
#      parallel. GNU make already IS a bounded worker pool with failure
#      collection; `-k` keeps going after a failing gate so the run reports
#      EVERY gate's verdict, not just the first.
#
# Serial wall-clock was `warm build + conformance + max(tail gate)`; parallel
# wall-clock is `warm build + max(conformance, tail gates)` modulo the `-j`
# cap.
#
# Usage: scripts/audit_gates.sh
#   HARN_BIN                 pre-built binary to reuse (skips the warm build)
#   AUDIT_GATES_CONCURRENCY  `make -j` cap (default: nproc)
set -euo pipefail
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$repo_root"

# Independent gates that reuse the warm harn binary, run concurrently under
# `make -j`. `conformance` is handled separately below so its failure can still
# be reported as the conformance failure rather than hidden inside a make fanout.
GATES=(
  test-agent-scripts
  protocol-conformance
  lint-no-xfail-regression
  lint-harn
  fmt-harn
  check-highlight
  check-language-spec
  check-grammar-keywords
  check-trigger-quickref
  check-provider-matrix
  check-provider-support
  check-connector-matrix
  check-trigger-examples
  check-docs-model-refs
  check-docs-snippets
  check-docs-cli-flags
  check-diagnostics-catalog
  check-protocol-artifacts
  check-connector-schemas
  check-session-bundle-schema
  check-run-view-fixtures
  check-provider-catalog
  check-provider-catalog-drift
  check-ported-handler-loc
  check-source-file-lengths
  check-python-boundary
  check-harn-syntax-sensitive-scans
  check-ci-cache-policy
  check-crate-sibling-versions
  check-dependabot-groups
  check-docs-workflow-quickstart
  check-release-audit-contract
  check-vm-rss-soak
)

nproc_count() { getconf _NPROCESSORS_ONLN 2>/dev/null || echo 4; }
concurrency="${AUDIT_GATES_CONCURRENCY:-$(nproc_count)}"
case "$concurrency" in
  ''|*[!0-9]*) concurrency="$(nproc_count)" ;;
esac
[ "$concurrency" -lt 1 ] && concurrency=1

export CARGO_INCREMENTAL="${CARGO_INCREMENTAL:-0}"

# Resolve and export the warm binary so conformance and every downstream gate
# skip cargo staleness checks.
started="$(date +%s)"
echo "=== harn cli warm build ==="
if [ -z "${HARN_BIN:-}" ]; then
  HARN_BIN="$("$SCRIPT_DIR/harn_bin.sh" --print)"
fi
if [ ! -x "$HARN_BIN" ]; then
  echo "error: HARN_BIN is not executable: $HARN_BIN" >&2
  exit 1
fi
export HARN_BIN
echo "ok: harn-bin ($HARN_BIN)"
echo "ok: harn warm build ($(( $(date +%s) - started ))s)"

# ── Parallel phase: conformance + independent gates. ──
# `-k` (keep-going) runs every gate even when one fails, then make exits
# non-zero — so CI sees the full verdict set, not just the first failure.
# `-O` (--output-sync) groups each gate's output so parallel logs don't
# interleave; it needs GNU Make >= 4.0 (ubuntu-latest ships 4.3), so it is
# added only when supported to keep the script portable to macOS's Make 3.81.
output_sync=""
if make --help 2>&1 | grep -q -- "--output-sync"; then
  output_sync="-Otarget"
fi

audit_status=0
(
  echo "=== audit gates (make -j$concurrency -k${output_sync:+ $output_sync}, HARN_BIN warm) ==="
  gate_started="$(date +%s)"
  if make -j"$concurrency" -k ${output_sync} "${GATES[@]}"; then
    echo "ok: audit gates ($(( $(date +%s) - gate_started ))s)"
    echo "=== all audit gates passed ==="
  else
    status=$?
    echo "FAIL: one or more audit gates failed ($(( $(date +%s) - gate_started ))s)" >&2
    exit "$status"
  fi
) &
audit_pid=$!

conformance_status=0
conformance_started="$(date +%s)"
echo "=== conformance (HARN_BIN warm) ==="
if make conformance; then
  echo "ok: conformance ($(( $(date +%s) - conformance_started ))s)"
else
  conformance_status=$?
  echo "FAIL: conformance failed ($(( $(date +%s) - conformance_started ))s)" >&2
fi

if wait "$audit_pid"; then
  audit_status=0
else
  audit_status=$?
fi

if [ "$conformance_status" -ne 0 ]; then
  exit "$conformance_status"
fi
if [ "$audit_status" -ne 0 ]; then
  exit "$audit_status"
fi
echo "=== conformance and audit gates passed ==="
