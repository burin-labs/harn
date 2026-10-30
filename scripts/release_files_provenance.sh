#!/usr/bin/env bash
# Project the manifest's file identities into the signed release-files predicate.
set -euo pipefail
script_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=scripts/lib/candidate_archive_contract.sh
source "$script_dir/lib/candidate_archive_contract.sh"

jq -e --arg schema "$CANDIDATE_MANIFEST_SCHEMA" --arg version "${2:?version required}" '
  if .schemaVersion != $schema or
    (.repository | type) != "string" or (.sourceCommit | type) != "string" or
    (.runId | type) != "string" or (.runAttempt | type) != "string"
  then error("invalid candidate manifest provenance") else . end |
  [.artifacts[] | select(.kind != "archive") | {file, kind, sha256}] as $files |
  if ($files | length) == 0 or
    ($files | map(.file) | unique | length) != ($files | length) or
    any($files[]; (.file | type) != "string" or (.file | length) == 0 or
      (.kind | type) != "string" or (.kind | length) == 0 or
      (.sha256 | type) != "string" or (.sha256 | test("^[0-9a-f]{64}$") | not))
  then error("invalid release file identities") else {
    schemaVersion: "harn.release_files_provenance.v1",
    phase: "candidate",
    repository: .repository,
    sourceCommit: .sourceCommit,
    version: $version,
    workflow: {runId: .runId, runAttempt: .runAttempt},
    files: $files
  } end
' "${1:?candidate manifest required}"
