#!/usr/bin/env bash
# Accept either the producer's measured rehearsal or this recovery's exact
# successful downstream result. Missing/partial results never authorize.
set -euo pipefail
[[ "${SOURCE_SHA:-}" =~ ^[0-9a-f]{40}$ ]] || {
  echo '::error::Consumer authorization has no valid release source.' >&2
  exit 1
}
root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
# Recovery outputs do not authenticate a composition's current private graph.
# Read the immutable release range even on these routes, rather than trusting
# a caller-provided declaration or the current checkout's commit messages.
if ! pairing_text=$(bash "$root/scripts/lib/consumer_pairing_source.sh" \
    --release-source "${SOURCE_CHECKOUT:-}" "$SOURCE_SHA") \
  || ! declaration=$(printf '%s' "$pairing_text" | node "$root/scripts/ci/consumer-pairing.mjs" --declaration) \
  || ! kind=$(jq -er '.kind | select(. == "ordinary" or . == "composition")' <<< "$declaration"); then
  echo '::error::CONSUMER_OBSERVER reason=authorization_source_unmeasured' >&2
  exit 1
fi
if [[ "$kind" == composition ]] && [[ "${REQUIRES_REHEARSAL:-}" == true \
  || "${REQUIRES_ATTACHED_CONSUMER:-false}" == true \
  || "${REQUIRES_CONSUMER_REPAIR:-false}" == true || "${REPAIR_REUSED:-false}" == true ]]; then
  echo '::error::CONSUMER_OBSERVER reason=composition_recovery_unsupported' >&2
  exit 1
fi
case "${REQUIRES_ATTACHED_CONSUMER:-false}" in
  true)
    [[ "${REQUIRES_REHEARSAL:-}" == false && "${REHEARSAL_RESULT:-}" == skipped &&
       "${ATTACHED_RESULT:-}" == success && "${ATTACHED_SOURCE_SHA:-}" == "$SOURCE_SHA" &&
       "${CANDIDATE_RUN_ID:-}" =~ ^[1-9][0-9]*$ &&
       "${ATTACHED_PRODUCER_RUN:-}" == "$CANDIDATE_RUN_ID" ]] || {
      echo '::error::Attached completed-child authorization is absent or mismatched.' >&2
      exit 1
    }
    ;;
  false)
    [[ "${ATTACHED_RESULT:-skipped}" == skipped ]] || {
      echo '::error::Unexpected attached-child authorization.' >&2
      exit 1
    }
    ;;
  *) echo '::error::Unreported attached-child decision.' >&2; exit 1 ;;
esac
case "${REPAIR_REUSED:-false}" in
  true)
    [[ "${REQUIRES_CONSUMER_REPAIR:-false}" == true && "${REQUIRES_REHEARSAL:-}" == true &&
       "${REHEARSAL_RESULT:-}" == skipped && -z "${REHEARSAL_VERDICT:-}" &&
       -z "${REHEARSAL_SOURCE_SHA:-}" && "${REPAIR_RESULT:-}" == success ]] || {
      echo '::error::Prior consumer repair was not authenticated.' >&2
      exit 1
    }
    ;;
  false) ;;
  *) echo '::error::Unreported prior repair decision.' >&2; exit 1 ;;
esac
case "${REQUIRES_REHEARSAL:-}" in
  false)
    [[ "${REHEARSAL_RESULT:-}" == skipped ]] || {
      echo '::error::Unexpected recovery rehearsal for a producer-qualified candidate.' >&2
      exit 1
    }
    ;;
  true)
    [[ "${REPAIR_REUSED:-false}" == true ]] || [[ "${REHEARSAL_RESULT:-}" == success &&
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
case "${REQUIRES_CONSUMER_REPAIR:-false}" in
  true)
    if ! [[ "${REQUIRES_ATTACHED_CONSUMER:-false}" == false &&
       "${REQUIRES_REHEARSAL:-}" == true && "${REPAIR_RESULT:-}" == success ]] ||
      ! jq -e --arg source "$SOURCE_SHA" --arg producer "${CANDIDATE_RUN_ID:-}" \
        --arg attempt "${PRODUCER_ATTEMPT:-}" --arg pull "${CONSUMER_PULL:-}" '
        .schema == "harn.release.consumer_repair.v1" and .context.source_revision == $source and
        .producer_run == $producer and (.producer_attempt | tostring) == $attempt and
        .consumer_pull == $pull and ($producer | test("^[1-9][0-9]*$")) and
        ($attempt | test("^[1-9][0-9]*$")) and ($pull | test("^[1-9][0-9]*$"))
      ' <<< "${REPAIR_REQUEST:-}" >/dev/null; then
        echo '::error::Authenticated consumer repair is absent or mismatched.' >&2
        exit 1
    fi
    ;;
  false)
    [[ "${REPAIR_RESULT:-skipped}" == skipped && -z "${REPAIR_REQUEST:-}" ]] || {
      echo '::error::Unexpected consumer repair authorization.' >&2
      exit 1
    }
    ;;
  *) echo '::error::Unreported consumer repair decision.' >&2; exit 1 ;;
esac
echo 'ready=true' >> "${GITHUB_OUTPUT:?output required}"
