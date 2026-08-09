#!/usr/bin/env bash
set -euo pipefail

script_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=scripts/lib/release_version.sh
source "$script_dir/lib/release_version.sh"

if [[ $# -lt 1 ]] || ! release_tag_is_canonical "$1"; then
  echo "usage: scripts/check_release_smoke.sh vVERSION" >&2
  exit 2
fi

exec "$script_dir/harn_bin.sh" run "$script_dir/check_release_smoke.harn" -- "$@"
