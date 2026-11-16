#!/usr/bin/env bash

# shellcheck source=scripts/lib/release_version.sh
source "$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)/release_version.sh"

# Read the immutable release's complete pairing range. Callers keep the text
# private and may separately report the public base, revision and commit count.
# shellcheck disable=SC2034 # public result globals are consumed by sourcing callers
consumer_release_pairing_source() {
  local checkout=${1:-} revision=${2:-} version=${3:-}
  local measured base count text
  CONSUMER_PAIRING_TEXT='' CONSUMER_PAIRING_BASE='' CONSUMER_PAIRING_COUNT=''
  if [[ -z "$checkout" || ! "$revision" =~ ^[0-9a-f]{40}$ ]] \
    || ! release_version_is_canonical "$version" \
    || release_version_is_prerelease "$version"; then
    echo 'CONSUMER_OBSERVER reason=pairing_range_unmeasured' >&2
    return 1
  fi
  if ! measured=$(git -C "$checkout" rev-parse --verify "${revision}^{commit}" 2>/dev/null) \
    || [[ "$measured" != "$revision" ]] \
    || ! base=$(release_previous_stable_tag "$version" "$checkout") \
    || ! count=$(git -C "$checkout" rev-list --count "$base..$revision" 2>/dev/null) \
    || [[ ! "$count" =~ ^[0-9]+$ ]] \
    || ! text=$(git -C "$checkout" log --format='%B' "$base..$revision" 2>/dev/null); then
    echo 'CONSUMER_OBSERVER reason=pairing_range_unmeasured' >&2
    return 1
  fi
  CONSUMER_PAIRING_TEXT=$text CONSUMER_PAIRING_BASE=$base CONSUMER_PAIRING_COUNT=$count
}
