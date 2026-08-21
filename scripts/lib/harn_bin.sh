#!/usr/bin/env bash

# Shared Harn CLI binary resolution for hooks, Make targets, and CI helper
# scripts. Cargo owns freshness, target-dir configuration, and platform
# executable suffixes; this wrapper only validates explicit binaries and asks
# Cargo to run Harn's pre-runtime executable-path probe when it may build. This
# small host bootstrap exists because no Harn runtime is available yet; once it
# resolves one, command deadlines and child lifecycle belong in std/command.
#
# Auto-resolved no-build callers additionally require a content receipt written
# after a successful Cargo resolution. Cargo's top-level dep-info owns the
# executable's dependency set; a typed parser inside the exact binary hashes
# that normalized graph and every file/directory input. Git supplies a whole
# checkout fingerprint as an independent source/provenance proof.
# shellcheck source=scripts/lib/harn_bin_freshness.sh
source "$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd -P)/harn_bin_freshness.sh"

harn_bin_scripts_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd -P)"
harn_bin_source_repo_root="$(cd "$harn_bin_scripts_dir/.." && pwd -P)"
if [[ ! -e "$harn_bin_source_repo_root/.git" ]]; then
  # Standalone release tools retain scripts/lib but execute against a separate
  # Git checkout. They own their adjacent helper scripts, not their staging
  # directory as a synthetic repository root.
  harn_bin_source_repo_root=""
fi

harn_repo_root() {
  if [[ -n "$harn_bin_source_repo_root" ]]; then
    case "$PWD/" in
      "$harn_bin_source_repo_root/"*)
        printf '%s\n' "$harn_bin_source_repo_root"
        return
        ;;
    esac
  fi
  git rev-parse --show-toplevel 2>/dev/null || pwd -P
}

harn_debug_bin_suffix() {
  case "${OS:-$(uname -s)}" in
    Windows_NT|MINGW*|MSYS*|CYGWIN*) printf '.exe' ;;
    *) printf '' ;;
  esac
}

# Rust's current_exe() prints a native drive/backslash path on Windows, while
# the owning wrapper runs under Git Bash and performs structural path
# operations. Normalize once at that boundary; never reinterpret drive colons
# or backslashes as Make/shell syntax downstream.
harn_shell_executable_path() {
  local bin="$1"
  case "${OS:-$(uname -s)}" in
    Windows_NT|MINGW*|MSYS*|CYGWIN*)
      if ! command -v cygpath >/dev/null 2>&1; then
        echo "error: cannot normalize native Windows harn path without cygpath" >&2
        return 1
      fi
      cygpath -u "$bin"
      ;;
    *) printf '%s\n' "$bin" ;;
  esac
}

harn_internal_executable_path_command() {
  printf '__internal-executable-path'
}

harn_debug_named_binary_path() {
  local binary_name="$1"
  local target_dir="${CARGO_TARGET_DIR:-}"
  if [[ -z "$target_dir" ]]; then
    target_dir="$(harn_cached_cargo_target_dir)" || return $?
  fi
  printf '%s/debug/%s%s\n' "$target_dir" "$binary_name" "$(harn_debug_bin_suffix)"
}

harn_target_dir_cache_path() {
  printf '%s/.cargo/harn-target-dir\n' "$(harn_repo_root)"
}

