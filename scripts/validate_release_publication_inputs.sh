#!/usr/bin/env bash
set -euo pipefail
script_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=scripts/lib/release_version.sh
source "$script_dir/lib/release_version.sh"
[[ "${EVENT_NAME:-}" == workflow_dispatch && "${REF_TYPE:-}" == branch && "${REF_NAME:-}" == main ]] || { echo 'error: explicit publication requires workflow_dispatch on main' >&2; exit 2; }
release_tag_is_canonical "${INPUT_TAG:-}" || { echo 'error: tag must be a canonical release version' >&2; exit 2; }
[[ "${EXPECTED_POLICY_SHA:-}" =~ ^[0-9a-f]{40}$ && "$EXPECTED_POLICY_SHA" == "${GITHUB_SHA:-}" && "$EXPECTED_POLICY_SHA" == "$(git rev-parse HEAD)" ]] || { echo 'error: expected_policy_sha differs from execution policy' >&2; exit 2; }
[[ "${EXPECTED_SOURCE_SHA:-}" =~ ^[0-9a-f]{40}$ && "$EXPECTED_SOURCE_SHA" == "$(git rev-parse "refs/tags/$INPUT_TAG^{commit}")" ]] || { echo 'error: expected_source_sha differs from immutable tag target' >&2; exit 2; }
