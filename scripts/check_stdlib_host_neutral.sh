#!/usr/bin/env bash
set -euo pipefail
root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd -P)"
read_roots=()
if [[ -n "${HARN_EXT_STDLIB_HOST_NEUTRAL_ROOT:-}" ]]; then
  HARN_EXT_STDLIB_HOST_NEUTRAL_ROOT="$(cd -- "$HARN_EXT_STDLIB_HOST_NEUTRAL_ROOT" && pwd -P)"
  export HARN_EXT_STDLIB_HOST_NEUTRAL_ROOT
  read_roots+=(--read-only-root "$HARN_EXT_STDLIB_HOST_NEUTRAL_ROOT")
fi
if [[ -n "${HARN_EXT_STDLIB_HOST_NEUTRAL_BASELINE:-}" ]]; then
  baseline_dir="$(cd -- "$(dirname -- "$HARN_EXT_STDLIB_HOST_NEUTRAL_BASELINE")" && pwd -P)"
  HARN_EXT_STDLIB_HOST_NEUTRAL_BASELINE="$baseline_dir/$(basename -- "$HARN_EXT_STDLIB_HOST_NEUTRAL_BASELINE")"
  export HARN_EXT_STDLIB_HOST_NEUTRAL_BASELINE
  read_roots+=(--read-only-root "$HARN_EXT_STDLIB_HOST_NEUTRAL_BASELINE")
fi
cd "$root"
exec ./scripts/harn_bin.sh --no-build -- run ${read_roots[@]+"${read_roots[@]}"} scripts/check_stdlib_host_neutral.harn -- "$@"
