#!/usr/bin/env bash
set -euo pipefail

script_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd -P)"
# shellcheck source=scripts/lib/cargo_env.sh
source "$script_dir/lib/cargo_env.sh"

cargo_metadata_target_dir() {
  local metadata target_dir
  metadata="$(cargo metadata --format-version=1 --no-deps)"
  target_dir="$(printf '%s\n' "$metadata" | sed -n 's/.*"target_directory":"\([^"\\]*\)".*/\1/p')"
  if [[ -z "$target_dir" ]]; then
    echo "error: cargo metadata did not report a simple target_directory" >&2
    return 1
  fi
  printf '%s\n' "$target_dir"
}

if [[ -z "${CARGO_TARGET_DIR:-}" ]]; then
  CARGO_TARGET_DIR="$(cargo_metadata_target_dir)"
  export CARGO_TARGET_DIR
else
  # A caller-supplied target is an explicit isolation boundary, so keep its
  # intermediates self-contained. A metadata-discovered target may come from
  # repo config alongside a machine-shared build-dir; leave that config in
  # charge instead of silently overriding it.
  harn_export_cargo_build_dir_for_target "$CARGO_TARGET_DIR" || true
fi

if [[ -n "${HARN_CARGO_LEASE_RUNNER:-}" ]]; then
  lease_runner_request="$HARN_CARGO_LEASE_RUNNER"
  lease_owner="${HARN_CARGO_LEASE_OWNER:-cargo-wrapper}"
  lease_host="${HARN_CARGO_LEASE_HOST:-}"
  lease_wait_ms="${HARN_CARGO_LEASE_WAIT_MS:-}"
  lease_priority_class="${HARN_CARGO_LEASE_PRIORITY_CLASS:-}"
  unset HARN_CARGO_LEASE_RUNNER HARN_CARGO_LEASE_OWNER HARN_CARGO_LEASE_HOST \
    HARN_CARGO_LEASE_WAIT_MS HARN_CARGO_LEASE_PRIORITY_CLASS

  if [[ "$lease_runner_request" == */* ]]; then
    lease_runner="$lease_runner_request"
  else
    lease_runner="$(command -v "$lease_runner_request" || true)"
  fi
  if [[ -z "$lease_runner" || ! -x "$lease_runner" ]]; then
    echo "error: HARN_CARGO_LEASE_RUNNER must resolve to an executable Harn binary" >&2
    exit 1
  fi

  workspace="$(cd "$script_dir/.." && pwd -P)"
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
  exec "$lease_runner" "${lease_args[@]}" -- "$@"
fi

exec cargo "$@"
