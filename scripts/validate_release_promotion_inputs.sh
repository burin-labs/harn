#!/usr/bin/env bash
# Closed publication intent. A partial tuple never falls back to rebuilding.
set -euo pipefail
if [[ "${INPUT_PROMOTE_ONLY:-false}" != true ]]; then
  [[ -z "${PROMOTION_RUN:-}${PROMOTION_MANIFEST_ID:-}${PROMOTION_MANIFEST_DIGEST:-}${PROMOTION_SOURCE:-}${PROMOTION_ARCHIVE_POLICY:-}${PROMOTION_POLICY:-}" ]] || { echo 'error: archive proof inputs require promote_only=true' >&2; exit 2; }
  exit 0
fi
[[ "${EVENT_NAME:-}" == workflow_dispatch && "${REF_TYPE:-}" == branch && "${REF_NAME:-}" == main ]] || { echo 'error: promotion requires workflow_dispatch on main' >&2; exit 2; }
[[ "${PROMOTION_POLICY:-}" =~ ^[0-9a-f]{40}$ && "$PROMOTION_POLICY" == "${GITHUB_SHA:-}" ]] || { echo 'error: expected_policy_sha does not match workflow execution commit' >&2; exit 2; }
[[ "${PROMOTION_SOURCE:-}" =~ ^[0-9a-f]{40}$ && "${PROMOTION_ARCHIVE_POLICY:-}" =~ ^[0-9a-f]{40}$ && "${PROMOTION_RUN:-}" =~ ^[1-9][0-9]*$ && "${PROMOTION_MANIFEST_ID:-}" =~ ^[1-9][0-9]*$ && "${PROMOTION_MANIFEST_DIGEST:-}" =~ ^sha256:[0-9a-f]{64}$ && -n "${INPUT_TAG:-}" ]] || { echo 'error: promotion requires complete source, producer, manifest and policy identity' >&2; exit 2; }
[[ "${INPUT_WARM_CACHE_ONLY:-false}" != true && "${INPUT_BENCHMARK_ONLY:-false}" != true && "${INPUT_CANDIDATE_ONLY:-false}" != true && "${FORCE_REBUILD:-false}" != true && -z "${INPUT_TARGETS:-}${INPUT_LEGACY_PROVENANCE_OVERRIDE:-}" && "${INPUT_RUNNER_PROFILE:-policy}" == policy ]] || { echo 'error: promotion cannot be mixed with build, benchmark or legacy override inputs' >&2; exit 2; }
