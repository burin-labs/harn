#!/usr/bin/env bash
set -euo pipefail

repo_root=$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)

tmp_root=$(mktemp -d)
trap 'rm -rf "$tmp_root"' EXIT

fake_bin="$tmp_root/bin"
record="$tmp_root/make-record.txt"
conformance_start_fifo_dir="$tmp_root/conformance-started"
runtime_tmp="$tmp_root/runtime-tmp"
mkdir -p "$fake_bin"
mkdir -p "$conformance_start_fifo_dir"
mkdir -p "$runtime_tmp"
mkfifo "$conformance_start_fifo_dir/runner"

fake_harn="$fake_bin/harn"
cat > "$fake_harn" <<'SH'
#!/usr/bin/env bash
set -euo pipefail
if [[ "${1:-}" = "__internal-source-gate-receipt-v1" && "${2:-}" = "write" ]]; then
  mkdir -p "$(dirname "$3")"
  printf '{"test_double":true}\n' > "$3"
  exit 0
fi
invocation="$*"
printf '%s\t%s\tSESSION_STORE=%s\n' "$invocation" "HARN_BIN=$0" "$HARN_SESSION_STORE_ROOT" >> "$FAKE_CONFORMANCE_RECORD"
trap 'if [[ -n "${FAKE_CHILD_TERMINATED_RECORD-}" ]]; then printf "terminated\n" >> "$FAKE_CHILD_TERMINATED_RECORD"; fi; exit 143' TERM
if [[ -z "${HARN_SESSION_STORE_ROOT-}" || ! -d "$HARN_SESSION_STORE_ROOT" ]]; then
  echo "conformance runner did not receive an isolated session store" >&2
  exit 46
fi
if [[ "${FAKE_SPLIT_PHASE-0}" != "1" ]]; then
  if [[ -n "${FAKE_CONFORMANCE_READY_FIFO-}" ]]; then
    printf 'ready\n' > "$FAKE_CONFORMANCE_READY_FIFO"
  fi
  # Opening the FIFO blocks until the audit fanout has reached its own barrier;
  # this proves ordering without a wall-clock polling loop.
  printf 'conformance-started\n' > "$FAKE_CONFORMANCE_START_FIFO_DIR/runner"
  if [[ ! -f "$FAKE_AUDIT_ROOT/audit.started" ]]; then
    echo "audit gates were not started before conformance completed" >&2
    exit 41
  fi
fi
if [[ "${FAKE_CONFORMANCE_FAIL-0}" == "1" ]]; then
  echo "fake conformance runner failed" >&2
  exit 44
fi
printf 'fake conformance runner ok\n'
SH
chmod +x "$fake_harn"

export GITHUB_ACTIONS=true
export SOURCE_GATE_CI_BINARY_COMMIT="$(git -C "$repo_root" rev-parse --verify HEAD)"
export SOURCE_GATE_CI_BINARY_BUILD_FRESHNESS_ID="$(printf 'a%.0s' {1..40})"
if command -v sha256sum >/dev/null 2>&1; then
  SOURCE_GATE_CI_BINARY_SHA256="$(sha256sum "$fake_harn" | cut -d ' ' -f 1)"
else
  SOURCE_GATE_CI_BINARY_SHA256="$(shasum -a 256 "$fake_harn" | cut -d ' ' -f 1)"
fi
export SOURCE_GATE_CI_BINARY_SHA256

cat > "$fake_bin/make" <<'SH'
#!/usr/bin/env bash
set -euo pipefail

if [[ "${1-}" == "--help" ]]; then
  printf 'GNU Make test double\n  --output-sync\n'
  exit 0
fi

if [[ -z "${PYTHONPYCACHEPREFIX-}" ]]; then
  echo "audit gate did not export PYTHONPYCACHEPREFIX" >&2
  exit 50
fi
mkdir -p "$PYTHONPYCACHEPREFIX"
printf 'positive-fire\n' > "$PYTHONPYCACHEPREFIX/fake-check.pyc"
printf 'invocation\t%s\tHARN_BIN=%s\tHARN_TEST_JOBS=%s\tPYTHONPYCACHEPREFIX=%s\n' \
  "$*" "${HARN_BIN-__unset__}" "${HARN_TEST_JOBS-__unset__}" \
  "$PYTHONPYCACHEPREFIX" >> "$FAKE_AUDIT_RECORD"

