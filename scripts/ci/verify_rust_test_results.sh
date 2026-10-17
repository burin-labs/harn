#!/usr/bin/env bash
# Aggregate the complete partition suite or the explicit archive fallback.
set -euo pipefail

require_result() {
  local name=$1 actual=$2 expected=$3
  printf '%s: %s (required: %s)\n' "$name" "${actual:-missing}" "$expected"
  case "$actual" in
    queued|in_progress|pending|waiting) pending=$((pending + 1)) ;;
  esac
  if [[ "$actual" != "$expected" ]]; then
    failed+=("$name")
  fi
}

pending=0
failed=()
require_result changes "${CHANGES_RESULT:-}" success
case "${RUST_SELECTED:-}" in
  true)
    require_result 'Build Rust test inputs' "${PRODUCER_RESULT:-}" success
    if [[ "${ARCHIVE_MODE:-}" == true ]]; then
      require_result 'Workspace archive ready' "${WORKSPACE_ARTIFACT_READY:-}" true
      require_result 'Rust test partitions' "${PARTITIONS_RESULT:-}" skipped
      require_result 'Rust archive tests' "${ARCHIVE_RESULT:-}" success
    else
      require_result 'Rust test partitions' "${PARTITIONS_RESULT:-}" success
      require_result 'Rust archive tests' "${ARCHIVE_RESULT:-}" skipped
    fi
    ;;
  false)
    require_result 'Build Rust test inputs' "${PRODUCER_RESULT:-}" skipped
    require_result 'Rust test partitions' "${PARTITIONS_RESULT:-}" skipped
    require_result 'Rust archive tests' "${ARCHIVE_RESULT:-}" skipped
    ;;
  *) failed+=('Rust path selection missing or invalid') ;;
esac
printf 'Rust test verdict: pending=%s failing=%s\n' "$pending" "${#failed[@]}"
if (( ${#failed[@]} )); then
  printf '::error::Required Rust proof did not complete: %s\n' "${failed[@]}"
  exit 1
fi
