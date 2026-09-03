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
#   3. Run the process-isolated conformance worker pool and independent audit
#      gates in parallel. The Harn script suite is itself a worker pool, so the
#      audit budget is partitioned between it and GNU make rather than nesting
#      an automatic Harn pool inside an already-full `make -j` pool. `-k` keeps
#      going after a failing ordinary gate so the run reports every verdict.
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
# Usage: scripts/audit_gates.sh [--phase all|conformance|audit] [--tree-sitter-parser-preflighted]
#   --phase all             run both worker pools together (default; local use)
#   --phase conformance     run conformance workers, then the performance ratchet
#   --phase audit           run only the independent audit gate fanout
#   --tree-sitter-parser-preflighted
#                            omit that gate only after the caller ran it on the
#                            same checkout (CI uses this to overlap it with the
#                            shared-binary build)
#   HARN_BIN                 pre-built binary to reuse (skips the warm build)
#   AUDIT_GATES_CONCURRENCY  `make -j` cap (default: nproc minus conformance
#                            workers; see headroom note below). Explicit values
#                            are honored unchanged.
#   HARN_CONFORMANCE_JOBS    process worker count (default: half of nproc,
#                            capped at 4; the other half runs audit gates)
#   HARN_CONFORMANCE_TIMEOUT_MS
#                            per-case timeout for conformance workers
#                            (default: 60000 under this parallel fanout)
set -euo pipefail
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=scripts/lib/source_gate_receipt.sh
source "$SCRIPT_DIR/lib/source_gate_receipt.sh"

gate_command=("$0" "$@")
gate_receipt="${HARN_EXT_GATE_RECEIPT:-$SCRIPT_DIR/../.harn/receipts/source-gate.json}"

phase="all"
tree_sitter_parser_preflighted="false"
while [ "$#" -gt 0 ]; do
  case "$1" in
    --phase)
      [ "$#" -ge 2 ] || { echo "error: --phase requires a value" >&2; exit 2; }
      phase="$2"
      shift 2
      ;;
    --phase=*)
      phase="${1#--phase=}"
      shift
      ;;
    --tree-sitter-parser-preflighted)
      tree_sitter_parser_preflighted="true"
      shift
      ;;
    -h|--help)
      echo "usage: scripts/audit_gates.sh [--phase all|conformance|audit] [--tree-sitter-parser-preflighted]"
      exit 0
      ;;
    *)
      echo "error: unknown argument: $1" >&2
      exit 2
      ;;
  esac
done
case "$phase" in
  all|conformance|audit) ;;
  *)
    echo "error: invalid --phase '$phase' (expected all, conformance, or audit)" >&2
    exit 2
    ;;
esac

gate_temp_root="$(mktemp -d "${TMPDIR:-/tmp}/harn-audit-gates.XXXXXX")"
export PYTHONPYCACHEPREFIX="$gate_temp_root/python-pyc"
audit_log=""
performance_log=""
conformance_log=""
cleanup_files() {
  [ -z "$audit_log" ] || rm -f "$audit_log"
  [ -z "$performance_log" ] || rm -f "$performance_log"
  [ -z "$conformance_log" ] || rm -f "$conformance_log"
  rm -rf "$gate_temp_root"
}
trap cleanup_files EXIT

harn_source_gate_begin "$gate_receipt" "${HARN_EXT_GATE_PR_NUMBER:-}"

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$repo_root"

# Independent gates that reuse the warm harn binary, run concurrently under
# `make -j`. `conformance` is handled separately below so its failure can still
# be reported as the conformance failure rather than hidden inside a make fanout.
GATES=(
  test-agent-scripts
  # Was in `make all` but in no workflow, so nothing watched it: the suite sat
  # red on main for a source-scope regression until someone ran it by hand. A
  # gate that only fires locally is not a gate.
  protocol-conformance
  lint-no-xfail-regression
  lint-actions-harn
  lint-harn
  fmt-harn
  check-highlight
  check-prompt-grammar
  check-grammar-keywords
  check-tree-sitter-parser
  verify-tree-sitter-parse
  check-docs
  check-protocol-artifacts
  check-connector-schemas
  check-harness-migrations
  check-openapi-snapshot
  check-session-bundle-schema
  check-provider-catalog-drift
  check-ported-handler-loc
  check-source-file-lengths
  check-test-target-coverage
  check-python-boundary
  check-harn-syntax-sensitive-scans
  check-loud-boundaries
  check-ci-cache-policy
  check-rust-test-lane-policy
  check-crate-sibling-versions
  check-dependabot-groups
  check-release-audit-contract
  check-vm-rss-soak
)
SCRIPT_TEST_GATE="test-harn-scripts"
if [ "$tree_sitter_parser_preflighted" = "true" ]; then
  filtered_gates=()
  for gate in "${GATES[@]}"; do
    if [ "$gate" != "check-tree-sitter-parser" ]; then
      filtered_gates+=("$gate")
    fi
  done
  GATES=("${filtered_gates[@]}")
