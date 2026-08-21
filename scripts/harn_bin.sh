#!/usr/bin/env bash
set -euo pipefail

script_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd -P)"
# shellcheck source=scripts/lib/cargo_env.sh
source "$script_dir/lib/cargo_env.sh"
# shellcheck source=scripts/lib/harn_bin.sh
source "$script_dir/lib/harn_bin.sh"

case "${HARN_BIN_NO_BUILD:-0}" in
  0) mode="build" ;;
  1) mode="no-build" ;;
  *)
    echo "error: HARN_BIN_NO_BUILD must be 0 or 1" >&2
    exit 2
    ;;
esac
print_only=0
record_receipt_only=0

usage() {
  cat <<'EOF'
usage: scripts/harn_bin.sh [--print] [--no-build] [--record-receipt] [--] [harn args...]

Resolves a worktree harn binary through Cargo unless HARN_BIN is explicit. With
command arguments, executes the resolved binary. No-build auto-resolution
requires the Cargo dependency and Git content receipt written by a successful
build-mode resolution; explicit HARN_BIN remains a caller-owned exact pin.

Environment:
  HARN_BIN           explicit executable to validate and use; the resolved path
                     is exported to the child process for nested Harn commands
  HARN_BIN_NO_BUILD  set to 1 to forbid implicit Cargo builds
  CARGO_TARGET_DIR                 target directory for --no-build worktree lookup
  HARN_BIN_CARGO_TIMEOUT_SECONDS   Cargo probe deadline in seconds (default: 600)
  HARN_BIN_RETRY_WITHOUT_WRAPPER   opt into one wrapper-disabled retry (0 or 1)
EOF
}

while [[ $# -gt 0 ]]; do
  case "$1" in
    --print)
      print_only=1
      shift
      ;;
    --no-build)
      mode="no-build"
      shift
      ;;
    --record-receipt)
      record_receipt_only=1
      shift
      ;;
    --)
      shift
      break
      ;;
    -h|--help)
      usage
      exit 0
      ;;
    *)
      break
      ;;
  esac
done

if [[ "$record_receipt_only" = "1" ]]; then
  if [[ "$print_only" = "1" || $# -ne 0 ]]; then
    echo "error: --record-receipt does not accept --print or harn arguments" >&2
    exit 2
  fi
  # This producer intentionally ignores an inherited explicit HARN_BIN. A
  # receipt is proof for the canonical worktree artifact, never a caller-owned
  # pin. Recording still fails closed unless compiled provenance matches the
  # current Git and Cargo dependency identities.
  if [[ -z "${CARGO_TARGET_DIR:-}" ]]; then
    harn_refresh_cargo_target_dir_cache >/dev/null
  fi
  bin="$(harn_debug_binary_path)"
  harn_record_binary_freshness "$bin"
  exit 0
fi

bin="$(harn_resolve_binary "$mode")"
if [[ "$print_only" = "1" ]]; then
  printf '%s\n' "$bin"
  exit 0
fi

export HARN_BIN="$bin"
exec "$bin" "$@"
