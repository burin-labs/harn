#!/usr/bin/env bash

# Shared Harn CLI binary resolution for hooks, Make targets, and CI helper
# scripts. Cargo owns freshness, target-dir configuration, and platform
# executable suffixes; this wrapper only validates explicit binaries and asks
# Cargo to run Harn's pre-runtime executable-path probe when it may build. This
# small host bootstrap exists because no Harn runtime is available yet; once it
# resolves one, command deadlines and child lifecycle belong in std/command.

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
    # Cargo metadata resolves layered target-dir configuration without
    # compiling anything. This keeps --no-build source checks usable in a
    # freshly entered worktree where setup wrote .cargo/config.toml but the
    # parent shell did not export CARGO_TARGET_DIR.
    target_dir="$(harn_cargo_metadata_target_dir)" || return $?
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

harn_cargo_probe_timeout_seconds() {
  local timeout_seconds="${HARN_BIN_CARGO_TIMEOUT_SECONDS:-600}"
  if [[ ! "$timeout_seconds" =~ ^[0-9]+([.][0-9]+)?$ ]] || [[ "$timeout_seconds" = "0" ]]; then
    echo "error: HARN_BIN_CARGO_TIMEOUT_SECONDS must be a positive number" >&2
    return 2
  fi
  printf '%s\n' "$timeout_seconds"
}

harn_kill_process_group() {
  local signal="$1"
  local pid="$2"
  kill -"$signal" -- "-$pid" 2>/dev/null || kill -"$signal" "$pid" 2>/dev/null || true
}

harn_run_cargo_probe_with_deadline() (
  local timeout_seconds=""
  local probe_pid=""
  local watchdog_pid=""
  local status=0
  local state_dir=""

  timeout_seconds="$(harn_cargo_probe_timeout_seconds)" || return $?
  state_dir="$(mktemp -d "${TMPDIR:-/tmp}/harn-bin-probe.XXXXXX")"

  # Invoked by the EXIT trap below.
  # shellcheck disable=SC2329
  cleanup_probe() {
    if [[ -n "$watchdog_pid" ]]; then
      harn_kill_process_group TERM "$watchdog_pid"
      wait "$watchdog_pid" 2>/dev/null || true
    fi
    if [[ -n "$probe_pid" ]]; then
      harn_kill_process_group TERM "$probe_pid"
      wait "$probe_pid" 2>/dev/null || true
    fi
    rm -rf "$state_dir"
  }
  trap cleanup_probe EXIT
  trap 'exit 130' INT
  trap 'exit 143' TERM
  trap 'exit 129' HUP

  # Monitor mode gives each background job its own process group, including
  # descendants. Its human job-status notices are control noise, not probe
  # stderr, so keep them out of callers and hooks.
  {
    set -m
    cargo run --quiet --bin harn -- "$(harn_internal_executable_path_command)" \
      >"$state_dir/stdout" 2>"$state_dir/stderr" &
    probe_pid=$!
    (
      sleep "$timeout_seconds"
      if kill -0 "$probe_pid" 2>/dev/null; then
        printf 'timed-out\n' >"$state_dir/timed-out"
        harn_kill_process_group TERM "$probe_pid"
        sleep 1
        harn_kill_process_group KILL "$probe_pid"
      fi
    ) &
    watchdog_pid=$!

    wait "$probe_pid" || status=$?
    probe_pid=""
    if [[ -f "$state_dir/timed-out" ]]; then
      # Let the watchdog finish its TERM grace and hard-kill descendants that
      # ignored it. The process group can outlive its Cargo leader.
      wait "$watchdog_pid" 2>/dev/null || true
    else
      harn_kill_process_group TERM "$watchdog_pid"
      wait "$watchdog_pid" 2>/dev/null || true
    fi
    watchdog_pid=""
    set +m
  } 2>"$state_dir/job-control"

  cat "$state_dir/stderr" >&2
  if [[ -f "$state_dir/timed-out" ]]; then
    echo "error: Cargo harn binary probe timed out after ${timeout_seconds}s" >&2
    return 124
  fi
  if [[ "$status" -ne 0 ]]; then
    return "$status"
  fi
  cat "$state_dir/stdout"
)

harn_compiler_wrapper_configured() {
  if [[ -n "${RUSTC_WRAPPER:-}" || -n "${RUSTC_WORKSPACE_WRAPPER:-}" || \
        -n "${CARGO_BUILD_RUSTC_WRAPPER:-}" || -n "${CARGO_BUILD_RUSTC_WORKSPACE_WRAPPER:-}" ]]; then
    return 0
  fi
  local repo_root=""
  repo_root="$(harn_repo_root)"
  [[ -f "$repo_root/.cargo/config.toml" ]] && \
    grep -Eq '^[[:space:]]*rustc(-workspace)?-wrapper[[:space:]]*=' "$repo_root/.cargo/config.toml"
}

harn_resolve_binary() {
  local mode="${1:-build}"
  local bin=""
  local status=0

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
  bin="$(harn_run_cargo_probe_with_deadline)" || status=$?
  if [[ "$status" -ne 0 ]]; then
    if [[ "$status" -ne 124 ]] || ! harn_compiler_wrapper_configured; then
      return "$status"
    fi
    echo "warning: retrying Cargo harn binary probe with the compiler wrapper disabled" >&2
    status=0
    bin="$(
      RUSTC_WRAPPER='' \
      RUSTC_WORKSPACE_WRAPPER='' \
      CARGO_BUILD_RUSTC_WRAPPER='' \
      CARGO_BUILD_RUSTC_WORKSPACE_WRAPPER='' \
      SCCACHE_DISABLE=1 \
      harn_run_cargo_probe_with_deadline
    )" || status=$?
    if [[ "$status" -ne 0 ]]; then
      return "$status"
    fi
  fi
  if ! harn_require_executable_bin "$bin"; then
    return 1
  fi
  printf '%s\n' "$bin"
}
