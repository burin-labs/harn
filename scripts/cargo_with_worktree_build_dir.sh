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

exec cargo "$@"
