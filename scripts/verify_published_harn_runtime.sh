#!/usr/bin/env bash
# Bind the installed orchestration tool to the published source and signed index.
set -euo pipefail

if [[ $# != 4 ]]; then
  echo 'usage: verify_published_harn_runtime.sh <receipt.json> <cache-dir> <source-sha> <version>' >&2
  exit 2
fi
receipt="$1" cache_dir="$2" expected_source="$3" version="$4"
release_repository="${GH_REPO:?GH_REPO required}"
script_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd -P)"
# shellcheck source=scripts/lib/candidate_archive_contract.sh
source "$script_dir/lib/candidate_archive_contract.sh"
# shellcheck source=scripts/lib/published_release_correction.sh
source "$script_dir/lib/published_release_correction.sh"

[[ "$expected_source" =~ ^[0-9a-f]{40}$ && "$version" =~ ^[0-9]+\.[0-9]+\.[0-9]+$ ]]
[[ "$release_repository" =~ ^[A-Za-z0-9_.-]+/[A-Za-z0-9_.-]+$ ]]
[[ "$(git rev-parse "refs/tags/v$version^{commit}")" == "$expected_source" ]]
# This profile is the Linux registry publisher, not source-build freshness or
# a general installer. Other release consumers retain their existing adapters.
jq -e --arg version "$version" --arg repository "$release_repository" '
  .schema_version == "harn-bootstrap-v1" and .version == $version and
  .target == "x86_64-unknown-linux-gnu" and
  (.binary_path | type == "string" and startswith("/") and length > 1) and
  (.binary_sha256 | type == "string" and test("^[0-9a-f]{64}$")) and
  (.checksum | type == "string" and test("^[0-9a-f]{64}$")) and
  .source == ("https://github.com/" + $repository + "/releases/download/v" +
    $version + "/harn-x86_64-unknown-linux-gnu.tar.xz")
' "$receipt" >/dev/null
binary="$(jq -er '.binary_path' "$receipt")"
checksum="$(jq -er '.checksum' "$receipt")"
binary_sha="$(jq -er '.binary_sha256' "$receipt")"
archive="$cache_dir/downloads/$version/harn-x86_64-unknown-linux-gnu.tar.xz"
[[ -x "$binary" && "$(sha256_file "$archive")" == "$checksum" ]]
scratch="$(mktemp -d "${TMPDIR:-/tmp}/harn-published-runtime.XXXXXX")"
trap 'rm -rf "$scratch"' EXIT

# Reuse the owning publisher proof: source signature, closed publisher actor,
# complete archive inventory, signed index, workflow/source/run and timestamps.
release_require_published_correction "$release_repository" "v$version" "$expected_source" "$scratch/index.json"
jq -e --arg checksum "$checksum" '
  .assets["x86_64-unknown-linux-gnu"].sha256 == $checksum
' "$scratch/index.json" >/dev/null

# A forged installer receipt cannot authorize another binary. Bind the actual
# executable to the authenticated archive member before executing that tool.
tar -xOf "$archive" harn >"$scratch/harn"
[[ "$(sha256_file "$scratch/harn")" == "$binary_sha" &&
   "$(sha256_file "$binary")" == "$binary_sha" ]]
"$binary" version --json >"$scratch/version.json"
jq -e --arg source "$expected_source" --arg version "$version" '
  .ok == true and .data.version == $version and .data.source_revision == $source
' "$scratch/version.json" >/dev/null
jq -n --arg source "$expected_source" --arg version "$version" \
  --arg path "$binary" --arg archive_sha256 "$checksum" --arg binary_sha256 "$binary_sha" \
  '{schema:"harn.published_orchestration_runtime.v1",source:$source,version:$version,
    path:$path,archive_sha256:$archive_sha256,binary_sha256:$binary_sha256}'
