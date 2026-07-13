#!/usr/bin/env bash
# Parallel runner for the Harn conformance + audit gate battery.
#
# The CI "Harn conformance + audit" lane historically ran a long SERIAL chain
# of `make` targets. Every target after `make conformance` reuses the same
# already-warm `target/debug/harn` binary and is INDEPENDENT of its siblings,
# yet they ran one after another — and with `HARN_BIN` unset each `cargo run`
# also re-walked cargo's build graph on every invocation.
#
# This mirrors the proven shape of `release_gate.sh audit`:
#   1. Build the harn CLI ONCE up front (the serial long pole) and run the
#      conformance suite.
#   2. Export HARN_BIN so no downstream gate pays a cargo staleness check.
#   3. Hand the independent gates to `make -j` — GNU make already IS a bounded
#      worker pool with failure collection. `-k` keeps going after a failing
#      gate so the run reports EVERY gate's verdict, not just the first.
#
# Serial wall-clock was `conformance + sum(tail gates)`; parallel wall-clock is
# `conformance + max(tail gate)` modulo the `-j` cap, collapsing the tail from
# a sum to roughly its longest single gate.
#
# Usage: scripts/audit_gates.sh
#   HARN_BIN                 pre-built binary to reuse (skips the warm build)
#   AUDIT_GATES_CONCURRENCY  `make -j` cap (default: nproc)
set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$repo_root"

# Independent gates that reuse the warm harn binary, run concurrently under
# `make -j`. `conformance` is handled separately below: it defines the warm
# binary and runs the conformance suite, so it must complete first.
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
  check-source-file-lengths
  check-python-boundary
  check-harn-syntax-sensitive-scans
  check-crate-sibling-versions
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

# ── Phase 1: warm build + conformance suite (the serial long pole). ──
started="$(date +%s)"
echo "=== conformance (warm build + conformance suite) ==="
make conformance
echo "ok: conformance ($(( $(date +%s) - started ))s)"

# Resolve and export the warm binary so no downstream gate re-runs cargo.
if [ -z "${HARN_BIN:-}" ]; then
  target_dir="${CARGO_TARGET_DIR:-}"
  if [ -z "$target_dir" ]; then
    target_dir="$(cargo metadata --format-version=1 --no-deps \
      | python3 -c 'import json,sys; print(json.load(sys.stdin)["target_directory"])')"
  fi
  suffix=""
  case "${OS:-$(uname -s)}" in
    Windows_NT|MINGW*|MSYS*|CYGWIN*) suffix=".exe" ;;
  esac
  HARN_BIN="$target_dir/debug/harn$suffix"
fi
if [ ! -x "$HARN_BIN" ]; then
  echo "error: conformance completed but HARN_BIN is not executable: $HARN_BIN" >&2
  exit 1
fi
export HARN_BIN
echo "ok: harn-bin ($HARN_BIN)"

# ── Phase 2: independent gates via make -j (bounded worker pool). ──
# `-k` (keep-going) runs every gate even when one fails, then make exits
# non-zero — so CI sees the full verdict set, not just the first failure.
# `-O` (--output-sync) groups each gate's output so parallel logs don't
# interleave; it needs GNU Make >= 4.0 (ubuntu-latest ships 4.3), so it is
# added only when supported to keep the script portable to macOS's Make 3.81.
output_sync=""
if make --help 2>&1 | grep -q -- "--output-sync"; then
  output_sync="-Otarget"
fi
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
