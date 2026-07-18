#!/bin/sh

# Shared Cargo environment helpers for Harn's local hooks and CI/release
# warm-build scripts. Cargo normally keeps intermediates in the target
# directory, which is already stable and isolated per worktree. When a caller
# explicitly selects another target, align any inherited custom build-dir with
# that boundary as well.

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
