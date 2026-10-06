#!/usr/bin/env bash
# Accept either the producer's measured rehearsal or this recovery's exact
# successful downstream result. Missing/partial results never authorize.
set -euo pipefail
[[ "${SOURCE_SHA:-}" =~ ^[0-9a-f]{40}$ ]] || {
  echo '::error::Consumer authorization has no valid release source.' >&2
  exit 1
}
case "${REQUIRES_REHEARSAL:-}" in
  false)
    [[ "${REHEARSAL_RESULT:-}" == skipped ]] || {
      echo '::error::Unexpected recovery rehearsal for a producer-qualified candidate.' >&2
      exit 1
    }
    ;;
  true)
    [[ "${REHEARSAL_RESULT:-}" == success &&
       "${REHEARSAL_VERDICT:-}" == pass &&
       "${REHEARSAL_SOURCE_SHA:-}" == "$SOURCE_SHA" ]] || {
      echo "::error::Recovery consumer rehearsal is unqualified: result=${REHEARSAL_RESULT:-missing} verdict=${REHEARSAL_VERDICT:-unmeasured} source=${REHEARSAL_SOURCE_SHA:-missing}." >&2
      exit 1
    }
    ;;
  *)
    echo '::error::Missing consumer rehearsal decision.' >&2
    exit 1
    ;;
esac
echo 'ready=true' >> "${GITHUB_OUTPUT:?output required}"
