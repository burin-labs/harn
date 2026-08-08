#!/usr/bin/env bash
set -euo pipefail

script_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd -P)"
# shellcheck source=scripts/lib/harn_bin.sh
source "$script_dir/lib/harn_bin.sh"

if [[ $# -ne 2 ]]; then
  echo "usage: scripts/snapshot_harn_bin.sh SOURCE_BIN DESTINATION_DIR" >&2
  exit 2
fi

harn_snapshot_binary "$1" "$2"
