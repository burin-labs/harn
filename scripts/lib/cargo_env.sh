#!/bin/sh

# Shared Cargo environment helpers for Harn's local hooks and CI/release
# warm-build scripts. `build.build-dir` is independent from `target-dir`, so
# callers that isolate `CARGO_TARGET_DIR` must also isolate Cargo's intermediate
# build directory or workspace builds can still contend through shared config.

harn_cargo_build_dir_for_target() {
  if [ "$#" -ne 1 ] || [ -z "$1" ]; then
    return 2
  fi
  printf '%s/build\n' "$1"
}

harn_export_cargo_build_dir_under_target() {
  target_dir=${1:-${CARGO_TARGET_DIR:-}}
  if [ -z "$target_dir" ]; then
    return 1
  fi
  if [ -n "${CARGO_BUILD_BUILD_DIR:-}" ]; then
    return 1
  fi

  CARGO_BUILD_BUILD_DIR=$(harn_cargo_build_dir_for_target "$target_dir")
  export CARGO_BUILD_BUILD_DIR
  return 0
}
