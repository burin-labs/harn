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
#
# Paging is an explicit bounded loop rather than `gh api --paginate`. With
# `--jq`, `--paginate` applies the filter to each page and prints one result per
# page, so an aggregating expression over a paginated read emits several lines
# and parses as none. `--slurp` would fix that and is rejected alongside `--jq`.
set -euo pipefail

if [[ $# -ne 2 ]]; then
  echo "usage: $0 <artifact-name-prefix> <destination-directory>" >&2
  exit 2
fi

prefix=$1
destination=$2

: "${GH_REPO:?GH_REPO must be set}"
: "${GH_TOKEN:?GH_TOKEN must be set}"

# The artifact list is newest first, so the first match is the newest match.
# Ten pages of a hundred bounds the read while covering far more history than
# either reader needs: main records a measurement on every Rust push.
readonly MAX_PAGES=10
readonly PER_PAGE=100

mkdir -p "$destination"

artifact_id=""
for ((page = 1; page <= MAX_PAGES; page++)); do
  # One request per page. The count and the match both come from it, so the
  # end-of-list test cannot disagree with what was searched.
  page_json="$(gh api "repos/${GH_REPO}/actions/artifacts?per_page=${PER_PAGE}&page=${page}")"
  artifact_id="$(jq -r --arg prefix "$prefix" \
    '[.artifacts[]
      | select(.expired == false)
      | select(.workflow_run.head_branch == "main")
      | select(.name | startswith($prefix))]
      | .[0].id // empty' <<<"$page_json")"
  if [[ -n "$artifact_id" ]]; then
    break
  fi
  # A short page is the end of the list, not a reason to keep asking.
  if [[ "$(jq -r '.artifacts | length' <<<"$page_json")" -lt "$PER_PAGE" ]]; then
    break
  fi
done

if [[ -z "$artifact_id" ]]; then
  echo "::notice::No unexpired main artifact matching '${prefix}' was found in the newest $((MAX_PAGES * PER_PAGE))."
  exit 0
fi

zip_path="${destination}/artifact.zip"
gh api "repos/${GH_REPO}/actions/artifacts/${artifact_id}/zip" > "$zip_path"

# The download endpoint returns the stored bytes, which are a zip only for an
# artifact that was archived on upload. An artifact published with
# `archive: false` comes back as the file itself, and unzipping it would leave
# an empty destination that reads exactly like "nothing was recorded". Assert
# the shape instead of inferring it from an empty directory.
if [[ "$(head -c 2 "$zip_path")" != "PK" ]]; then
  echo "::error::Artifact ${artifact_id} did not download as a zip; it was probably uploaded with archive:false, which this reader does not handle." >&2
  exit 1
fi

unzip -o -q "$zip_path" -d "$destination"
rm -f "$zip_path"
echo "::notice::Restored main artifact ${artifact_id} matching '${prefix}'."
