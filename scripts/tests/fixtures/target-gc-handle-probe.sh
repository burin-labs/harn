#!/usr/bin/env bash
# Test-only probe for the private fixture trees used by the policy suites.
# Their only open handles are the Cargo locks created by those suites. The real
# all-process census is exercised separately by the reader test and host proof.
set -u
target="${1:?target path required}"
errors="$(mktemp)"
trap 'rm -f "$errors"' EXIT
for lock in "$target/.cargo-lock" "$target/debug/.cargo-lock" "$target/release/.cargo-lock"; do
  [ -f "$lock" ] || continue
  status=0
  result="$(lsof -t -- "$lock" 2>"$errors")" || status=$?
  if [ "$status" -eq 0 ] && [ -n "$result" ]; then
    echo busy
    exit 0
  fi
  if [ "$status" -ne 1 ] || [ -n "$result" ] || [ -s "$errors" ]; then
    echo unknown
    exit 0
  fi
done
echo idle
