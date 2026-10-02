#!/usr/bin/env bash
set -euo pipefail

# This decision runs before a Harn artifact is available.
: "${EVENT_NAME:?EVENT_NAME is required}"
: "${GITHUB_OUTPUT:?GITHUB_OUTPUT is required}"

active=false
if [[ "${SPRINT_FAST_CI:-}" == true ]] \
  && [[ "$EVENT_NAME" == pull_request || "$EVENT_NAME" == merge_group ]]; then
  active=true
  echo "::notice title=Sprint fast CI::Slow pre-merge proofs run after merge on main."
fi
printf 'active=%s\n' "$active" >> "$GITHUB_OUTPUT"
