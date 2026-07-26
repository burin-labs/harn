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
#   3. Run process-isolated conformance shards and the independent `make -j`
#      audit gates in parallel. GNU make already IS a bounded worker pool with
#      failure collection; `-k` keeps going after a failing gate so the run
#      reports EVERY gate's verdict, not just the first.
#   4. Run the per-test performance ratchet after that fanout. Its workload is
#      intentionally parallel, but its wall/user-time measurements must not be
#      contaminated by the unrelated audit processes running beside it.
#
# Serial wall-clock was `warm build + conformance + max(tail gate)`; parallel
# wall-clock is `warm build + max(conformance, tail gates) + performance gate`
# modulo the `-j` cap. Keeping the benchmark isolated trades a small amount of
# throughput for a measurement that reflects the workload rather than runner
# contention.
#
# Usage: scripts/audit_gates.sh
#   HARN_BIN                 pre-built binary to reuse (skips the warm build)
#   AUDIT_GATES_CONCURRENCY  `make -j` cap (default: nproc minus conformance
#                            shards; see headroom note below). Explicit values
#                            are honored unchanged.
#   HARN_CONFORMANCE_SHARDS  process shard count (default: half of nproc,
#                            capped at 4; the other half runs audit gates)
#   HARN_CONFORMANCE_TIMEOUT_MS
#                            per-case timeout for conformance shards
#                            (default: 60000 under this parallel fanout)
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
  lint-actions-harn
  lint-harn
  fmt-harn
  check-highlight
  check-prompt-grammar
  check-language-spec
  check-grammar-keywords
  verify-tree-sitter-parse
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
  check-openapi-snapshot
  check-session-bundle-schema
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
explicit_audit_concurrency="${AUDIT_GATES_CONCURRENCY-}"
concurrency="${explicit_audit_concurrency:-$(nproc_count)}"
case "$concurrency" in
  ''|*[!0-9]*) concurrency="$(nproc_count)" ;;
esac
[ "$concurrency" -lt 1 ] && concurrency=1

processor_count="$(nproc_count)"
default_shards=$((processor_count / 2))
[ "$default_shards" -lt 1 ] && default_shards=1
[ "$default_shards" -gt 4 ] && default_shards=4
conformance_shards="${HARN_CONFORMANCE_SHARDS:-$default_shards}"
case "$conformance_shards" in
  ''|*[!0-9]*) conformance_shards="$default_shards" ;;
esac
[ "$conformance_shards" -lt 1 ] && conformance_shards=1

# Conformance shards and the audit `make -j` fanout share one runner. Process-
# heavy cases (agent_state_resume_process, autonomy_*, trust_graph_*, trigger_*)
# already budget ~30s of internal polling for scheduling starvation; when the
# audit fanout also claims every core, those cases still hit the outer 30s
# per-case timeout while passing in isolation. The default shard count claims
# at most half the cores, and the default audit fanout receives the remainder,
# so the two worker pools never oversubscribe the runner. Explicit shard and
# concurrency values remain absolute so local scripts/tests keep their knobs.
if [ -z "$explicit_audit_concurrency" ] && [ "$concurrency" -gt 1 ]; then
  reserved="$conformance_shards"
  if [ "$concurrency" -gt "$reserved" ]; then
    concurrency=$((concurrency - reserved))
  else
    concurrency=1
  fi
fi

# Parallel audit load needs a looser outer case budget than the CLI default
# (30s). Helpers in conformance/tests/_common.harn already poll for 30s; the
# case timeout must exceed that under contention. Override with
# HARN_CONFORMANCE_TIMEOUT_MS when needed.
conformance_timeout_ms="${HARN_CONFORMANCE_TIMEOUT_MS:-60000}"
case "$conformance_timeout_ms" in
  ''|*[!0-9]*) conformance_timeout_ms=60000 ;;
esac
[ "$conformance_timeout_ms" -lt 1000 ] && conformance_timeout_ms=60000

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
# Performance checks must use the already-authoritative binary too. Without
# this alias the RSS soak asks Cargo for a target path and can sit behind an
# unrelated parallel Cargo lock for minutes before running a 250 ms benchmark.
export HARN_CHECK_BIN="${HARN_CHECK_BIN:-$HARN_BIN}"
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

# Capture the audit gates' output as well as streaming it, so the summary at the
# end can NAME the gates that failed. `set -o pipefail` above is what makes this
# safe: without it the pipe would report tee's status and every audit failure
# would read as a pass.
audit_log="$(mktemp)"
performance_log="$(mktemp)"
conformance_log_dir="$(mktemp -d)"
child_pids=()
cleanup_files() {
  rm -f "$audit_log"
  rm -f "$performance_log"
  rm -rf "$conformance_log_dir"
}
terminate_children() {
  local status="$1"
  local pid
  for pid in "${child_pids[@]}"; do
    [ -z "$pid" ] && continue
    kill "$pid" 2>/dev/null || true
  done
  for pid in "${child_pids[@]}"; do
    [ -z "$pid" ] && continue
    wait "$pid" 2>/dev/null || true
  done
  exit "$status"
}
trap cleanup_files EXIT
trap 'terminate_children 130' INT
trap 'terminate_children 143' TERM

