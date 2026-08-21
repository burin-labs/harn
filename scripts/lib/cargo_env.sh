#!/bin/sh

# Shared Cargo environment helpers for Harn's local hooks and CI/release
# warm-build scripts. Cargo normally keeps intermediates in the target
# directory, which is already stable and isolated per worktree. When a caller
# explicitly selects another target, align any inherited custom build-dir with
# that boundary as well.

harn_cargo_metadata_target_dir() (
  metadata=$(cargo metadata --format-version=1 --no-deps) || exit $?
  target_dir=$(printf '%s\n' "$metadata" | sed -n 's/.*"target_directory":"\([^"\\]*\)".*/\1/p')
  if [ -z "$target_dir" ]; then
    echo "error: cargo metadata did not report a simple target_directory" >&2
    exit 1
  fi
  printf '%s\n' "$target_dir"
)

harn_export_cargo_build_dir_for_target() {
  target_dir=${1:-${CARGO_TARGET_DIR:-}}
  if [ -z "$target_dir" ]; then
    return 1
  fi
  if [ -n "${CARGO_BUILD_BUILD_DIR:-}" ]; then
    return 1
  fi

  # Cargo's separate build-dir path is redundant when it equals target-dir.
  # On native Windows, enabling it also makes Cargo hand verbatim `\\?\` OUT_DIR
  # paths to C/C++ build scripts. cc-rs forwards those to cl.exe, which can
  # reinterpret the source path as rooted at `\\` and fail to find it. Keep an
  # explicit caller-owned build-dir, but let Cargo's target-local default own
  # intermediates when a Windows caller only selected CARGO_TARGET_DIR.
  case "${OS:-$(uname -s)}" in
    Windows_NT | MINGW* | MSYS* | CYGWIN*) return 1 ;;
  esac

  CARGO_BUILD_BUILD_DIR=$target_dir
  export CARGO_BUILD_BUILD_DIR
  return 0
}