harn_refresh_cargo_target_dir_cache() (
  local target_dir=""
  local cache=""
  local temporary=""
  local recorded=""

  cleanup_target_dir_cache() {
    [[ -z "$temporary" ]] || rm -f "$temporary"
  }
  trap cleanup_target_dir_cache EXIT
  trap 'exit 129' HUP
  trap 'exit 130' INT
  trap 'exit 143' TERM

  target_dir="$(harn_cargo_metadata_target_dir)" || return $?
  cache="$(harn_target_dir_cache_path)" || return $?
  if [[ -r "$cache" ]]; then
    {
      IFS= read -r recorded || recorded=""
      if IFS= read -r; then
        recorded=""
      fi
    } <"$cache"
    if [[ "$recorded" = "$target_dir" ]]; then
      printf '%s\n' "$target_dir"
      return 0
    fi
  fi
  mkdir -p "${cache%/*}" || return $?
  temporary="$(mktemp "${cache}.tmp.XXXXXX")" || return $?
  printf '%s\n' "$target_dir" >"$temporary" || return $?
  mv "$temporary" "$cache" || return $?
  temporary=""
  printf '%s\n' "$target_dir"
)

harn_cached_cargo_target_dir() {
  local cache=""
  local target_dir=""
  cache="$(harn_target_dir_cache_path)" || return $?
  if [[ -r "$cache" ]]; then
    {
      IFS= read -r target_dir || return $?
      if IFS= read -r; then
        target_dir=""
      fi
    } <"$cache"
    if [[ -n "$target_dir" ]]; then
      printf '%s\n' "$target_dir"
      return 0
    fi
  fi
  harn_refresh_cargo_target_dir_cache
}

harn_debug_binary_path() {
  harn_debug_named_binary_path harn
}

harn_require_executable_bin() {
  local bin="$1"
  if [[ ! -x "$bin" ]]; then
    echo "error: harn binary is not executable: $bin" >&2
    return 1
  fi
}

# Copy a resolved Cargo output into a caller-owned immutable execution path.
# Parallel Cargo invocations may replace or briefly unlink target/debug/harn;
# long-lived gates must execute a snapshot whose lifetime they control.
harn_snapshot_binary() {
  local source_bin="$1"
  local destination_dir="$2"
  local destination_name="${3:-harn}"
  local suffix=""
  local snapshot=""

  harn_require_executable_bin "$source_bin" || return $?
  case "$source_bin" in
    *.exe) suffix=".exe" ;;
  esac
  mkdir -p "$destination_dir" || return $?
  snapshot="$destination_dir/$destination_name$suffix"
  cp "$source_bin" "$snapshot" || return $?
  chmod +x "$snapshot" || return $?
  printf '%s\n' "$snapshot"
}

harn_cargo_probe_timeout_seconds() {
  local timeout_seconds="${HARN_BIN_CARGO_TIMEOUT_SECONDS:-600}"
  if [[ ! "$timeout_seconds" =~ ^[0-9]+([.][0-9]+)?$ ]] || [[ "$timeout_seconds" = "0" ]]; then
    echo "error: HARN_BIN_CARGO_TIMEOUT_SECONDS must be a positive number" >&2
    return 2
  fi
  printf '%s\n' "$timeout_seconds"
}

harn_retry_without_wrapper() {
  case "${HARN_BIN_RETRY_WITHOUT_WRAPPER:-0}" in
    0|1) printf '%s\n' "${HARN_BIN_RETRY_WITHOUT_WRAPPER:-0}" ;;
    *)
      echo "error: HARN_BIN_RETRY_WITHOUT_WRAPPER must be 0 or 1" >&2
      return 2
      ;;
  esac
}

harn_kill_process_group() {
  local signal="$1"
  local pid="$2"
  kill -"$signal" -- "-$pid" 2>/dev/null || kill -"$signal" "$pid" 2>/dev/null || true
}