audit_status=0
(
  echo "=== audit gates (make -j$concurrency -k${output_sync:+ $output_sync}, HARN_BIN warm) ==="
  gate_started="$(date +%s)"
  if make -j"$concurrency" -k ${output_sync} "${GATES[@]}" 2>&1 | tee "$audit_log"; then
    echo "ok: audit gates ($(( $(date +%s) - gate_started ))s)"
    echo "=== all audit gates passed ==="
  else
    status=$?
    echo "FAIL: one or more audit gates failed ($(( $(date +%s) - gate_started ))s)" >&2
    exit "$status"
  fi
) &
audit_pid=$!
child_pids+=("$audit_pid")

conformance_status=0
conformance_started="$(date +%s)"
echo "=== conformance ($conformance_shards process-isolated shards, HARN_BIN warm) ==="
conformance_pids=()
for shard_index in $(seq 1 "$conformance_shards"); do
  shard_log="$conformance_log_dir/shard-$shard_index.log"
  (
    "$SCRIPT_DIR/harn_test_env.sh" "$HARN_BIN" test conformance \
      --timeout "$conformance_timeout_ms" \
      --shard-index "$shard_index" \
      --shard-total "$conformance_shards"
  ) >"$shard_log" 2>&1 &
  shard_pid=$!
  conformance_pids+=("$shard_pid")
  child_pids+=("$shard_pid")
done

conformance_failures=()
for offset in "${!conformance_pids[@]}"; do
  shard_index=$((offset + 1))
  shard_pid="${conformance_pids[$offset]}"
  if wait "$shard_pid"; then
    shard_status=0
  else
    shard_status=$?
    conformance_status="$shard_status"
    conformance_failures+=("$shard_index:$shard_status")
  fi
  child_pids[offset + 1]=""
  echo "=== conformance shard $shard_index/$conformance_shards ==="
  cat "$conformance_log_dir/shard-$shard_index.log"
done

if [ "$conformance_status" -eq 0 ]; then
  echo "ok: conformance ($conformance_shards shards, $(( $(date +%s) - conformance_started ))s)"
else
  echo "FAIL: conformance shard(s) ${conformance_failures[*]} failed ($(( $(date +%s) - conformance_started ))s)" >&2
fi

if wait "$audit_pid"; then
  audit_status=0
else
  audit_status=$?
fi
child_pids[0]=""

# The performance gate is a benchmark, not a source or artifact consistency
# check. Run it only after the conformance shards and audit fanout have settled
# so its resource measurements are comparable to the platform baseline.
performance_status=0
performance_started="$(date +%s)"
echo "=== test-case performance (isolated, HARN_BIN warm) ==="
if make check-test-case-performance 2>&1 | tee "$performance_log"; then
  echo "ok: test-case performance ($(( $(date +%s) - performance_started ))s)"
else
  performance_status=$?
  echo "FAIL: test-case performance ($(( $(date +%s) - performance_started ))s)" >&2
fi

# Say at the TAIL what failed, because the tail is where a reader looks.
#
# Conformance and the audit gates run in PARALLEL, so a failing gate prints its
# `make: *** [target] Error N` minutes before this point and thousands of lines
# above it. The tail then reads "ok: conformance" followed by a bare non-zero
# exit — which is byte-identical to the documented sccache post-job flake
# (ci.yml: "non-zero JOB exit (exit 2) AFTER every gate already passed"). A real
# two-gate failure has been misread as that flake. A log that makes a real
# failure look like a known flake is a defect even when every gate is correct.
failure_summary() {
  local phase="$1"
  local log_path="${2:-$audit_log}"
  local names
  # `|| true`: a gate can fail WITHOUT emitting `make: *** [target]` (a bare
  # non-zero, a killed process). Under `set -euo pipefail` an empty grep would
  # abort this function and print nothing at all — the silence this summary
  # exists to end. Name what we can; always print the rest.
  names="$(grep -oE 'make: \*\*\* \[[^]]+\]' "$log_path" 2>/dev/null \
    | sed -E 's/.*: ([^]]+)\]/\1/' | sort -u | tr '\n' ' ' || true)"
  {
    echo ""
    echo "=== FAILED: ${phase} ==="
    if [ -n "${names// /}" ]; then
      echo "failing gate(s): ${names}"
    fi
    echo "The failing output is ABOVE, not at this tail."
    if [ "$log_path" = "$audit_log" ]; then
      echo "The audit gates run in parallel with conformance."
    else
      echo "This phase runs after conformance and the audit fanout."
    fi
    echo "Search this log for 'make: *** ' and 'FAIL:'."
    echo "'ok: conformance' near the tail does NOT mean this job passed."
  } >&2
}

if [ "$conformance_status" -ne 0 ]; then
  failure_summary "conformance"
  exit "$conformance_status"
fi
if [ "$audit_status" -ne 0 ]; then
  failure_summary "audit gates"
fi
if [ "$performance_status" -ne 0 ]; then
  failure_summary "test-case performance" "$performance_log"
fi
if [ "$audit_status" -ne 0 ]; then
  exit "$audit_status"
fi
if [ "$performance_status" -ne 0 ]; then
  exit "$performance_status"
fi
echo "=== conformance and audit gates passed ==="
