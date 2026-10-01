#!/usr/bin/env bash

# shellcheck disable=SC2034 # constants are consumed by sourcing scripts
# Shared contract for candidate archive receipts and the candidate manifest.
# Sourced by the receipt writer, the manifest assembler, and their tests. The
# manifest schema itself (burin-labs.candidate_manifest.v1) is owned by
# burin-labs/.github, which validates every manifest this repository writes.
#
# Keep this file free of bash-4 associative arrays. GitHub's macOS runners
# still invoke /bin/bash 3.2, and under `set -u` a key like
# `harn-x86_64-apple-darwin.tar.gz` is parsed as arithmetic (`harn - ...`).

CANDIDATE_MANIFEST_SCHEMA="burin-labs.candidate_manifest.v1"
CANDIDATE_RECEIPT_SCHEMA="harn.candidate_archive_receipt.v1"
# The predicate type of the build attestation every candidate file carries.
RELEASE_ARCHIVE_PREDICATE_TYPE="https://harnlang.com/attestations/release-archive/v1"
# The run artifact holding the files built beside the archives.
RELEASE_FILES_ARTIFACT="harn-release-files"

EXPECTED_RELEASE_ARCHIVES=(
  harn-aarch64-apple-darwin.tar.gz
  harn-aarch64-unknown-linux-gnu.tar.gz
  harn-x86_64-apple-darwin.tar.gz
  harn-x86_64-pc-windows-msvc.zip
  harn-x86_64-unknown-linux-gnu.tar.gz
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
  case "$archive" in
    harn-aarch64-apple-darwin.tar.gz) printf '%s\n' "aarch64-apple-darwin" ;;
    harn-aarch64-unknown-linux-gnu.tar.gz) printf '%s\n' "aarch64-unknown-linux-gnu" ;;
    harn-x86_64-apple-darwin.tar.gz) printf '%s\n' "x86_64-apple-darwin" ;;
    harn-x86_64-pc-windows-msvc.zip) printf '%s\n' "x86_64-pc-windows-msvc" ;;
    harn-x86_64-unknown-linux-gnu.tar.gz) printf '%s\n' "x86_64-unknown-linux-gnu" ;;
    *)
      echo "error: unknown release archive: $archive" >&2
      return 1
      ;;
  esac
}

archive_for_target() {
  local target="${1:-}"
  case "$target" in
    aarch64-apple-darwin) printf '%s\n' "harn-aarch64-apple-darwin.tar.gz" ;;
    aarch64-unknown-linux-gnu) printf '%s\n' "harn-aarch64-unknown-linux-gnu.tar.gz" ;;
    x86_64-apple-darwin) printf '%s\n' "harn-x86_64-apple-darwin.tar.gz" ;;
    x86_64-pc-windows-msvc) printf '%s\n' "harn-x86_64-pc-windows-msvc.zip" ;;
    x86_64-unknown-linux-gnu) printf '%s\n' "harn-x86_64-unknown-linux-gnu.tar.gz" ;;
    *)
      echo "error: unknown release target: $target" >&2
      return 1
      ;;
  esac
}

# shellcheck source=scripts/lib/sha256.sh
source "$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd -P)/sha256.sh"

sha256_file() {
  sha256_file_hex "$1"
}