harn_run_cargo_probe_with_deadline() (
  local predicted_bin="$1"
  local timeout_seconds=""
  local probe_pid=""
  local watchdog_pid=""
  local status=0
  local state_dir=""
  local cargo_wrapper="$harn_bin_scripts_dir/cargo_with_worktree_build_dir.sh"
  local build_freshness_id="${HARN_BUILD_FRESHNESS_ID:-}"
  local -a cargo_config_args=()
  local clear_freshness_environment=0

  timeout_seconds="$(harn_cargo_probe_timeout_seconds)" || return $?
  if [[ ! "$build_freshness_id" =~ ^([0-9a-f]{40}|[0-9a-f]{64})$ ]]; then
    echo "error: internal Harn build freshness identity is missing or malformed" >&2
    return 1
  fi
  if [[ "${HARN_CARGO_LEASE_MODE:-}" != "off" ]]; then
    clear_freshness_environment=1
    cargo_config_args+=(--config "env.HARN_BUILD_FRESHNESS_ID='$build_freshness_id'")
  fi
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
    (
      # Rendezvous only after Bash has created this monitored job's process
      # group. Without this boundary a sub-second watchdog can signal the PID
      # before setpgid completes; the eventual Cargo descendant then escapes
      # into a newly established group and survives the timeout.
      : >"$state_dir/probe-started"
      # Released lease supervisors intentionally reject unknown HARN_*
      # environment controls. Cargo's typed CLI configuration carries the new
      # build input through an older supervisor without exposing it to that
      # supervisor's environment validator.
      if [[ "$clear_freshness_environment" == "1" ]]; then
        unset HARN_BUILD_FRESHNESS_ID
      fi
      HARN_CARGO_LEASE_WORKSPACE="$(harn_repo_root)" \
      "$cargo_wrapper" "${cargo_config_args[@]}" build --quiet \
        --bin harn --bin harn-freshness-check \
        --features internal-freshness-checker &&
        "$predicted_bin" "$(harn_internal_executable_path_command)"
    ) >"$state_dir/stdout" 2>"$state_dir/stderr" &
    probe_pid=$!
    while [[ ! -f "$state_dir/probe-started" ]]; do
      if ! kill -0 "$probe_pid" 2>/dev/null; then
        break
      fi
      sleep 0.01
    done
    # A watchdog or descendant must never inherit the command-substitution
    # output pipe. If a platform delays process-group teardown, an inherited
    # writer keeps the caller blocked on EOF even after the probe has returned.
    (
      sleep "$timeout_seconds"
      if kill -0 "$probe_pid" 2>/dev/null; then
        printf 'timed-out\n' >"$state_dir/timed-out"
        harn_kill_process_group TERM "$probe_pid"
        sleep 1
        harn_kill_process_group KILL "$probe_pid"
      fi
    ) </dev/null >"$state_dir/watchdog-stdout" 2>"$state_dir/watchdog-stderr" &
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

# True when this checkout has never been through `make setup`. Such a worktree
# has no shared target dir and no compiler wrapper, so every build starts cold.
harn_worktree_unconfigured() {
  [[ ! -f "$(harn_repo_root)/.cargo/config.toml" ]]
}

# What to do about a probe that ran out of time. A cold build of the whole
# workspace does not fit in the default deadline, so in a fresh worktree the
# deadline is reached for a legitimate reason and the bare timeout reads like a
# hang. Name the likely cause and the two ways out.
harn_print_probe_timeout_hints() {
  if harn_worktree_unconfigured; then
    echo "hint: $(harn_repo_root) has no .cargo/config.toml, so this build had no" >&2
    echo "hint: shared target dir and no compiler wrapper. Run 'make setup' here once." >&2
  fi
  echo "hint: to reuse a binary you already built:" >&2
  echo "hint:   HARN_BIN=<path-to-harn> HARN_BIN_NO_BUILD=1 <command>" >&2
  echo "hint: to allow a longer cold build:" >&2
  echo "hint:   HARN_BIN_CARGO_TIMEOUT_SECONDS=3600 <command>" >&2
}

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

harn_resolve_binary() (
  local mode="${1:-build}"
  local bin=""
  local retry_without_wrapper=""
  local status=0
  local predicted_bin=""
  local current_freshness=""
  local embedded_freshness=""
  local predicted_checker=""
  local lease_runner_snapshot_dir=""
  local lease_runner_snapshot=""
  local installed_lease_runner=""
  local configured_lease_runner=""

  cleanup_lease_runner_snapshot() {
    [[ -z "$lease_runner_snapshot_dir" ]] || rm -rf "$lease_runner_snapshot_dir"
  }
  trap cleanup_lease_runner_snapshot EXIT
  trap 'exit 129' HUP
  trap 'exit 130' INT
  trap 'exit 143' TERM

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
      harn_require_binary_freshness_receipt "$bin" || return $?
      printf '%s\n' "$bin"
      return 0
    fi
    echo "error: no fresh worktree harn binary found at $bin" >&2
    echo "hint: set HARN_BIN or run scripts/ci_warm_harn_bin.sh, then retry." >&2
    return 1
  fi

  retry_without_wrapper="$(harn_retry_without_wrapper)" || return $?

  harn_export_cargo_build_dir_for_target "${CARGO_TARGET_DIR:-}" || true
  if [[ -z "${CARGO_TARGET_DIR:-}" ]]; then
    harn_refresh_cargo_target_dir_cache >/dev/null || return $?
  fi
  predicted_bin="$(harn_debug_binary_path)" || return $?
  predicted_checker="$(harn_binary_freshness_checker_path "$predicted_bin")" || return $?
  mkdir -p "$(harn_binary_target_dir "$predicted_bin")" || return $?
  configured_lease_runner="${HARN_CARGO_LEASE_RUNNER:-}"
  if [[ -n "$configured_lease_runner" && "$configured_lease_runner" != */* ]]; then
    configured_lease_runner="$(command -v "$configured_lease_runner" 2>/dev/null || true)"
  fi
  installed_lease_runner="$(command -v harn 2>/dev/null || true)"
  if [[ "${HARN_CARGO_LEASE_MODE:-}" != "off" \
        && -n "$configured_lease_runner" \
        && -x "$predicted_bin" \
        && "$configured_lease_runner" -ef "$predicted_bin" ]]; then
    # An explicit runner may name (or symlink to) the artifact this fixed-point
    # build must replace. Preserve the caller's lease authority while moving
    # execution off the mutable Cargo output on every platform.
    lease_runner_snapshot_dir="$(mktemp -d "${TMPDIR:-/tmp}/harn-cargo-lease-runner.XXXXXX")" || return $?
    lease_runner_snapshot="$(
      harn_snapshot_binary "$predicted_bin" "$lease_runner_snapshot_dir" harn-lease-runner
    )" || return $?
    export HARN_CARGO_LEASE_RUNNER="$lease_runner_snapshot"
    export HARN_CARGO_LEASE_MODE=required
  elif [[ "${HARN_CARGO_LEASE_MODE:-}" != "off" \
        && -z "${HARN_CARGO_LEASE_RUNNER:-}" \
        && -n "$installed_lease_runner" \
        && ! "$installed_lease_runner" -ef "$predicted_bin" \
        && -x "$installed_lease_runner" ]]; then
    # Required mode is the capability probe: an older or unrelated executable
    # fails closed when asked to supervise the build. Do not run a separate
    # help invocation whose startup policy can differ from the actual command.
    export HARN_CARGO_LEASE_RUNNER="$installed_lease_runner"
    export HARN_CARGO_LEASE_MODE=required
  elif [[ "${HARN_CARGO_LEASE_MODE:-}" != "off" \
        && -z "${HARN_CARGO_LEASE_RUNNER:-}" \
        && -x "$predicted_bin" ]]; then
    # The fixed-point build deliberately removes an unproven target binary so
    # Cargo must relink it. Supervise that mutation from a caller-owned
    # snapshot: Unix may unlink a running image, Windows may not replace one,
    # and neither platform should make the lease runner its own build output.
    lease_runner_snapshot_dir="$(mktemp -d "${TMPDIR:-/tmp}/harn-cargo-lease-runner.XXXXXX")" || return $?
    lease_runner_snapshot="$(
      harn_snapshot_binary "$predicted_bin" "$lease_runner_snapshot_dir" harn-lease-runner
    )" || return $?
    export HARN_CARGO_LEASE_RUNNER="$lease_runner_snapshot"
    export HARN_CARGO_LEASE_MODE=required
  fi
  if [[ -x "$predicted_bin" ]] && \
     ! harn_require_binary_freshness_receipt "$predicted_bin" >/dev/null 2>&1; then
    # Never let a build-mode probe bless an unproven or externally changed
    # artifact that Cargo's mtime fingerprint might otherwise call fresh. Read
    # its authoritative dep-info while the helper is still runnable, then
    # remove only the canonical build output so Cargo must relink it.
    HARN_BUILD_FRESHNESS_ID="$(harn_build_freshness_id "$predicted_bin" 1)" || \
      HARN_BUILD_FRESHNESS_ID="$(harn_build_freshness_id "$predicted_bin" 0)" || return $?
    rm -f -- "$predicted_bin" "$predicted_checker" || return $?
  else
    HARN_BUILD_FRESHNESS_ID="$(harn_build_freshness_id "$predicted_bin" 0)" || return $?
  fi
  export HARN_BUILD_FRESHNESS_ID
  if harn_worktree_unconfigured; then
    echo "warning: $(harn_repo_root) has no .cargo/config.toml; 'make setup' has not run here, so this build starts cold" >&2
  fi
  bin="$(harn_run_cargo_probe_with_deadline "$predicted_bin")" || status=$?
  if [[ "$status" -ne 0 ]]; then
    if [[ "$status" -ne 124 ]]; then
      return "$status"
    fi
    if ! harn_compiler_wrapper_configured; then
      harn_print_probe_timeout_hints
      return "$status"
    fi
    if [[ "$retry_without_wrapper" != "1" ]]; then
      harn_print_probe_timeout_hints
      echo "hint: a compiler wrapper may be waiting behind active sibling builds." >&2
      echo "hint: retry without it only when the wrapper is genuinely wedged:" >&2
      echo "hint:   HARN_BIN_RETRY_WITHOUT_WRAPPER=1 <command>" >&2
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
      harn_run_cargo_probe_with_deadline "$predicted_bin"
    )" || status=$?
    if [[ "$status" -ne 0 ]]; then
      if [[ "$status" -eq 124 ]]; then
        harn_print_probe_timeout_hints
      fi
      return "$status"
    fi
  fi
  bin="$(harn_shell_executable_path "$bin")" || return $?
  if ! harn_require_executable_bin "$bin"; then
    return 1
  fi
  current_freshness="$(harn_build_freshness_id "$bin" 1)" || return $?
  embedded_freshness="$(harn_embedded_build_freshness_id "$bin")" || return $?
  if [[ "$embedded_freshness" != "$current_freshness" ]]; then
    # The first build after bootstrap or a changed dependency set used the
    # previous dep-info graph. Re-run once with the now-authoritative graph so
    # Cargo's rerun-if-env-changed edge converges the compiled provenance.
    HARN_BUILD_FRESHNESS_ID="$current_freshness"
    export HARN_BUILD_FRESHNESS_ID
    bin="$(harn_run_cargo_probe_with_deadline "$predicted_bin")" || return $?
    bin="$(harn_shell_executable_path "$bin")" || return $?
    harn_require_executable_bin "$bin" || return $?
    current_freshness="$(harn_build_freshness_id "$bin" 1)" || return $?
    embedded_freshness="$(harn_embedded_build_freshness_id "$bin")" || return $?
    if [[ "$embedded_freshness" != "$current_freshness" ]]; then
      echo "error: Harn build freshness identity did not converge after Cargo rebuilt the authoritative dependency graph" >&2
      return 1
    fi
  fi
  harn_record_binary_freshness "$bin" || return $?
  printf '%s\n' "$bin"
)
