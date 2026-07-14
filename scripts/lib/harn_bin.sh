#!/usr/bin/env bash

# Shared Harn CLI binary resolution for hooks, Make targets, and CI helper
# scripts. Cargo owns freshness, target-dir configuration, and platform
# executable suffixes; this wrapper only validates explicit binaries and asks
# Cargo to run Harn's pre-runtime executable-path probe when it may build.

harn_repo_root() {
  git rev-parse --show-toplevel 2>/dev/null || pwd
}

harn_debug_bin_suffix() {
  case "${OS:-$(uname -s)}" in
    Windows_NT|MINGW*|MSYS*|CYGWIN*) printf '.exe' ;;
    *) printf '' ;;
  esac
}

harn_internal_executable_path_command() {
  printf '__internal-executable-path'
}

harn_debug_binary_path() {
  local target_dir="${CARGO_TARGET_DIR:-}"
  if [[ -z "$target_dir" ]]; then
    echo "error: CARGO_TARGET_DIR is required to locate a no-build worktree harn binary" >&2
    return 1
  fi
  printf '%s/debug/harn%s\n' "$target_dir" "$(harn_debug_bin_suffix)"
}

harn_require_executable_bin() {
  local bin="$1"
  if [[ ! -x "$bin" ]]; then
    echo "error: harn binary is not executable: $bin" >&2
    return 1
  fi
}

harn_resolve_binary() {
  local mode="${1:-build}"
  local bin=""

  if [[ -n "${HARN_BIN:-}" ]]; then
    if ! harn_require_executable_bin "$HARN_BIN"; then
      return 1
    fi
    printf '%s\n' "$HARN_BIN"
    return 0
  fi

  if [[ "$mode" = "no-build" ]]; then
    if ! bin="$(harn_debug_binary_path)"; then
      return 1
    fi
    if [[ -x "$bin" ]]; then
      printf '%s\n' "$bin"
      return 0
    fi
    echo "error: no fresh worktree harn binary found at $bin" >&2
    echo "hint: set HARN_BIN or run scripts/ci_warm_harn_bin.sh, then retry." >&2
    return 1
  fi

  harn_export_cargo_build_dir_for_target "${CARGO_TARGET_DIR:-}" || true
  bin="$(cargo run --quiet --bin harn -- "$(harn_internal_executable_path_command)")"
  if ! harn_require_executable_bin "$bin"; then
    return 1
  fi
  printf '%s\n' "$bin"
}
