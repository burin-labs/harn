#!/usr/bin/env bash
# Decide whether a pull request's owned-runner macOS check is satisfied for one exact head SHA.
#
# usage: owned_macos_pr_check.sh <owner/repo> <pr-number> <head-sha> [--timeout-minutes N]
#
# Exit codes:
#   0  satisfied: either the pull request touches nothing the macOS tests read (named, never
#      silent), or the newest "macOS workspace tests (owned)" check on that commit is a success
#      whose run is macos-pr-tests.yml, dispatched on main, for this pull request and this SHA.
#   3  pending: the check is required but absent or still running, and the commit is younger than
#      the timeout. The caller waits and asks again.
#   1  not satisfied: the check failed, was never produced within the timeout, or its run is not
#      the trusted one. A missing check is never a pass.
#   2  usage error.
#
# Which files make the check required is read from the `paths:` filter of macos-pr-dispatch.yml,
# so the dispatcher and this decision cannot drift apart.
set -euo pipefail

repo="${1:?usage: owned_macos_pr_check.sh <owner/repo> <pr-number> <head-sha> [--timeout-minutes N]}"
pr="${2:?missing pull request number}"
sha="${3:?missing head SHA}"
timeout_minutes=90
if [[ "${4:-}" == --timeout-minutes ]]; then timeout_minutes="${5:?missing minutes}"; fi
name="macOS workspace tests (owned)"
workflow_path=".github/workflows/macos-pr-tests.yml"
dispatcher="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)/.github/workflows/macos-pr-dispatch.yml"

[[ "$pr" =~ ^[0-9]+$ && "$timeout_minutes" =~ ^[0-9]+$ ]] || { echo "owned macOS check: pull request and timeout must be numbers" >&2; exit 2; }
[[ "$sha" =~ ^[0-9a-f]{40}$ ]] || { echo "owned macOS check: '$sha' is not a full commit SHA" >&2; exit 2; }

# The dispatcher's path filter, as anchored regular expressions.
patterns="$(awk '/^ +paths:/{on=1; next} on && /^ +- /{gsub(/^ +- +"|"$/, ""); print; next} on{exit}' "$dispatcher")"
[[ -n "$patterns" ]] || { echo "owned macOS check: no paths found in the dispatcher; refusing to guess" >&2; exit 2; }
to_regex() { sed -e 's/[.+^${}()|]/\\&/g' -e 's/\*\*/\x01/g' -e 's/\*/[^\/]*/g' -e 's/\x01/.*/g' -e 's/^/^/' -e 's/$/$/'; }

files="$(gh api "repos/${repo}/pulls/${pr}/files?per_page=100" --paginate | jq -r -s '[.[][]?.filename] | .[]')"
relevant=0
while IFS= read -r file; do
  [[ -n "$file" ]] || continue
  while IFS= read -r pattern; do
    if [[ "$file" =~ $(printf '%s' "$pattern" | to_regex) ]]; then relevant=1; break 2; fi
  done <<<"$patterns"
done <<<"$files"
if [[ -z "$files" ]]; then
  echo "owned macOS check: could not read the pull request's files; not a pass" >&2
  exit 1
fi
if [[ "$relevant" == 0 ]]; then
  echo "owned macOS check: not required, no changed file is read by the macOS tests"
  exit 0
fi

runs="$(gh api "repos/${repo}/commits/${sha}/check-runs?per_page=100" --paginate | jq -s '[.[].check_runs[]]')"
newest="$(jq -c --arg name "$name" --arg sha "$sha" \
  '[.[] | select(.name == $name and .head_sha == $sha)] | sort_by(.id) | last // empty' <<<"$runs")"

# Age of the commit's checks, from the earliest start of any check on it.
started="$(jq -r '[.[].started_at // empty] | sort | first // empty' <<<"$runs")"
age_minutes=0
if [[ -n "$started" ]]; then
  age_minutes=$(( ( $(date -u +%s) - $(date -u -j -f '%Y-%m-%dT%H:%M:%SZ' "$started" +%s 2>/dev/null || date -u -d "$started" +%s) ) / 60 ))
fi
pending() {
  if (( age_minutes >= timeout_minutes )); then
    echo "owned macOS check: $1; ${age_minutes} min since the commit's first check, over the ${timeout_minutes} min limit; not a pass" >&2
    exit 1
  fi
  echo "owned macOS check: $1; waiting (${age_minutes} of ${timeout_minutes} min)" >&2
  exit 3
}

[[ -n "$newest" ]] || pending "no '${name}' check on ${sha}"
status="$(jq -r .status <<<"$newest")"
conclusion="$(jq -r '.conclusion // "none"' <<<"$newest")"
[[ "$status" == completed ]] || pending "the check is ${status}"
if [[ "$conclusion" != success ]]; then
  echo "owned macOS check: concluded ${conclusion} on ${sha}; not a pass" >&2
  exit 1
fi

# The check run must point at the trusted run: macos-pr-tests.yml from main, dispatched, for this pull request and SHA.
details="$(jq -r '.details_url // ""' <<<"$newest")"
if [[ ! "$details" =~ ^https://github\.com/${repo}/actions/runs/([0-9]+)(/.*)?$ ]]; then
  echo "owned macOS check: the check does not point at a run of ${repo}; not a pass" >&2
  exit 1
fi
run="$(gh api "repos/${repo}/actions/runs/${BASH_REMATCH[1]}")"
expected_title="macOS PR tests for #${pr} at ${sha}"
problem="$(jq -r --arg repo "$repo" --arg path "$workflow_path" --arg title "$expected_title" '
  if .path != $path then "workflow is \(.path), not \($path)"
  elif .event != "workflow_dispatch" then "event is \(.event), not workflow_dispatch"
  elif .head_branch != "main" then "ran on \(.head_branch), not main"
  elif .repository.full_name != $repo then "run belongs to \(.repository.full_name)"
  elif .display_title != $title then "run is titled \"\(.display_title)\", not \"\($title)\""
  elif .status != "completed" or .conclusion != "success" then "run is \(.status)/\(.conclusion)"
  else "" end' <<<"$run")"
if [[ -n "$problem" ]]; then
  echo "owned macOS check: the run behind the check is not the trusted one (${problem}); not a pass" >&2
  exit 1
fi
echo "owned macOS check: success on ${sha} from a main-dispatched ${workflow_path} run"
