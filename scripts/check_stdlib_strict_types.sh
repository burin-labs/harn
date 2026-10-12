#!/usr/bin/env bash
set -euo pipefail
root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd -P)"
cd "$root"
exec ./scripts/harn_bin.sh --no-build -- run scripts/check_stdlib_strict_types.harn -- "$@"