case "$*" in
  test-harn-scripts)
    if [[ "${FAKE_SCRIPT_TEST_FAIL-0}" == "1" ]]; then
      echo "make: *** [Makefile:552: test-harn-scripts] Error 48" >&2
      exit 48
    fi
    printf 'fake Harn script suite ok\n'
    exit 0
    ;;
  check-test-case-performance)
    if [[ "$(wc -l < "$FAKE_CONFORMANCE_RECORD" | tr -d ' ')" != "1" ]]; then
      echo "performance gate started before conformance completed" >&2
      exit 45
    fi
    if [[ "${FAKE_PERFORMANCE_FAIL-0}" == "1" ]]; then
      echo "fake performance gate failed" >&2
      exit 47
    fi
    printf 'fake performance gate ok\n'
    exit 0
    ;;
  -j2\ -k\ -Otarget\ *|-j1\ -k\ -Otarget\ *)
    if [[ "${FAKE_AUDIT_FAIL_BEFORE_BARRIER-0}" == "1" ]]; then
      exec 4< "$FAKE_CONFORMANCE_READY_FIFO"
      IFS= read -r <&4
      exec 4<&-
      echo "make: *** [Makefile:314: fmt-harn] Error 49" >&2
      exit 49
    fi
    touch "$FAKE_AUDIT_ROOT/audit.started"
    if [[ "${FAKE_SPLIT_PHASE-0}" != "1" ]]; then
      exec 3< "$FAKE_CONFORMANCE_START_FIFO_DIR/runner"
      IFS= read -r <&3
      exec 3<&-
    fi
    if [[ "${FAKE_AUDIT_FAIL-0}" == "1" ]]; then
      # Exactly how GNU make reports a failing target under `-k`, because the
      # failure summary parses these lines to name the gates.
      echo "make: *** [Makefile:314: fmt-harn] Error 123" >&2
      echo "make: *** [Makefile:705: check-source-file-lengths] Error 1" >&2
      echo "fake audit gates failed" >&2
      exit 43
    fi
    printf 'fake audit gates ok\n'
    exit 0
    ;;
  *)
    echo "unexpected make invocation: $*" >&2
    exit 42
    ;;
esac
SH
chmod +x "$fake_bin/make"

AUDIT_GATES_CONCURRENCY=4 \
  HARN_CONFORMANCE_JOBS=3 \
  HARN_BIN="$fake_harn" \
  FAKE_AUDIT_RECORD="$record" \
  FAKE_CONFORMANCE_RECORD="$tmp_root/conformance-record.txt" \
  FAKE_AUDIT_ROOT="$tmp_root" \
  FAKE_CONFORMANCE_START_FIFO_DIR="$conformance_start_fifo_dir" \
  TMPDIR="$runtime_tmp" \
  PATH="$fake_bin:$PATH" \
  "$repo_root/scripts/audit_gates.sh" > "$tmp_root/audit.out"

pycache_root="$(awk -F '\t' 'NR == 1 { sub(/^PYTHONPYCACHEPREFIX=/, "", $5); print $5 }' "$record")"
case "$pycache_root" in
  "$runtime_tmp"/harn-audit-gates.*/python-pyc) ;;
  *)
    echo "audit gate did not place Python bytecode under its exact runtime temp root: $pycache_root" >&2
    exit 1
    ;;
esac
gate_runtime_root="${pycache_root%/python-pyc}"
if [[ -e "$gate_runtime_root" ]]; then
  echo "audit gate leaked its positive-fire runtime root: $gate_runtime_root" >&2
  exit 1
fi

if ! grep -Fxq "ok: harn-bin ($fake_harn)" "$tmp_root/audit.out"; then
  echo "audit_gates did not reuse the explicit HARN_BIN" >&2
  cat "$tmp_root/audit.out" >&2
  exit 1