fi

nproc_count() { getconf _NPROCESSORS_ONLN 2>/dev/null || echo 4; }
explicit_audit_concurrency="${AUDIT_GATES_CONCURRENCY-}"
concurrency="${explicit_audit_concurrency:-$(nproc_count)}"
case "$concurrency" in
  ''|*[!0-9]*) concurrency="$(nproc_count)" ;;
esac
[ "$concurrency" -lt 1 ] && concurrency=1

processor_count="$(nproc_count)"
default_conformance_jobs=$((processor_count / 2))
[ "$default_conformance_jobs" -lt 1 ] && default_conformance_jobs=1
[ "$default_conformance_jobs" -gt 4 ] && default_conformance_jobs=4
conformance_jobs="${HARN_CONFORMANCE_JOBS:-$default_conformance_jobs}"
case "$conformance_jobs" in
  ''|*[!0-9]*) conformance_jobs="$default_conformance_jobs" ;;
esac
[ "$conformance_jobs" -lt 1 ] && conformance_jobs=1

# Conformance workers and the audit `make -j` fanout share one runner. Process-
# heavy cases (agent_state_resume_process, autonomy_*, trust_graph_*, trigger_*)
# already budget ~30s of internal polling for scheduling starvation; when the
# audit fanout also claims every core, those cases still hit the outer 30s
# per-case timeout while passing in isolation. The default worker count claims
# at most half the cores, and the default audit fanout receives the remainder,
# so the two worker pools never oversubscribe the runner. Explicit worker and
# concurrency values remain absolute so local scripts/tests keep their knobs.
if [ "$phase" = "all" ] && [ -z "$explicit_audit_concurrency" ] && [ "$concurrency" -gt 1 ]; then
  reserved="$conformance_jobs"
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
harn_source_gate_bind_binary "$HARN_BIN"
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
conformance_log="$(mktemp)"
child_pids=()
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
forget_child() {
  local waited_pid="$1"
  local index
  for index in "${!child_pids[@]}"; do
    if [ "${child_pids[$index]}" = "$waited_pid" ]; then
      child_pids[$index]=""
      return
    fi
  done
}
trap 'terminate_children 129' HUP
trap 'terminate_children 130' INT
trap 'terminate_children 143' TERM

