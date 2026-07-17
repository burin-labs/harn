#!/usr/bin/env bash
set -euo pipefail

repo_root=$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)

tmp_root=$(mktemp -d)
trap 'rm -rf "$tmp_root"' EXIT

fake_bin="$tmp_root/bin"
record="$tmp_root/make-record.txt"
mkdir -p "$fake_bin"

fake_harn="$fake_bin/harn"
cat > "$fake_harn" <<'SH'
#!/usr/bin/env bash
set -euo pipefail
printf 'fake harn\n'
SH
chmod +x "$fake_harn"

cat > "$fake_bin/make" <<'SH'
#!/usr/bin/env bash
set -euo pipefail

if [[ "${1-}" == "--help" ]]; then
  printf 'GNU Make test double\n  --output-sync\n'
  exit 0
fi

printf '%s\t%s\tHARN_BIN=%s\n' "$(date +%s%N)" "$*" "${HARN_BIN-__unset__}" >> "$FAKE_AUDIT_RECORD"

if [[ "$*" == "conformance" ]]; then
  for _ in 1 2 3 4 5 6 7 8 9 10; do
    if [[ -f "$FAKE_AUDIT_ROOT/audit.started" ]]; then
      printf 'fake conformance ok\n'
      exit 0
    fi
    sleep 0.05
  done
  echo "audit gates were not started before conformance completed" >&2
  exit 41
fi

case "$*" in
  -j3\ -k\ -Otarget\ *)
    touch "$FAKE_AUDIT_ROOT/audit.started"
    sleep 0.1
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

AUDIT_GATES_CONCURRENCY=3 \
  HARN_BIN="$fake_harn" \
  FAKE_AUDIT_RECORD="$record" \
  FAKE_AUDIT_ROOT="$tmp_root" \
  PATH="$fake_bin:$PATH" \
  "$repo_root/scripts/audit_gates.sh" > "$tmp_root/audit.out"

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
if ! grep -Fq $'\tconformance\t'"HARN_BIN=$fake_harn" "$record"; then
  echo "conformance did not receive the warmed HARN_BIN" >&2
  cat "$record" >&2
  exit 1
fi
if ! grep -Fq $'\t-j3 -k -Otarget ' "$record"; then
  echo "audit gates did not use the configured make fanout" >&2
  cat "$record" >&2
  exit 1
fi
if ! grep -Fq "check-ported-handler-loc" "$record"; then
  echo "required audit fanout omitted the ported-handler LOC ratchet" >&2
  cat "$record" >&2
  exit 1
fi

: > "$record"
rm -f "$tmp_root/audit.started"
if AUDIT_GATES_CONCURRENCY=3 \
  HARN_BIN="$fake_harn" \
  FAKE_AUDIT_RECORD="$record" \
  FAKE_AUDIT_ROOT="$tmp_root" \
  FAKE_AUDIT_FAIL=1 \
  PATH="$fake_bin:$PATH" \
  "$repo_root/scripts/audit_gates.sh" > "$tmp_root/audit-fail.out" 2>&1; then
  echo "audit_gates masked a failing audit fanout" >&2
  cat "$tmp_root/audit-fail.out" >&2
  exit 1
fi
if ! grep -Fq "FAIL: one or more audit gates failed" "$tmp_root/audit-fail.out"; then
  echo "audit_gates did not report the failing audit fanout" >&2
  cat "$tmp_root/audit-fail.out" >&2
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

# A PASSING run must not print a failure summary — a warning that fires on green
# is noise, and noise is how a real warning stops being read.
if grep -Fq "=== FAILED:" "$tmp_root/audit.out"; then
  echo "a passing run printed a failure summary" >&2
  exit 1
fi

echo "audit_gates_parallel_test: ok"
