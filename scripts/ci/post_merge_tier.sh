#!/usr/bin/env bash
set -euo pipefail

# The post-merge tier (harn#9495): pull requests and merge groups leave the
# slow proof families to the push to main, which always runs them. A change
# that selects the full suite runs them before it lands: one that touches a
# `full_suite` path, or a pull request whose body declares `CI-Scope: full`.
# This decision runs before a Harn artifact is available.
: "${EVENT_NAME:?EVENT_NAME is required}"
: "${GITHUB_OUTPUT:?GITHUB_OUTPUT is required}"

active=false
if [[ "$EVENT_NAME" == pull_request || "$EVENT_NAME" == merge_group ]]; then
  # The path filter answers exactly `true` or `false`; anything else is an
  # unmeasured change, and an unmeasured change runs everything.
  case "${FULL_SUITE_PATHS:-}" in
    true) full=true ;;
    false) full=false ;;
    *)
      echo "::notice title=Post-merge tier::full-suite path filter unmeasured (${FULL_SUITE_PATHS:-unset}); running every proof."
      full=true
      ;;
  esac
  if [[ "$EVENT_NAME" == pull_request && "$full" == false && -n "${GITHUB_EVENT_PATH:-}" && -f "${GITHUB_EVENT_PATH:-}" ]]; then
    body="$(jq -r '.pull_request.body // ""' "$GITHUB_EVENT_PATH")"
    while IFS= read -r line; do
      if [[ "$line" =~ ^[[:space:]]*[Cc][Ii]-[Ss][Cc][Oo][Pp][Ee]:[[:space:]]*(.*)$ ]]; then
        scope="${BASH_REMATCH[1]//[[:space:]]/}"
        if [[ "$scope" != full ]]; then
          echo "::error title=Post-merge tier::CI-Scope: names unknown scope '${BASH_REMATCH[1]}'; harn declares only 'full'."
          exit 1
        fi
        full=true
      fi
    done <<< "${body//$'\r'/}"
  fi
  if [[ "$full" == false ]]; then
    active=true
    echo "::notice title=Post-merge tier::Slow proofs run after merge on main."
  fi
fi
printf 'active=%s\n' "$active" >> "$GITHUB_OUTPUT"
