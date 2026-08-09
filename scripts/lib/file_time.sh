#!/usr/bin/env bash

# Print a file's modification time as Unix epoch seconds across GNU and BSD
# userlands. A successful stat invocation is accepted only when its output has
# the numeric shape callers can safely use in shell arithmetic.
file_mtime_epoch() {
  local candidate

  candidate="$(stat -c %Y "$1" 2>/dev/null || true)"
  if [[ "${candidate}" =~ ^[0-9]+$ ]]; then
    printf '%s\n' "${candidate}"
    return
  fi

  candidate="$(stat -f %m "$1" 2>/dev/null || true)"
  if [[ "${candidate}" =~ ^[0-9]+$ ]]; then
    printf '%s\n' "${candidate}"
    return
  fi

  return 1
}