fi
if ! grep -Fxq "=== conformance and audit gates passed ===" "$tmp_root/audit.out"; then
  echo "audit_gates did not report the combined success marker" >&2
  cat "$tmp_root/audit.out" >&2
  exit 1
fi
if [[ "$(wc -l < "$tmp_root/conformance-record.txt" | tr -d ' ')" != "1" ]]; then
  echo "conformance did not run through one canonical runner" >&2
  cat "$tmp_root/conformance-record.txt" >&2
  exit 1
fi
while IFS= read -r session_field; do
  session_store="${session_field#SESSION_STORE=}"
  if [[ -e "$session_store" ]]; then
    echo "conformance runner leaked its isolated session store: $session_store" >&2
    exit 1
  fi
done < <(awk -F '\t' '{print $3}' "$tmp_root/conformance-record.txt")
expected="test conformance --parallel --jobs 3 --timeout 60000"
if ! grep -Fq "$expected" "$tmp_root/conformance-record.txt"; then
  echo "canonical conformance invocation missing: $expected" >&2
  cat "$tmp_root/conformance-record.txt" >&2
  exit 1
fi
if ! grep -Fq $'\t-j2 -k -Otarget ' "$record"; then
  echo "audit gates did not reserve a worker for the nested script suite" >&2
  cat "$record" >&2
  exit 1
fi
if ! grep -Fq $'invocation\ttest-harn-scripts\t' "$record" \
  || ! grep -Fq $'HARN_TEST_JOBS=2' "$record"; then
  echo "Harn script suite did not receive its reserved share of the audit budget" >&2
  cat "$record" >&2
  exit 1
fi
if ! grep -Fq "check-ported-handler-loc" "$record"; then
  echo "required audit fanout omitted the ported-handler LOC ratchet" >&2
  cat "$record" >&2
  exit 1
fi
if ! grep -Fq "lint-actions-harn" "$record"; then
  echo "required audit fanout omitted the Harn-backed Actions lint" >&2
  cat "$record" >&2
  exit 1
fi
if ! grep -Fq -- "- run: make lint-actions-source" "$repo_root/.github/workflows/ci.yml"; then
  echo "Actions hygiene job must use the source-only lint target" >&2
  exit 1
fi
if grep -Eq -- '^[[:space:]]*- run: make lint-actions[[:space:]]*$' "$repo_root/.github/workflows/ci.yml"; then
  echo "Actions hygiene job must not invoke the compile-bearing aggregate target" >&2
  exit 1
fi
if ! grep -Fq $'invocation\tcheck-test-case-performance' "$record"; then
  echo "performance gate did not run as an isolated phase" >&2
  cat "$record" >&2
  exit 1
fi

# CI consumes the same warm binary from two independent matrix workers. Each
# phase must be complete by itself and must not quietly execute its sibling's
# workload (which would restore the CPU contention this split removes).
split_conformance_record="$tmp_root/split-conformance-record.txt"
split_conformance_make_record="$tmp_root/split-conformance-make-record.txt"
FAKE_SPLIT_PHASE=1 \
  AUDIT_GATES_CONCURRENCY=3 \
  HARN_CONFORMANCE_JOBS=3 \
  HARN_BIN="$fake_harn" \
  FAKE_AUDIT_RECORD="$split_conformance_make_record" \
  FAKE_CONFORMANCE_RECORD="$split_conformance_record" \
  FAKE_AUDIT_ROOT="$tmp_root" \
  FAKE_CONFORMANCE_START_FIFO_DIR="$conformance_start_fifo_dir" \
  PATH="$fake_bin:$PATH" \
  "$repo_root/scripts/audit_gates.sh" --phase conformance > "$tmp_root/split-conformance.out"
if [[ "$(wc -l < "$split_conformance_record" | tr -d ' ')" != "1" ]]; then
  echo "conformance worker did not use one canonical runner" >&2
  exit 1
fi
if ! grep -Fq $'invocation\tcheck-test-case-performance' "$split_conformance_make_record"; then
  echo "conformance worker omitted its isolated performance ratchet" >&2
  exit 1
fi
if grep -Fq $'\t-j3 -k -Otarget ' "$split_conformance_make_record"; then
  echo "conformance worker also ran the audit fanout" >&2
  exit 1
