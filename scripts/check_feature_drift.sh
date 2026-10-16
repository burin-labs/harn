#!/usr/bin/env bash
set -euo pipefail
root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd -P)"
cd "$root"
exec ./scripts/harn_bin.sh -- run --no-sandbox scripts/check_feature_drift.harn -- "$@"
