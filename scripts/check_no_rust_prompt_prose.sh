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
if [ "${HARN_PROMPT_PROSE_SELF_TEST:-0}" = "1" ]; then
  "$harn_bin" run scripts/check_rust_prompt_prose.harn -- --self-test
fi
"$harn_bin" run scripts/check_rust_prompt_prose.harn
