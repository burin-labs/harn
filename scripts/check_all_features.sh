#!/usr/bin/env bash
set -euo pipefail
root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd -P)"
cd "$root"
exec ./scripts/harn_bin.sh --no-build -- run --no-sandbox scripts/check_all_features.harn -- "$@"
