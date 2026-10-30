#!/usr/bin/env bash
set -euo pipefail

script_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd -P)"
export HARN_EXT_PUBLISH_POLICY_ROOT="$script_dir"
exec "$script_dir/harn_bin.sh" -- run --no-sandbox "$script_dir/publish.harn" -- "$@"
