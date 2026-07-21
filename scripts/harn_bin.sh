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

usage() {
  cat <<'EOF'
usage: scripts/harn_bin.sh [--print] [--no-build] [--] [harn args...]

Resolves a worktree harn binary through Cargo unless HARN_BIN is explicit. With
command arguments, executes the resolved binary.

Environment:
  HARN_BIN           explicit executable to validate and use
  HARN_BIN_NO_BUILD  set to 1 to forbid implicit Cargo builds
  CARGO_TARGET_DIR                 target directory for --no-build worktree lookup
  HARN_BIN_CARGO_TIMEOUT_SECONDS   Cargo probe deadline in seconds (default: 600)
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

bin="$(harn_resolve_binary "$mode")"
if [[ "$print_only" = "1" ]]; then
  printf '%s\n' "$bin"
  exit 0
fi

exec "$bin" "$@"
