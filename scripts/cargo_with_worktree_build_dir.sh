#!/usr/bin/env bash
set -euo pipefail

script_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd -P)"
workspace_request="${HARN_CARGO_LEASE_WORKSPACE:-$script_dir/..}"
workspace="$(cd "$workspace_request" && pwd -P)"
unset HARN_CARGO_LEASE_WORKSPACE
# shellcheck source=scripts/lib/cargo_env.sh
source "$script_dir/lib/cargo_env.sh"

lease_runner_request="${HARN_CARGO_LEASE_RUNNER:-}"
lease_owner="${HARN_CARGO_LEASE_OWNER:-cargo-wrapper}"
lease_host="${HARN_CARGO_LEASE_HOST:-}"
lease_wait_ms="${HARN_CARGO_LEASE_WAIT_MS:-3600000}"
lease_priority_class="${HARN_CARGO_LEASE_PRIORITY_CLASS:-interactive}"
lease_workload_timeout_ms="${HARN_CARGO_LEASE_WORKLOAD_TIMEOUT_MS:-}"
lease_mode="$(harn_effective_cargo_lease_mode)" || exit $?
unset HARN_CARGO_LEASE_RUNNER HARN_CARGO_LEASE_OWNER HARN_CARGO_LEASE_HOST \
  HARN_CARGO_LEASE_WAIT_MS HARN_CARGO_LEASE_PRIORITY_CLASS HARN_CARGO_LEASE_MODE
unset HARN_CARGO_LEASE_WORKLOAD_TIMEOUT_MS

cargo_subcommand() {
  local arg=""
  local expects_value=0
  for arg in "$@"; do
    if [[ "$expects_value" == "1" ]]; then
      expects_value=0
      continue
    fi
    case "$arg" in
      +*) ;;
      --config | -C)
        expects_value=1
        ;;
      --config=* | --color=* | --frozen | --locked | --offline | -q | --quiet | -v | --verbose)
        ;;
      -h | --help)
        printf 'help\n'
        return
        ;;
      -V | --version)
        printf 'version\n'
        return
        ;;
      --*) ;;
      -*) ;;
      *)
        printf '%s\n' "$arg"
        return
        ;;
    esac
  done
}

cargo_subcommand_is_static() {
  case "$1" in
    "" | add | audit | fetch | fmt | generate-lockfile | help | info | locate-project | \
      login | logout | machete | metadata | remove | search | tree | update | vendor | version | yank)
      return 0
      ;;
    *) return 1 ;;
  esac
}

resolve_lease_runner() {
  local candidate=""
  local can_run_target_binary=1
  local suffix=""

  if [[ -n "$lease_runner_request" ]]; then
    if [[ "$lease_runner_request" == */* ]]; then
      candidate="$lease_runner_request"
    else
      candidate="$(command -v "$lease_runner_request" || true)"
    fi
    if [[ -z "$candidate" || ! -x "$candidate" ]]; then
      echo "error: HARN_CARGO_LEASE_RUNNER must resolve to an executable Harn binary" >&2
      return 1
    fi
    printf '%s\n' "$candidate"
    return
  fi

  # A caller that has already warmed an explicit binary should not need the
  # target-local copy to exist as well. This is especially important for
  # focused checks launched from a separate build directory: falling through
  # would silently start a second Rust-heavy compile instead of reusing the
  # supplied lease supervisor.
  if [[ -n "${HARN_BIN:-}" && -x "$HARN_BIN" ]] \
    && "$HARN_BIN" host lease run cargo --help >/dev/null 2>&1; then
    printf '%s\n' "$HARN_BIN"
    return
  fi

  case "${OS:-$(uname -s)}" in
    Windows_NT | MINGW* | MSYS* | CYGWIN*)
      suffix=".exe"
      can_run_target_binary=0
      ;;
  esac
  candidate="$CARGO_TARGET_DIR/debug/harn$suffix"
  # Windows cannot replace an executable while that same image supervises the
  # Cargo build. Use only an independently installed runner there.
  if [[ "$can_run_target_binary" == "1" && -x "$candidate" ]] \
    && "$candidate" host lease run cargo --help >/dev/null 2>&1; then
    printf '%s\n' "$candidate"
    return
  fi
  candidate="$(command -v harn || true)"
  if [[ -n "$candidate" && -x "$candidate" ]] \
    && "$candidate" host lease run cargo --help >/dev/null 2>&1; then
    printf '%s\n' "$candidate"
  fi
}

# Make compiler-object identity independent of the disposable worktree's
# absolute path. `dev_setup.sh` projects the same contract through Cargo's
# generated config, but the wrapper is also used before setup and in lean
# source checkouts. Failing open there made a shared 24 GiB sccache report
# effectively zero Rust hits across worktrees while appearing healthy.
if [[ -z "${SCCACHE_BASEDIRS:-}" ]]; then
  export SCCACHE_BASEDIRS="$workspace"
fi

if [[ -z "${CARGO_TARGET_DIR:-}" ]]; then
  CARGO_TARGET_DIR="$(harn_cargo_metadata_target_dir)"
  export CARGO_TARGET_DIR
else
  # A caller-supplied target is an explicit isolation boundary, so keep its
  # intermediates self-contained. A metadata-discovered target may come from
  # repo config alongside a deliberate custom build-dir; leave that config in
  # charge instead of silently overriding it.
  harn_export_cargo_build_dir_for_target "$CARGO_TARGET_DIR" || true
fi

subcommand="$(cargo_subcommand "$@")"
if [[ "${CARGO_HARN_HOST_LEASE_ACTIVE:-0}" == "1" ]] \
  || [[ "$lease_mode" == "off" ]] \
  || cargo_subcommand_is_static "$subcommand"; then
  exec cargo "$@"
fi

lease_runner="$(resolve_lease_runner)"
if [[ -z "$lease_runner" ]]; then
  if [[ "$lease_mode" == "required" ]]; then
    echo "error: no compatible prebuilt Harn binary can supervise this Cargo command" >&2
    exit 1
  fi
  echo "warning: no compatible prebuilt Harn binary; running Cargo without the rust-heavy lease" >&2
  exec cargo "$@"
fi

lease_args=(
  host lease run cargo
  --owner "$lease_owner"
  --workspace "$workspace"
  --target-dir "$CARGO_TARGET_DIR"
)
if [[ -n "${CARGO_BUILD_BUILD_DIR:-}" ]]; then
  lease_args+=(--build-dir "$CARGO_BUILD_BUILD_DIR")
fi
if [[ -n "$lease_host" ]]; then
  lease_args+=(--host "$lease_host")
fi
if [[ -n "$lease_wait_ms" ]]; then
  lease_args+=(--wait-ms "$lease_wait_ms")
fi
if [[ -n "$lease_priority_class" ]]; then
  lease_args+=(--priority-class "$lease_priority_class")
fi
if [[ -n "$lease_workload_timeout_ms" ]] \
  && "$lease_runner" host lease run cargo --help 2>&1 \
    | grep -q -- '--workload-timeout-ms'; then
  lease_args+=(--workload-timeout-ms "$lease_workload_timeout_ms")
fi

export CARGO_HARN_HOST_LEASE_ACTIVE=1
exec "$lease_runner" "${lease_args[@]}" -- "$@"
