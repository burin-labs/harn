#!/usr/bin/env bash
set -euo pipefail

script_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd -P)"
# shellcheck source=scripts/lib/cargo_env.sh
source "$script_dir/lib/cargo_env.sh"
# shellcheck source=scripts/lib/harn_bin.sh
source "$script_dir/lib/harn_bin.sh"

mode="build"
print_only=0

usage() {
  cat <<'EOF'
usage: scripts/harn_bin.sh [--print] [--no-build] [--] [harn args...]

Resolves a fresh worktree harn binary, rebuilding it only when the existing
binary is missing or older than Rust/Cargo executable inputs. With command
arguments, executes the resolved harn binary.

Environment:
  HARN_BIN                 explicit binary to validate and use
  HARN_BIN_ASSUME_FRESH=1  test-only escape hatch for fake binaries
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
