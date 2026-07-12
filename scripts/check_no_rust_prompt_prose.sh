#!/bin/sh
set -eu

script_dir=$(CDPATH='' cd -- "$(dirname "$0")" && pwd -P)
# shellcheck source=scripts/lib/cargo_env.sh
. "$script_dir/lib/cargo_env.sh"

resolve_harn_bin() {
  if [ -n "${HARN_BIN:-}" ]; then
    printf '%s\n' "$HARN_BIN"
    return
  fi

  harn_export_cargo_build_dir_under_target "${CARGO_TARGET_DIR:-}" || true
  RUSTC_WRAPPER= CARGO_BUILD_RUSTC_WRAPPER= cargo build --quiet --bin harn
  target_dir="$(cargo metadata --format-version=1 --no-deps \
    | python3 -c 'import json, sys; print(json.load(sys.stdin)["target_directory"])')"
  suffix=""
  case "$(uname -s)" in
    Windows_NT|MINGW*|MSYS*|CYGWIN*) suffix=".exe" ;;
  esac
  printf '%s/debug/harn%s\n' "$target_dir" "$suffix"
}

harn_bin="$(resolve_harn_bin)"
if [ "${HARN_PROMPT_PROSE_SELF_TEST:-0}" = "1" ]; then
  "$harn_bin" run scripts/check_rust_prompt_prose.harn -- --self-test
fi
"$harn_bin" run scripts/check_rust_prompt_prose.harn
