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
  if [[ "$full" == false && ( -z "${GITHUB_EVENT_PATH:-}" || ! -f "${GITHUB_EVENT_PATH:-}" ) ]]; then
    echo '::notice::Event scope is unavailable; selecting full suite.'
    full=true
  fi
  if [[ "$full" == false && -n "${GITHUB_EVENT_PATH:-}" && -f "${GITHUB_EVENT_PATH:-}" ]]; then
    if [[ "$EVENT_NAME" == merge_group ]]; then
      base=$(jq -er '.merge_group.base_sha' "$GITHUB_EVENT_PATH")
      head=$(jq -er '.merge_group.head_sha' "$GITHUB_EVENT_PATH")
      [[ "$base" =~ ^[0-9a-f]{40}$ && "$head" =~ ^[0-9a-f]{40}$ ]] || exit 1
      commits=$(gh api --paginate "repos/${GITHUB_REPOSITORY:?}/compare/$base...$head?per_page=100" --jq '.commits[].sha')
      [[ -n "$commits" ]] || { echo 'Merge group has no measured commits' >&2; exit 1; }
      body=''
      measured_prs=0
      while IFS= read -r sha; do
        prs=$(gh api --paginate --slurp "repos/$GITHUB_REPOSITORY/commits/$sha/pulls?per_page=100")
        measured_prs=$((measured_prs + $(jq -er 'add | length' <<< "$prs")))
        bodies=$(jq -r 'add | .[].body // ""' <<< "$prs")
        body+="$bodies"$'\n'
      done <<< "$commits"
      if (( measured_prs == 0 )); then
        echo '::notice::No pull request scope could be measured; selecting full suite.'
        full=true
      fi
    else
      number=$(jq -er '.number // .pull_request.number' "$GITHUB_EVENT_PATH")
      [[ "$number" =~ ^[1-9][0-9]*$ ]] || exit 1
      # A rerun retains its original event payload. Read the current body so
      # adding scope and rerunning CI actually applies the declaration.
      body=$(gh api "repos/${GITHUB_REPOSITORY:?}/pulls/$number" --jq '.body // ""')
    fi
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