fi
if ! grep -Fxq "=== conformance and performance gates passed ===" "$tmp_root/split-conformance.out"; then
  echo "conformance worker omitted its terminal marker" >&2
  exit 1
fi

split_audit_record="$tmp_root/split-audit-record.txt"
split_audit_conformance_record="$tmp_root/split-audit-conformance-record.txt"
: > "$split_audit_conformance_record"
FAKE_SPLIT_PHASE=1 \
  AUDIT_GATES_CONCURRENCY=4 \
  HARN_CONFORMANCE_JOBS=3 \
  HARN_BIN="$fake_harn" \
  FAKE_AUDIT_RECORD="$split_audit_record" \
  FAKE_CONFORMANCE_RECORD="$split_audit_conformance_record" \
  FAKE_AUDIT_ROOT="$tmp_root" \
  FAKE_CONFORMANCE_START_FIFO_DIR="$conformance_start_fifo_dir" \
  PATH="$fake_bin:$PATH" \
  "$repo_root/scripts/audit_gates.sh" --phase audit > "$tmp_root/split-audit.out"
if [[ -s "$split_audit_conformance_record" ]]; then
  echo "audit worker also ran conformance" >&2
  exit 1
fi
if ! grep -Fq $'\t-j2 -k -Otarget ' "$split_audit_record"; then
  echo "audit worker omitted its bounded ordinary-gate fanout" >&2
  exit 1
fi
if ! grep -Fq $'invocation\ttest-harn-scripts\t' "$split_audit_record" \
  || ! grep -Fq $'HARN_TEST_JOBS=2' "$split_audit_record"; then
  echo "split audit worker omitted the bounded script-test pool" >&2
  exit 1
fi
if grep -Fq $'invocation\tcheck-test-case-performance' "$split_audit_record"; then
  echo "audit worker also ran the performance ratchet" >&2
  exit 1
fi
if ! grep -Fxq "=== audit gates passed ===" "$tmp_root/split-audit.out"; then
  echo "audit worker omitted its terminal marker" >&2
  exit 1
fi

preflighted_audit_record="$tmp_root/preflighted-audit-record.txt"
: > "$preflighted_audit_record"
FAKE_SPLIT_PHASE=1 \
  AUDIT_GATES_CONCURRENCY=3 \
  HARN_CONFORMANCE_JOBS=3 \
  HARN_BIN="$fake_harn" \
  FAKE_AUDIT_RECORD="$preflighted_audit_record" \
  FAKE_CONFORMANCE_RECORD="$split_audit_conformance_record" \
  FAKE_AUDIT_ROOT="$tmp_root" \
  FAKE_CONFORMANCE_START_FIFO_DIR="$conformance_start_fifo_dir" \
  PATH="$fake_bin:$PATH" \
  "$repo_root/scripts/audit_gates.sh" --phase audit \
    --tree-sitter-parser-preflighted > "$tmp_root/preflighted-audit.out"
if grep -Fq "check-tree-sitter-parser" "$preflighted_audit_record"; then
  echo "preflighted audit repeated the generated parser check" >&2
  exit 1
fi

# Default (unset) AUDIT_GATES_CONCURRENCY must reserve cores for conformance
# workers so process-heavy cases are not starved by the make -j fanout.
: > "$record"
rm -f "$tmp_root/audit.started"
# Fake nproc=4 via a PATH wrapper around getconf would be brittle; instead
# force a high explicit-unset path by patching through env that the script
# treats as unset. We simulate by calling with AUDIT_GATES_CONCURRENCY unset
# and HARN_CONFORMANCE_JOBS=3 while injecting a getconf that reports 4.
fake_getconf="$fake_bin/getconf"
cat > "$fake_getconf" <<'SH'
#!/usr/bin/env bash
if [[ "${1-}" == "_NPROCESSORS_ONLN" ]]; then
  echo 4
  exit 0
