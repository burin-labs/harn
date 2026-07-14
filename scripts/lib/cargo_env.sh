#!/bin/sh

# Shared Cargo environment helpers for Harn's local hooks and CI/release
# warm-build scripts. `build.build-dir` is independent from `target-dir`, so an
# isolated target must override the machine-shared development build directory.
# Cargo normally places intermediates directly in the target directory; using
# that same path preserves warm artifacts from ordinary Cargo commands.

harn_export_cargo_build_dir_for_target() {
  target_dir=${1:-${CARGO_TARGET_DIR:-}}
  if [ -z "$target_dir" ]; then
    return 1
  fi
  if [ -n "${CARGO_BUILD_BUILD_DIR:-}" ]; then
    return 1
  fi

  CARGO_BUILD_BUILD_DIR=$target_dir
  export CARGO_BUILD_BUILD_DIR
  return 0
}
