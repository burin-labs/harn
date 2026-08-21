#!/usr/bin/env bash

# Shared policy for the release PR drift workflow. Keep ref recognition here so
# pull-request events, push refreshes, and regression fixtures cannot drift.

release_pr_version() {
  local ref="$1"
  local version_pattern='[0-9]+\.[0-9]+\.[0-9]+(-[0-9A-Za-z.-]+)?'

  if [[ "$ref" =~ ^release/v(${version_pattern})$ ]]; then
    printf '%s\n' "${BASH_REMATCH[1]}"
    return 0
  fi
  if [[ "$ref" =~ ^release-attempt/v(${version_pattern})/([0-9a-f]{40})$ ]]; then
    printf '%s\n' "${BASH_REMATCH[1]}"
    return 0
  fi
  return 1
}

release_pr_target_json() {
  local ref="$1" sha="$2" pr="$3" version
  version=$(release_pr_version "$ref") || return 64
  jq -nc \
    --arg ref "$ref" \
    --arg sha "$sha" \
    --arg pr "$pr" \
    --arg version "$version" \
    '{ref:$ref, sha:$sha, pr:$pr, version:$version}'
}

# Print the body of the first `## Unreleased` up to the next `## ` heading,
# with leading/trailing blank lines stripped. An absent section intentionally
# projects to an empty body: it is safe when both the release pin and main lack
# direct notes, while a later direct note still compares unequal and fails.
extract_unreleased() {
  awk '
    !done && /^## [Uu]nreleased[[:space:]]*$/ { in_section = 1; next }
    in_section && /^## / { in_section = 0; done = 1 }
    in_section { lines[++n] = $0 }
    END {
      first = 1
      while (first <= n && lines[first] ~ /^[[:space:]]*$/) first++
      last = n
      while (last >= first && lines[last] ~ /^[[:space:]]*$/) last--
      for (i = first; i <= last; i++) print lines[i]
    }
  '
}
