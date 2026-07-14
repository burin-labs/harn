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

Resolves a worktree harn binary through Cargo unless HARN_BIN is explicit. With
command arguments, executes the resolved binary.

Environment:
  HARN_BIN          explicit executable to validate and use
  CARGO_TARGET_DIR  target directory for --no-build worktree lookup
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
