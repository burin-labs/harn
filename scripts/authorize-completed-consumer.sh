#!/usr/bin/env bash
# Attach the exact dispatched child's completed proof through the existing
# publication authorization. This does not change the original timeout verdict.
set -euo pipefail
script_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=scripts/lib/release_consumer_verdict.sh
source "$script_dir/lib/release_consumer_verdict.sh"

[[ "${GITHUB_RUN_ID:-}" =~ ^[1-9][0-9]*$ &&
   "${GITHUB_RUN_ATTEMPT:-}" =~ ^[1-9][0-9]*$ &&
   "${AUTHORIZATION_FILE:-}" == /* ]] || {
  echo '::error::Missing authorization invocation identity or private output path.' >&2
  exit 1
}
[[ ! -e "$AUTHORIZATION_FILE" && ! -L "$AUTHORIZATION_FILE" ]] || {
  echo '::error::Authorization output already exists; stale proof refused.' >&2
  exit 1
}
receipt="$(release_authenticated_late_consumer "${GITHUB_REPOSITORY:?repository required}" \
  "${CANDIDATE_RUN_ID:?producer required}" "${SOURCE_SHA:?source required}" \
  "${CANARY_REPOSITORY:?registered consumer required}" \
  "${COMPLETED_CONSUMER_RUN_ID:?completed consumer required}")" || {
  echo '::error::Completed consumer authorization refused; publication remains unqualified.' >&2
  exit 1
}
receipt="$(jq -ce --arg run "$GITHUB_RUN_ID" --arg attempt "$GITHUB_RUN_ATTEMPT" \
  '. + {authorization_run:($run|tonumber),authorization_attempt:($attempt|tonumber)}' <<< "$receipt")"
[[ ${#receipt} -le 65536 ]] || {
  echo '::error::Authorization receipt exceeds its bounded schema transport.' >&2
  exit 1
}
umask 077
temporary="$(mktemp "${AUTHORIZATION_FILE}.XXXXXX")"
trap 'rm -f "$temporary"' EXIT
printf '%s\n' "$receipt" > "$temporary"
mv "$temporary" "$AUTHORIZATION_FILE"
if [[ -n "${GITHUB_OUTPUT:-}" ]]; then
  printf 'source_sha=%s\nproducer_run=%s\nconsumer_run=%s\n' \
    "$SOURCE_SHA" "$CANDIDATE_RUN_ID" "$COMPLETED_CONSUMER_RUN_ID" >> "$GITHUB_OUTPUT"
fi
jq -r '"Completed exact-source consumer authorized: producer="+(.producer_run|tostring)+
  " child="+(.consumer_run|tostring)+" observed="+(.consumer_observed|tostring)+" pending=0 bad=0"' <<< "$receipt"
