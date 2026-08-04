#!/usr/bin/env bash
set -euo pipefail

repo_root=$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)
harn_bin=${HARN_BIN:?check_harn_syntax_sensitive_scans_performance_test requires HARN_BIN}

output=$(cd "$repo_root" && "$harn_bin" run --profile scripts/check_harn_syntax_sensitive_scans.harn 2>&1) || {
  printf '%s\n' "$output" >&2
  exit 1
}

builtin_calls() {
  local name=$1
  local calls
  calls=$(printf '%s\n' "$output" | sed -nE "s/.*${name}.*\(([0-9]+) calls.*/\1/p" | head -n 1)
  printf '%s\n' "${calls:-0}"
}

# These are work-count ceilings, not wall-clock budgets: they stay stable on a
# noisy runner while catching the two algorithmic regressions that made this
# guard dominate CI (per-line list length and unconditional regex evaluation).
regex_calls=$(builtin_calls regex_captures)
len_calls=$(builtin_calls len)
max_calls=5000

if (( regex_calls > max_calls )); then
  echo "syntax-sensitive scan used ${regex_calls} regex calls; expected <= ${max_calls}" >&2
  exit 1
fi
if (( len_calls > max_calls )); then
  echo "syntax-sensitive scan used ${len_calls} len calls; expected <= ${max_calls}" >&2
  exit 1
fi

dispatch_probe='
let control = 0
const control_start = harness.clock.monotonic_ms()
for item in range(0, 50000) { control = control + 100 }
const control_ms = harness.clock.monotonic_ms() - control_start
const values = range(0, 100)
let measured = 0
const builtin_start = harness.clock.monotonic_ms()
for item in range(0, 50000) { measured = measured + len(values) }
const builtin_ms = harness.clock.monotonic_ms() - builtin_start
harness.stdio.println("control_ms=" + to_string(control_ms) + " builtin_ms=" + to_string(builtin_ms))
if builtin_ms > control_ms * 6 + 500 { harness.runtime.exit(1) }
'
if ! dispatch_output=$("$harn_bin" run -e "$dispatch_probe" 2>&1); then
  echo "pure builtin dispatch exceeded the same-run control budget: ${dispatch_output}" >&2
  exit 1
fi

echo "check_harn_syntax_sensitive_scans_performance_test: ok (regex=${regex_calls}, len=${len_calls}; ${dispatch_output})"
