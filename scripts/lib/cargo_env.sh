#!/bin/sh

# Shared Cargo environment helpers for Harn's local hooks and CI/release
# warm-build scripts. Cargo normally keeps intermediates in the target
# directory, which is already stable and isolated per worktree. When a caller
# explicitly selects another target, align any inherited custom build-dir with
# that boundary as well.

harn_cargo_metadata_target_dir() (
  restricted_decoder=0
  metadata=$(cargo metadata --format-version=1 --no-deps) || exit $?
  if command -v jq >/dev/null 2>&1; then
    # Cargo owns this path and exposes it through a JSON contract. Delegate
    # JSON decoding to jq so escaped native Windows separators, drive colons,
    # spaces, and Unicode are data rather than shell/regex syntax. The cache
    # below is line-oriented, so fail closed on paths it cannot represent.
    target_dir=$(printf '%s\n' "$metadata" | jq -er '
      .target_directory
      | strings
      | select(length > 0)
      | select((contains("\n") or contains("\r")) | not)
    ') || target_dir=
  else
    restricted_decoder=1
    # Keep Rust-only Unix checkouts usable without adding jq as a build
    # prerequisite. This fallback intentionally accepts only Cargo JSON whose
    # target path needs no JSON escapes; anything broader requires a real JSON
    # decoder instead of hand-rolled unescaping.
    target_dir=$(printf '%s\n' "$metadata" \
      | sed -n 's/.*"target_directory":"\([^"\\]*\)".*/\1/p')
  fi
  if [ -z "$target_dir" ]; then
    if [ "$restricted_decoder" = 1 ]; then
      echo "error: jq is required when Cargo's target_directory contains JSON escapes" >&2
    fi
    echo "error: cargo metadata did not report a supported nonempty target_directory" >&2
    exit 1
  fi

  case "${OS:-$(uname -s)}" in
    Windows_NT | MINGW* | MSYS* | CYGWIN*)
      if ! command -v cygpath >/dev/null 2>&1; then
        echo "error: cannot normalize Cargo's native Windows target_directory without cygpath" >&2
        exit 1
      fi
      target_dir=$(cygpath -u "$target_dir") || exit $?
      ;;
  esac
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
