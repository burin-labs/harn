#!/usr/bin/env bash
# `audit --docs-contracts-proven-by` lets only the CI residual rehearsal lean on
# the sibling job that runs `make check-docs`. Every other caller must be
# refused before any audit work starts, so a release can never skip the
# documentation contracts by passing it.
set -euo pipefail
repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
out="$(mktemp "${TMPDIR:-/tmp}/harn-release-gate-docs-proof.XXXXXX")"
trap 'rm -f "$out"' EXIT

refused() {
  local expected="$1"
  shift
  if "$@" >"$out" 2>&1; then
    echo "release_gate accepted: $*" >&2
    exit 1
  fi
  if ! grep -Fq -- "$expected" "$out"; then
    echo "release_gate refused for another reason: $*" >&2
    cat "$out" >&2
    exit 1
  fi
}

only_ci="--docs-contracts-proven-by is only valid with --residual-only inside GitHub Actions"
refused "$only_ci" env -u GITHUB_ACTIONS \
  "$repo_root/scripts/release_gate.sh" audit --residual-only --docs-contracts-proven-by "Docs job"
refused "$only_ci" env GITHUB_ACTIONS=true \
  "$repo_root/scripts/release_gate.sh" audit --source-only --docs-contracts-proven-by "Docs job"
refused "$only_ci" env GITHUB_ACTIONS=true \
  "$repo_root/scripts/release_gate.sh" audit --receipt /nonexistent --docs-contracts-proven-by "Docs job"
refused "requires a CI job name" env GITHUB_ACTIONS=true \
  "$repo_root/scripts/release_gate.sh" audit --residual-only --docs-contracts-proven-by

echo "release_gate_docs_proof_test: ok"