fi
command getconf "$@"
SH
chmod +x "$fake_getconf"
unset AUDIT_GATES_CONCURRENCY
HARN_CONFORMANCE_JOBS=3 \
  HARN_BIN="$fake_harn" \
  FAKE_AUDIT_RECORD="$record" \
  FAKE_CONFORMANCE_RECORD="$tmp_root/conformance-reserve-record.txt" \
  FAKE_AUDIT_ROOT="$tmp_root" \
  FAKE_CONFORMANCE_START_FIFO_DIR="$conformance_start_fifo_dir" \
  PATH="$fake_bin:$PATH" \
  "$repo_root/scripts/audit_gates.sh" > "$tmp_root/audit-reserve.out"
if ! grep -Fq $'\t-j1 -k -Otarget ' "$record"; then
  echo "default audit concurrency did not reserve cores for conformance workers" >&2
  cat "$record" >&2
  cat "$tmp_root/audit-reserve.out" >&2
  exit 1
fi

: > "$record"
rm -f "$tmp_root/audit.started"
if AUDIT_GATES_CONCURRENCY=3 \
  HARN_CONFORMANCE_JOBS=3 \
  HARN_BIN="$fake_harn" \
  FAKE_AUDIT_RECORD="$record" \
  FAKE_CONFORMANCE_RECORD="$tmp_root/conformance-fail-audit-record.txt" \
  FAKE_AUDIT_ROOT="$tmp_root" \
  FAKE_CONFORMANCE_START_FIFO_DIR="$conformance_start_fifo_dir" \
  FAKE_AUDIT_FAIL=1 \
  PATH="$fake_bin:$PATH" \
  "$repo_root/scripts/audit_gates.sh" > "$tmp_root/audit-fail.out" 2>&1; then
  echo "audit_gates masked a failing audit fanout" >&2
  cat "$tmp_root/audit-fail.out" >&2
  exit 1
fi
failed_pycache_root="$(awk -F '\t' 'NR == 1 { sub(/^PYTHONPYCACHEPREFIX=/, "", $5); print $5 }' "$record")"
if [[ -z "$failed_pycache_root" ]]; then
  echo "failing audit gate did not exercise the Python bytecode path" >&2
  exit 1
fi
failed_gate_runtime_root="${failed_pycache_root%/python-pyc}"
if [[ -e "$failed_gate_runtime_root" ]]; then
  echo "failing audit gate leaked its runtime root: $failed_gate_runtime_root" >&2
  exit 1
fi

: > "$record"
rm -f "$tmp_root/audit.started"
if AUDIT_GATES_CONCURRENCY=3 \
  HARN_CONFORMANCE_JOBS=3 \
  HARN_BIN="$fake_harn" \
  FAKE_AUDIT_RECORD="$record" \
  FAKE_CONFORMANCE_RECORD="$tmp_root/conformance-fail-record.txt" \
  FAKE_CONFORMANCE_FAIL=1 \
  FAKE_AUDIT_ROOT="$tmp_root" \
  FAKE_CONFORMANCE_START_FIFO_DIR="$conformance_start_fifo_dir" \
  PATH="$fake_bin:$PATH" \
  "$repo_root/scripts/audit_gates.sh" > "$tmp_root/conformance-fail.out" 2>&1; then
  echo "audit_gates masked a failing conformance runner" >&2
  cat "$tmp_root/conformance-fail.out" >&2
  exit 1
fi
if ! grep -Fq "FAIL: conformance runner exited 44" "$tmp_root/conformance-fail.out"; then
  echo "audit_gates did not identify the failing conformance runner status" >&2
  cat "$tmp_root/conformance-fail.out" >&2
  exit 1
fi
if ! grep -Fq "=== FAILED: audit gates ===" "$tmp_root/audit-fail.out"; then
  echo "audit_gates did not report the failing audit fanout" >&2
  cat "$tmp_root/audit-fail.out" >&2
  exit 1
fi

