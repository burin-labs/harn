#!/bin/sh
set -eu

resolve_harn_bin() {
  if [ -n "${HARN_BIN:-}" ]; then
    printf '%s\n' "$HARN_BIN"
    return
  fi

  cargo build --quiet --bin harn
  target_dir="$(cargo metadata --format-version=1 --no-deps \
    | python3 -c 'import json, sys; print(json.load(sys.stdin)["target_directory"])')"
  suffix=""
  case "$(uname -s)" in
    Windows_NT|MINGW*|MSYS*|CYGWIN*) suffix=".exe" ;;
  esac
  printf '%s/debug/harn%s\n' "$target_dir" "$suffix"
}

harn_bin="$(resolve_harn_bin)"
"$harn_bin" run scripts/check_rust_prompt_prose.harn -- --self-test
"$harn_bin" run scripts/check_rust_prompt_prose.harn
