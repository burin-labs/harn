#!/usr/bin/env bash

# Shared contract for candidate archive receipts and promotion manifests.
# Sourced by write/assemble/verify helpers and their structural tests.

CANDIDATE_ARCHIVE_SCHEMA="harn.candidate_archive_manifest.v1"
CANDIDATE_RECEIPT_SCHEMA="harn.candidate_archive_receipt.v1"

EXPECTED_RELEASE_ARCHIVES=(
  harn-aarch64-apple-darwin.tar.gz
  harn-aarch64-unknown-linux-gnu.tar.gz
  harn-x86_64-apple-darwin.tar.gz
  harn-x86_64-pc-windows-msvc.zip
  harn-x86_64-unknown-linux-gnu.tar.gz
)

declare -A _CANDIDATE_TARGET_FOR_ARCHIVE=(
  ["harn-aarch64-apple-darwin.tar.gz"]="aarch64-apple-darwin"
  ["harn-aarch64-unknown-linux-gnu.tar.gz"]="aarch64-unknown-linux-gnu"
  ["harn-x86_64-apple-darwin.tar.gz"]="x86_64-apple-darwin"
  ["harn-x86_64-pc-windows-msvc.zip"]="x86_64-pc-windows-msvc"
  ["harn-x86_64-unknown-linux-gnu.tar.gz"]="x86_64-unknown-linux-gnu"
)

declare -A _CANDIDATE_ARCHIVE_FOR_TARGET=(
  ["aarch64-apple-darwin"]="harn-aarch64-apple-darwin.tar.gz"
  ["aarch64-unknown-linux-gnu"]="harn-aarch64-unknown-linux-gnu.tar.gz"
  ["x86_64-apple-darwin"]="harn-x86_64-apple-darwin.tar.gz"
  ["x86_64-pc-windows-msvc"]="harn-x86_64-pc-windows-msvc.zip"
  ["x86_64-unknown-linux-gnu"]="harn-x86_64-unknown-linux-gnu.tar.gz"
)

candidate_archive_expected_targets_json() {
  jq -n '[
    "aarch64-apple-darwin",
    "aarch64-unknown-linux-gnu",
    "x86_64-apple-darwin",
    "x86_64-pc-windows-msvc",
    "x86_64-unknown-linux-gnu"
  ]'
}

target_for_archive() {
  local archive="${1:-}"
  if [[ -z "${_CANDIDATE_TARGET_FOR_ARCHIVE[$archive]+x}" ]]; then
    echo "error: unknown release archive: $archive" >&2
    return 1
  fi
  printf '%s\n' "${_CANDIDATE_TARGET_FOR_ARCHIVE[$archive]}"
}

archive_for_target() {
  local target="${1:-}"
  if [[ -z "${_CANDIDATE_ARCHIVE_FOR_TARGET[$target]+x}" ]]; then
    echo "error: unknown release target: $target" >&2
    return 1
  fi
  printf '%s\n' "${_CANDIDATE_ARCHIVE_FOR_TARGET[$target]}"
}

sha256_file() {
  if command -v sha256sum >/dev/null 2>&1; then
    sha256sum "$1" | awk '{print $1}'
  else
    shasum -a 256 "$1" | awk '{print $1}'
  fi
}

validate_candidate_manifest_json() {
  local manifest_file="${1:-}"
  local expected_targets_json
  expected_targets_json="$(candidate_archive_expected_targets_json)"

  if [[ -z "$manifest_file" || ! -f "$manifest_file" ]]; then
    echo "error: candidate archive manifest does not exist: ${manifest_file:-<empty>}" >&2
    return 1
  fi

  if ! jq -e \
    --arg schema "$CANDIDATE_ARCHIVE_SCHEMA" \
    --argjson expectedTargets "$expected_targets_json" \
    '
    . as $doc |
    $doc.schemaVersion == $schema and
    ($doc.sourceCommit | type == "string" and test("^[0-9a-f]{40}$")) and
    ($doc.policyRevision | type == "string" and test("^[0-9a-f]{40}$")) and
    ($doc.runId | type == "string" and test("^[0-9]+$")) and
    ($doc.runAttempt | type == "string" and test("^[0-9]+$")) and
    ($doc.archives | type == "object") and
    ([$doc.archives | keys[]] | sort) == ($expectedTargets | sort) and
    all($expectedTargets[];
      . as $target |
      $doc.archives[$target] as $entry |
      ($entry.archive | type == "string" and length > 0) and
      ($entry.sha256 | type == "string" and test("^[0-9a-f]{64}$")) and
      ($entry.signingStatus | IN("signed", "not_applicable")) and
      ($entry.notarizationStatus | IN("notarized", "not_applicable")) and
      ($entry.attestationIdentity | type == "string" and length > 0) and
      ($entry.runId | type == "string" and test("^[0-9]+$")) and
      ($entry.runAttempt | type == "string" and test("^[0-9]+$"))
    )
    ' "$manifest_file" >/dev/null; then
    echo "error: candidate archive manifest is malformed or incomplete: $manifest_file" >&2
    return 1
  fi

  local archive target expected_archive
  for target in $(jq -r '.archives | keys[]' "$manifest_file" | LC_ALL=C sort); do
    archive="$(jq -r --arg target "$target" '.archives[$target].archive' "$manifest_file")"
    expected_archive="$(archive_for_target "$target")"
    if [[ "$archive" != "$expected_archive" ]]; then
      echo "error: manifest target $target binds unexpected archive $archive (expected $expected_archive)" >&2
      return 1
    fi
  done
}
