#!/usr/bin/env bash
set -euo pipefail
root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd -P)"
cd "$root"
exec ./scripts/harn_bin.sh -- run scripts/check_agent_guidance.harn -- "$@"
