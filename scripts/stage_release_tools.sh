#!/usr/bin/env bash
# Materialize the self-contained release toolchain used after the publish ref changes.
set -euo pipefail

if [[ $# -ne 1 || -z "$1" ]]; then
  echo "usage: stage_release_tools.sh <destination>" >&2
  exit 2
fi

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd -P)"
destination="$1"
if [[ -e "$destination" ]]; then
  echo "error: release-tools destination already exists: $destination" >&2
  exit 2
fi

mkdir -p "$destination"
for source in \
  release_ship.sh \
  release_gate.sh \
  release_contract.harn \
  release_contract.json \
  release_contract.env \
  verify_release_archive_provenance.sh \
  publish_certified_release_assets.sh \
  verify_release_tag_main_ancestry.sh \
  release_metadata.harn \
  release_withdrawals.harn \
  path_visibility.harn \
  npm_ci_with_retry.sh \
  publish.sh \
  publish.harn \
  publish_plan.harn \
  cargo_with_worktree_build_dir.sh \
  harn_bin.sh
do
  cp "$repo_root/scripts/$source" "$destination/$source"
done
cp -R "$repo_root/scripts/lib" "$destination/lib"