audit_status=0
audit_pid=""
script_test_status=0
script_test_pid=""
if [ "$phase" != "conformance" ]; then
  # `test-harn-scripts` recursively starts a Harn worker pool. Letting its
  # automatic pool run as one opaque job inside `make -j$concurrency` expands a
  # four-worker CI budget to as many as eleven processes (eight Harn workers
  # plus three sibling gates). That production-only oversubscription starved a
  # rustfmt child until its honest 30s execute rail fired, even though isolated
  # full-suite runs completed the same case in well under one second.
  #
  # Partition one total audit budget instead: half for the nested script suite,
  # half for ordinary gates. With a one-worker budget, keep the gate in the
  # ordinary serial fanout so even that boundary remains total.
  audit_gates=("${GATES[@]}")
  ordinary_concurrency="$concurrency"
  script_test_jobs=0
  if [ "$concurrency" -gt 1 ]; then
    script_test_jobs=$((concurrency / 2))
    ordinary_concurrency=$((concurrency - script_test_jobs))
  else
    audit_gates=("$SCRIPT_TEST_GATE" "${audit_gates[@]}")
  fi
  : > "$audit_log"

  (
    echo "=== audit gates (make -j$ordinary_concurrency -k${output_sync:+ $output_sync}, HARN_BIN warm) ==="
    gate_started="$(date +%s)"
    if make -j"$ordinary_concurrency" -k ${output_sync} "${audit_gates[@]}" 2>&1 | tee -a "$audit_log"; then
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

  if [ "$script_test_jobs" -gt 0 ]; then
    (
      echo "=== Harn script tests ($script_test_jobs reserved workers, HARN_BIN warm) ==="
      gate_started="$(date +%s)"
      if HARN_TEST_JOBS="$script_test_jobs" make "$SCRIPT_TEST_GATE" 2>&1 | tee -a "$audit_log"; then
        echo "ok: Harn script tests ($(( $(date +%s) - gate_started ))s)"
      else
        status=$?
        echo "FAIL: Harn script tests failed ($(( $(date +%s) - gate_started ))s)" >&2
        exit "$status"
      fi
    ) &
    script_test_pid=$!
    child_pids+=("$script_test_pid")
  fi
fi

conformance_status=0
conformance_pid=""
if [ "$phase" != "audit" ]; then
  conformance_started="$(date +%s)"
  echo "=== conformance ($conformance_jobs process-isolated workers, HARN_BIN warm) ==="
  (
    "$SCRIPT_DIR/harn_test_env.sh" "$HARN_BIN" test conformance \
      --parallel \
      --jobs "$conformance_jobs" \
      --timeout "$conformance_timeout_ms"
  ) >"$conformance_log" 2>&1 &
  conformance_pid=$!
  child_pids+=("$conformance_pid")
fi

if [ -n "$audit_pid" ]; then
  if wait "$audit_pid"; then
    audit_status=0
  else
    audit_status=$?
  fi
  forget_child "$audit_pid"
fi
if [ -n "$script_test_pid" ]; then
  if wait "$script_test_pid"; then
    script_test_status=0
  else
    script_test_status=$?
  fi
  forget_child "$script_test_pid"
fi

# The audit workers own the shared runner budget. Observe them before waiting
# for conformance so an early fanout failure can cancel the still-running test
# process instead of leaving it (or its descendants) blocked after the caller
# exits. `harn_test_env.sh` forwards that cancellation to the actual Harn child.
if [ -n "$conformance_pid" ]; then
  if [ "$audit_status" -ne 0 ] || [ "$script_test_status" -ne 0 ]; then
    kill -TERM "$conformance_pid" 2>/dev/null || true
  fi
  if wait "$conformance_pid"; then
    conformance_status=0
  else
    conformance_status=$?
  fi
  forget_child "$conformance_pid"
  cat "$conformance_log"

  if [ "$conformance_status" -eq 0 ]; then
    echo "ok: conformance ($conformance_jobs workers, $(( $(date +%s) - conformance_started ))s)"
  elif [ "$audit_status" -ne 0 ] || [ "$script_test_status" -ne 0 ]; then
    echo "cancelled: conformance after audit failure ($(( $(date +%s) - conformance_started ))s)" >&2
    # The owning audit failure is reported below; cancellation is not an
    # independent conformance regression.
    conformance_status=0
  else
    echo "FAIL: conformance runner exited $conformance_status ($(( $(date +%s) - conformance_started ))s)" >&2
  fi
fi

# The performance gate is a benchmark, not a source or artifact consistency
# check. Run it only after the conformance workers and audit fanout have settled
# so its resource measurements are comparable to the platform baseline.
performance_status=0
if [ "$phase" != "audit" ] \
  && [ "$conformance_status" -eq 0 ] \
  && [ "$audit_status" -eq 0 ] \
  && [ "$script_test_status" -eq 0 ]; then
  performance_started="$(date +%s)"
  echo "=== test-case performance (isolated, HARN_BIN warm) ==="
  if make check-test-case-performance 2>&1 | tee "$performance_log"; then
    echo "ok: test-case performance ($(( $(date +%s) - performance_started ))s)"
  else
    performance_status=$?
    echo "FAIL: test-case performance ($(( $(date +%s) - performance_started ))s)" >&2
  fi
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
  local failure_phase="$1"
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
    echo "=== FAILED: ${failure_phase} ==="
    if [ -n "${names// /}" ]; then
      echo "failing gate(s): ${names}"
    fi
    echo "The failing output is ABOVE, not at this tail."
    if [ "$phase" = "all" ] && [ "$log_path" = "$audit_log" ]; then
      echo "The audit gates run in parallel with conformance."
    elif [ "$phase" = "all" ]; then
      echo "This phase runs after conformance and the audit fanout."
    else
      echo "This worker runs independently of the other Harn proof worker."
    fi
    echo "Search this log for 'make: *** ' and 'FAIL:'."
    if [ "$phase" = "all" ]; then
      echo "'ok: conformance' near the tail does NOT mean this job passed."
    fi
  } >&2
}

if [ "$conformance_status" -ne 0 ]; then
  failure_summary "conformance"
  exit "$conformance_status"
fi
if [ "$audit_status" -ne 0 ] || [ "$script_test_status" -ne 0 ]; then
  failure_summary "audit gates"
fi
if [ "$performance_status" -ne 0 ]; then
  failure_summary "test-case performance" "$performance_log"
fi
if [ "$audit_status" -ne 0 ]; then
  exit "$audit_status"
fi
if [ "$script_test_status" -ne 0 ]; then
  exit "$script_test_status"
fi
if [ "$performance_status" -ne 0 ]; then
  exit "$performance_status"
fi
case "$phase" in
  all) summary="conformance and audit gates passed" ;;
  conformance) summary="conformance and performance gates passed" ;;
  audit) summary="audit gates passed" ;;
esac
echo "=== $summary ==="
subtask_placement="${HARN_VM_SUBTASK_PLACEMENT:-worker}"
receipt_audit_jobs="$concurrency"
receipt_conformance_jobs="$conformance_jobs"
[[ "$phase" = "conformance" ]] && receipt_audit_jobs="-"
[[ "$phase" = "audit" ]] && receipt_conformance_jobs="-"
harn_source_gate_finish "$gate_receipt" full_io "$subtask_placement" \
  "$receipt_audit_jobs" "$receipt_conformance_jobs" "$summary" "${gate_command[@]}"
echo "ok: source gate receipt ($gate_receipt)"
