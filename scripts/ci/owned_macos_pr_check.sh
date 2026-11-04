#!/usr/bin/env bash
# Decide whether a pull request's owned-runner macOS check passed for one exact head SHA.
#
# usage: owned_macos_pr_check.sh <owner/repo> <head-sha>
#
# Reads the check runs recorded on that commit and passes (exit 0) only when the
# newest run named "macOS workspace tests (owned)" is completed with a success
# conclusion. Anything else is a failure: no such check, one still running, a
# failure, or a success that belongs to a different commit. A missing check is
# never read as success, and a success on an earlier head does not carry over to a
# newer one because the lookup is by commit.
set -euo pipefail

repo="${1:?usage: owned_macos_pr_check.sh <owner/repo> <head-sha>}"
sha="${2:?usage: owned_macos_pr_check.sh <owner/repo> <head-sha>}"
name="macOS workspace tests (owned)"

[[ "$sha" =~ ^[0-9a-f]{40}$ ]] || { echo "owned macOS check: '$sha' is not a full commit SHA" >&2; exit 2; }

runs="$(gh api "repos/${repo}/commits/${sha}/check-runs?per_page=100" --paginate --jq '.check_runs[]' | jq -s .)"
newest="$(jq -c --arg name "$name" --arg sha "$sha" \
  '[.[] | select(.name == $name and .head_sha == $sha)] | sort_by(.id) | last // empty' <<<"$runs")"

if [[ -z "$newest" ]]; then
  echo "owned macOS check: no '${name}' check on ${sha}; not a pass" >&2
  exit 1
fi
status="$(jq -r .status <<<"$newest")"
conclusion="$(jq -r '.conclusion // "none"' <<<"$newest")"
if [[ "$status" == completed && "$conclusion" == success ]]; then
  echo "owned macOS check: success on ${sha}"
  exit 0
fi
echo "owned macOS check: ${status}/${conclusion} on ${sha}; not a pass" >&2
exit 1
