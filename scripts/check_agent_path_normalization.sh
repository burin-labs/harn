#!/usr/bin/env bash
set -euo pipefail
root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd -P)"
read_roots=()
if [[ $# -eq 1 && "$1" != --self-test ]]; then
  scan_root="$(cd -- "$1" && pwd -P)"
  set -- "$scan_root"
  read_roots=(--read-only-root "$scan_root")
fi
cd "$root"
exec ./scripts/harn_bin.sh --no-build -- run ${read_roots[@]+"${read_roots[@]}"} scripts/check_agent_path_normalization.harn -- "$@"
