#!/usr/bin/env bash
set -euo pipefail

repo_root="$(cd "$(dirname "$BASH_SOURCE")/../.." && pwd)"
ci_workflow="$repo_root/.github/workflows/ci.yml"
release_workflow="$repo_root/.github/workflows/build-release-binaries.yml"

if ! grep -Fq 'HARN_BUILD_REVISION:' "$ci_workflow"; then
  echo "build_revision_workflow_test: CI must attest its immutable commit" >&2
  exit 1
fi
if ! grep -Fq 'HARN_BUILD_REVISION=$(git rev-parse HEAD)' "$release_workflow"; then
  echo "build_revision_workflow_test: release builds must attest the checked-out commit" >&2
  exit 1
fi
if [[ "$(grep -Fc -- '- name: Verify benchmark source revision' "$release_workflow")" -ne 2 ]]; then
  echo "build_revision_workflow_test: benchmark AOT and build jobs must verify the exact source revision" >&2
  exit 1
fi
if ! grep -Fq 'benchmark_source_ref and benchmark_source_sha must be provided together' "$release_workflow"; then
  echo "build_revision_workflow_test: benchmark source ref/SHA must be an atomic contract" >&2
  exit 1
fi
if ! grep -Fq 'an explicit benchmark source must be dispatched from main so current policy is authoritative' "$release_workflow"; then
  echo "build_revision_workflow_test: immutable benchmark sources must use current-main policy" >&2
  exit 1
fi
if ! grep -Fq 'benchmark source resolved to $actual_source_sha; expected $EXPECTED_SOURCE_SHA' "$release_workflow"; then
  echo "build_revision_workflow_test: benchmark source mismatches must fail closed" >&2
  exit 1
fi

echo "build_revision_workflow_test: CI and release builds attest source revisions"
