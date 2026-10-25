#!/usr/bin/env bash
# Writes the gzip copies of the unix release archives that only the ACP agent
# registry manifest (spec/acp-registry/harn/agent.json) installs from. The
# registry rejects .tar.xz, so each copy is the same tar stream recompressed.
set -euo pipefail

usage() {
  echo "usage: build_acp_registry_archives.sh --archives-dir DIR --output-dir DIR" >&2
}

archives_dir=""
output_dir=""
while [[ $# -gt 0 ]]; do
  case "$1" in
    --archives-dir) archives_dir="${2:-}"; shift 2 ;;
    --output-dir) output_dir="${2:-}"; shift 2 ;;
    *) usage; exit 2 ;;
  esac
done
if [[ -z "$archives_dir" || -z "$output_dir" ]]; then
  usage
  exit 2
fi

root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd -P)"
# shellcheck source=scripts/lib/candidate_archive_contract.sh
source "$root/scripts/lib/candidate_archive_contract.sh"

mkdir -p "$output_dir"
for gz in "${ACP_REGISTRY_ARCHIVES[@]}"; do
  source_archive="$archives_dir/${gz%.tar.gz}.tar.xz"
  if [[ ! -f "$source_archive" ]]; then
    echo "error: missing $source_archive" >&2
    exit 1
  fi
  xz -dc "$source_archive" | gzip -9n > "$output_dir/$gz"
done
