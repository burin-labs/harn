#!/usr/bin/env bash
set -euo pipefail
root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd -P)"
cd "$root"
# This compile gate also runs on a fresh CI runner before a source CLI exists.
# The canonical resolver builds once when needed and reuses an explicit HARN_BIN.
exec ./scripts/harn_bin.sh -- run --no-sandbox scripts/check_all_features.harn -- "$@"
