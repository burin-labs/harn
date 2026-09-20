#!/bin/sh
set -eu

script_dir=$(CDPATH='' cd -- "$(dirname "$0")" && pwd -P)

resolve_harn_bin() {
  if [ -n "${HARN_BIN:-}" ]; then
    printf '%s\n' "$HARN_BIN"
    return
  fi

  "$script_dir/harn_bin.sh" --print
}

harn_bin="$(resolve_harn_bin)"
"$harn_bin" run scripts/check_cancellation_owner.harn -- --self-test
"$harn_bin" run scripts/check_cancellation_owner.harn
