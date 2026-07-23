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

echo "build_revision_workflow_test: CI and release builds attest source revisions"
