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

# Resolve the lease policy once for every Cargo entry point. CI intentionally
# runs without a local cross-process lease unless a caller explicitly supplies
# one; developer shells default to auto-discovery, while an explicit runner is
# a required authority. Keeping this policy here prevents wrappers and their
# callers from disagreeing about whether control environment must traverse a
# lease supervisor.
harn_effective_cargo_lease_mode() {
  lease_mode=${HARN_CARGO_LEASE_MODE:-}
  if [ -z "$lease_mode" ]; then
    if [ -n "${HARN_CARGO_LEASE_RUNNER:-}" ]; then
      lease_mode=required
    elif [ "${CI:-}" = "true" ]; then
      lease_mode=off
    else
      lease_mode=auto
    fi
  fi

  case "$lease_mode" in
    auto | off | required) printf '%s\n' "$lease_mode" ;;
    *)
      echo "error: HARN_CARGO_LEASE_MODE must be auto, off, or required" >&2
      return 2
      ;;
  esac
}

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
