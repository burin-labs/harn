#!/usr/bin/env bash
set -euo pipefail
here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
exec "$here/../../scripts/harn_bin.sh" -- run --no-sandbox "$here/run_qmode.harn" -- "$@"