: > "$record"
rm -f "$tmp_root/audit.started"
if AUDIT_GATES_CONCURRENCY=3 \
  HARN_CONFORMANCE_JOBS=3 \
  HARN_BIN="$fake_harn" \
  FAKE_AUDIT_RECORD="$record" \
  FAKE_CONFORMANCE_RECORD="$tmp_root/performance-fail-record.txt" \
  FAKE_AUDIT_ROOT="$tmp_root" \
  FAKE_CONFORMANCE_START_FIFO_DIR="$conformance_start_fifo_dir" \
  FAKE_PERFORMANCE_FAIL=1 \
  PATH="$fake_bin:$PATH" \
  "$repo_root/scripts/audit_gates.sh" > "$tmp_root/performance-fail.out" 2>&1; then
  echo "audit_gates masked a failing performance phase" >&2
  cat "$tmp_root/performance-fail.out" >&2
  exit 1
fi
if ! grep -Fq "=== FAILED: test-case performance ===" "$tmp_root/performance-fail.out"; then
  echo "audit_gates did not report the failing performance phase" >&2
  cat "$tmp_root/performance-fail.out" >&2
  exit 1
fi

# The TAIL must name what failed.
#
# Conformance and the audit gates run in parallel, so a failing gate prints its
# `make: *** [target] Error N` long before the end. Without a summary the tail
# reads "ok: conformance" plus a bare non-zero exit — byte-identical to the
# sccache post-job flake documented in ci.yml, and a real two-gate failure has
# been misread as that flake. The tail is where a reader looks, so the tail must
# not lie by omission.
tail_out="$(tail -8 "$tmp_root/audit-fail.out")"
if ! grep -Fq "=== FAILED: audit gates ===" <<<"$tail_out"; then
  echo "the tail does not say the job failed; it ends on 'ok: conformance' and an exit code" >&2
  cat "$tmp_root/audit-fail.out" >&2
  exit 1
fi
for gate in fmt-harn check-source-file-lengths; do
  if ! grep -Fq "$gate" <<<"$tail_out"; then
    echo "the failure summary does not name the failing gate: $gate" >&2
    cat "$tmp_root/audit-fail.out" >&2
    exit 1
  fi
done
if ! grep -Fq "does NOT mean this job passed" <<<"$tail_out"; then
  echo "the tail does not warn that a passing conformance line is not a passing job" >&2
  exit 1
fi

# If the ordinary fanout fails before the conformance-side barrier is opened,
# the runner must cancel and reap the Harn child. This is the exact shape that
# previously left a FIFO-blocked child reparented to PID 1 after the wrapper's
# temporary directory had already been removed.
early_failure_termination="$tmp_root/early-failure-termination.txt"
early_failure_ready_fifo="$tmp_root/early-failure-ready"
: > "$early_failure_termination"
: > "$tmp_root/early-failure-conformance-record.txt"
mkfifo "$early_failure_ready_fifo"
if AUDIT_GATES_CONCURRENCY=3 \
  HARN_CONFORMANCE_JOBS=3 \
  HARN_BIN="$fake_harn" \
  FAKE_AUDIT_RECORD="$tmp_root/early-failure-audit-record.txt" \
  FAKE_CONFORMANCE_RECORD="$tmp_root/early-failure-conformance-record.txt" \
  FAKE_CHILD_TERMINATED_RECORD="$early_failure_termination" \
  FAKE_CONFORMANCE_READY_FIFO="$early_failure_ready_fifo" \
  FAKE_AUDIT_ROOT="$tmp_root" \
  FAKE_CONFORMANCE_START_FIFO_DIR="$conformance_start_fifo_dir" \
  FAKE_AUDIT_FAIL_BEFORE_BARRIER=1 \
  PATH="$fake_bin:$PATH" \
  "$repo_root/scripts/audit_gates.sh" > "$tmp_root/early-failure.out" 2>&1; then
  echo "audit_gates masked an early ordinary-gate failure" >&2
  exit 1
fi
if ! grep -Fxq "terminated" "$early_failure_termination"; then
  echo "audit_gates did not cancel and reap the blocked conformance child" >&2
  cat "$tmp_root/early-failure.out" >&2
  exit 1
fi

# A PASSING run must not print a failure summary — a warning that fires on green
# is noise, and noise is how a real warning stops being read.
if grep -Fq "=== FAILED:" "$tmp_root/audit.out"; then
  echo "a passing run printed a failure summary" >&2
  exit 1
fi

echo "audit_gates_parallel_test: ok"
