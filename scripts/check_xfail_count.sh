#!/bin/sh
set -eu

threshold_file="scripts/xfail_threshold.txt"
if [ ! -f "$threshold_file" ]; then
  echo "missing $threshold_file" >&2
  exit 1
fi

threshold=$(tr -d '[:space:]' < "$threshold_file")
case "$threshold" in
  ''|*[!0-9]*)
    echo "$threshold_file must contain a non-negative integer" >&2
    exit 1
    ;;
esac

count=$(grep -Rho -- '@xfail:' conformance 2>/dev/null | wc -l | tr -d '[:space:]')
if [ "$count" -gt "$threshold" ]; then
  echo "::error::xfail count went up: $count > $threshold" >&2
  exit 1
fi

echo "xfail regression ratchet OK ($count <= $threshold)"
