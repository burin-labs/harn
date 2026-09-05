#!/usr/bin/env bash
set -euo pipefail

sha="$(git rev-parse origin/main)"
state=failure
if [[ "${CHECK_OUTCOME:-}" == success ]]; then
  case "${MEASURED_STATE:-}" in
    success|pending) state="$MEASURED_STATE" ;;
  esac
fi
description="${DESCRIPTION:-development cutover measurement failed}"
gh api "repos/${GH_REPO:?}/statuses/$sha" -X POST \
  -f state="$state" \
  -f context="development cutover" \
  -f description="${description:0:139}" \
  -f target_url="${TARGET_URL:?}" >/dev/null
if [[ "$state" == failure ]]; then
  echo "::error::$description"
  exit 1
fi
