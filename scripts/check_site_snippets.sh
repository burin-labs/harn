#!/usr/bin/env bash
#
# Verify checked-in Harn snippets used by the marketing site parse cleanly.

set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT_DIR"

HARN_BIN="${HARN_BIN:-}"
if [[ -z "$HARN_BIN" ]]; then
  target_dir=""
  if command -v cargo >/dev/null 2>&1; then
    target_dir="$(cargo metadata --no-deps --format-version 1 2>/dev/null \
      | python3 -c 'import json,sys; print(json.load(sys.stdin).get("target_directory", ""))' 2>/dev/null)"
  fi
  if [[ -z "$target_dir" ]]; then
    target_dir="${CARGO_TARGET_DIR:-target}"
  fi
  if [[ ! -x "$target_dir/debug/harn" ]]; then
    echo "building harn-cli (set HARN_BIN to skip)..." >&2
    cargo build -q -p harn-cli
  fi
  HARN_BIN="$target_dir/debug/harn"
fi

checked=0
failures=0

shopt -s nullglob
for snippet in website/src/examples/*.harn.txt; do
  checked=$((checked + 1))
  if ! "$HARN_BIN" check "$snippet"; then
    failures=$((failures + 1))
  fi
done

echo
echo "site snippets: $checked checked, $failures failed"
if (( checked == 0 )); then
  echo "FAIL: no website/src/examples/*.harn.txt snippets found"
  exit 1
fi
if (( failures > 0 )); then
  exit 1
fi
