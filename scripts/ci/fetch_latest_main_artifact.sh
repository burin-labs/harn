#!/usr/bin/env bash
# Download the newest unexpired artifact whose name starts with a prefix and
# whose producing run was headed by main, then unzip it into a destination.
#
# The binary-size signal has two cross-run readers — main's last release
# measurement and main's last debug measurement — and both ask the same
# question: "what did main most recently record?". GitHub answers it from the
# artifact list, which carries the producing run's head branch, so neither
# reader needs a second storage mechanism, a committed file, or a cache writer.
#
# Absence is reported, never silently treated as a zero: when nothing matches,
# this exits 0 having written nothing, and prints why. The caller must render
# that as "not recorded" rather than as "no growth".
set -euo pipefail

if [[ $# -ne 2 ]]; then
  echo "usage: $0 <artifact-name-prefix> <destination-directory>" >&2
  exit 2
fi

prefix=$1
destination=$2

: "${GH_REPO:?GH_REPO must be set}"
: "${GH_TOKEN:?GH_TOKEN must be set}"

mkdir -p "$destination"

artifact_id="$(
  gh api --paginate \
    "repos/${GH_REPO}/actions/artifacts?per_page=100" \
    --jq "[.artifacts[]
          | select(.expired == false)
          | select(.name | startswith(\"${prefix}\"))
          | select(.workflow_run.head_branch == \"main\")]
          | sort_by(.created_at) | reverse | .[0].id // empty"
)"

if [[ -z "$artifact_id" ]]; then
  echo "::notice::No unexpired main artifact matching '${prefix}' was found."
  exit 0
fi

zip_path="${destination}/artifact.zip"
gh api "repos/${GH_REPO}/actions/artifacts/${artifact_id}/zip" > "$zip_path"
unzip -o -q "$zip_path" -d "$destination"
rm -f "$zip_path"
echo "::notice::Restored main artifact ${artifact_id} matching '${prefix}'."
