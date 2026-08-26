#!/usr/bin/env bash

# Source-state proof for the combined conformance and audit gate. Git owns the
# tested commit and clean state. The existing Harn binary freshness receipt (or
# CI's verified Rust artifact manifest) owns the executable identity.

# shellcheck source=scripts/lib/harn_bin.sh
source "$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd -P)/harn_bin.sh"

harn_source_gate_require_clean() {
  local repo_root="$1"
  if ! git -C "$repo_root" diff --quiet --ignore-submodules -- || \
     ! git -C "$repo_root" diff --cached --quiet --ignore-submodules -- || \
     [[ -n "$(git -C "$repo_root" ls-files --others --exclude-standard)" ]]; then
    echo "error: source gate evidence requires a clean tracked, index, and untracked state" >&2
    return 1
  fi
}

harn_source_gate_sha256() {
  if command -v sha256sum >/dev/null 2>&1; then
    sha256sum "$1" | cut -d ' ' -f 1
  else
    shasum -a 256 "$1" | cut -d ' ' -f 1
  fi
}

harn_source_gate_binary_identity() {
  local bin="$1"
  local head="$2"
  local receipt=""
  local actual_hash=""

  harn_require_executable_bin "$bin" || return $?
  receipt="$(harn_binary_freshness_receipt_path "$bin")" || return $?
  if [[ -r "$receipt" ]]; then
    harn_verified_build_freshness_id "$bin"
    return
  fi

  # A downloaded CI executable has no access to the producer's absolute Cargo
  # dep-info paths. The Rust artifact adapter verifies its committed manifest,
  # exact commit, and executable digest before exporting these values.
  if [[ "${GITHUB_ACTIONS:-false}" != "true" ]] || \
     [[ "${SOURCE_GATE_CI_BINARY_COMMIT:-}" != "$head" ]] || \
     [[ ! "${SOURCE_GATE_CI_BINARY_BUILD_FRESHNESS_ID:-}" =~ ^([0-9a-f]{40}|[0-9a-f]{64})$ ]] || \
     [[ ! "${SOURCE_GATE_CI_BINARY_SHA256:-}" =~ ^[0-9a-f]{64}$ ]]; then
    echo "error: explicit HARN_BIN cannot certify repository source without a matching freshness receipt" >&2
    return 1
  fi
  actual_hash="$(harn_source_gate_sha256 "$bin")" || return $?
  if [[ "$actual_hash" != "$SOURCE_GATE_CI_BINARY_SHA256" ]]; then
    echo "error: hosted Harn binary changed after Rust artifact verification" >&2
    return 1
  fi
  printf '%s\n' "$SOURCE_GATE_CI_BINARY_BUILD_FRESHNESS_ID"
}

harn_source_gate_begin() {
  local receipt="$1"
  local pr_number="${2:-}"
  local repo_root=""
  local remote_head="-"

  repo_root="$(harn_repo_root)" || return $?
  rm -f "$receipt"
  harn_source_gate_require_clean "$repo_root" || return $?
  source_gate_head="$(git -C "$repo_root" rev-parse --verify HEAD)" || return $?
  source_gate_remote_head="-"
  if [[ -n "$pr_number" ]]; then
    remote_head="$(gh pr view "$pr_number" --repo burin-labs/harn --json headRefOid --jq .headRefOid)" || return $?
    if [[ "$remote_head" != "$source_gate_head" ]]; then
      echo "error: remote PR #$pr_number head $remote_head does not match tested commit $source_gate_head" >&2
      return 1
    fi
    source_gate_remote_head="$remote_head"
  fi
}

harn_source_gate_bind_binary() {
  local bin="$1"
  local directory=""
  local name=""

  source_gate_build_id="$(harn_source_gate_binary_identity "$bin" "$source_gate_head")" || return $?
  directory="$(cd "$(dirname "$bin")" && pwd -P)" || return $?
  name="$(basename "$bin")"
  source_gate_binary="$directory/$name"
}

harn_source_gate_finish() {
  local receipt="$1"
  local runtime_mode="$2"
  local subtask_placement="$3"
  local audit_jobs="$4"
  local conformance_jobs="$5"
  local summary="$6"
  shift 6
  local repo_root=""
  local current_head=""
  local current_build_id=""
  local terminal_utc=""

  repo_root="$(harn_repo_root)" || return $?
  current_head="$(git -C "$repo_root" rev-parse --verify HEAD)" || return $?
  if [[ "$current_head" != "$source_gate_head" ]]; then
    echo "error: Git head changed while the source gate ran" >&2
    return 1
  fi
  harn_source_gate_require_clean "$repo_root" || return $?
  current_build_id="$(harn_source_gate_binary_identity "$source_gate_binary" "$current_head")" || return $?
  if [[ "$current_build_id" != "$source_gate_build_id" ]]; then
    echo "error: Harn binary freshness changed while the source gate ran" >&2
    return 1
  fi
  terminal_utc="$(date -u +%Y-%m-%dT%H:%M:%SZ)" || return $?
  "$source_gate_binary" __internal-source-gate-receipt-v1 write \
    "$receipt" "$current_head" "$source_gate_remote_head" \
    "$source_gate_binary" "$current_build_id" "$runtime_mode" \
    "$subtask_placement" "$audit_jobs" "$conformance_jobs" \
    "$terminal_utc" "$summary" -- "$@" || return $?
  if ! harn_source_gate_require_clean "$repo_root"; then
    rm -f "$receipt"
    echo "error: receipt path must be ignored or outside the tested checkout" >&2
    return 1
  fi
}

harn_source_gate_verify() {
  local receipt="$1"
  local bin="$2"
  local repo_root=""
  local head=""
  local build_id=""
  local directory=""

  repo_root="$(harn_repo_root)" || return $?
  harn_source_gate_require_clean "$repo_root" || return $?
  head="$(git -C "$repo_root" rev-parse --verify HEAD)" || return $?
  build_id="$(harn_source_gate_binary_identity "$bin" "$head")" || return $?
  directory="$(cd "$(dirname "$bin")" && pwd -P)" || return $?
  bin="$directory/$(basename "$bin")"
  "$bin" __internal-source-gate-receipt-v1 verify "$receipt" "$head" "$bin" "$build_id"
}
