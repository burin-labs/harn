#!/usr/bin/env bash
# Exercise release staging and its offline publication/cutover fixtures before tagging.
set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd -P)"
cd "$repo_root"
export HARN_BIN_NO_BUILD=1
export HARN_BIN="${HARN_BIN:-$(command -v harn)}"
./scripts/harn_bin.sh -- run scripts/release_staging.harn
./scripts/harn_bin.sh -- test scripts/tests/release_staging_test.harn

# These execute the production owners against local repositories and API
# fixtures. The provenance suite calls stage_release_tools.sh itself, then
# executes the staged verifier with the contract removed and restored.
readonly suites=(
  verify_release_archive_provenance
  release_tag_main_ancestry
  release_version
  release_publication_policy
  prepare_development_version
  development_bump_cutover
  development_cutover_monitor
)
passed=0
for suite in "${suites[@]}"; do
  echo "Release rehearsal: running $suite"
  bash "scripts/tests/${suite}_test.sh"
  passed=$((passed + 1))
done
echo "Release rehearsal: total=${#suites[@]} success=$passed pending=0 red=0"
