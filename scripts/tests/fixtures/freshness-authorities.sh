#!/usr/bin/env bash
set -euo pipefail

library="$1"
repository="$2"
output="$3"
harn_repo_root() {
  printf '%s\n' "$repository"
}
# shellcheck source=../../lib/harn_bin_freshness.sh
source "$library"
harn_write_freshness_authority_list "$output"
