#!/usr/bin/env bash
# Download the newest unexpired artifact named `<prefix><main commit SHA>`
# whose producing run was headed by main, then unzip it into a destination.
#
# The binary-size signal has two cross-run readers — main's last release
# measurement and main's last debug measurement — and both ask the same
# question: "what did main most recently record?". Both writers name their
# artifact after the commit they measured, so the answer is an exact-name
# lookup for each of main's newest commits, newest first. Neither reader needs
# a second storage mechanism, a committed file, or a cache writer.
#
# The lookup never lists the repository's artifacts unfiltered. That listing
# counts every artifact the repository holds, and on this repository GitHub
# times it out: it answers HTTP 500 with an empty body after about eight
# seconds at any page size, which gh reports as "unexpected end of JSON input".
# A `name=` read is indexed and answers in a fraction of a second.
#
# Absence is reported, never silently treated as a zero: when nothing matches,
# this exits 0 having written nothing, and prints why. The caller must render
# that as "not recorded" rather than as "no growth". A read that fails, or that
# returns something other than the expected JSON object, fails the job instead
# of reading as absence.
set -euo pipefail

if [[ $# -ne 2 ]]; then
  echo "usage: $0 <artifact-name-prefix> <destination-directory>" >&2
  exit 2
fi

prefix=$1
destination=$2

: "${GH_REPO:?GH_REPO must be set}"
: "${GH_TOKEN:?GH_TOKEN must be set}"

# main records a debug measurement on every Rust push and a release
# measurement on every push, so the newest few commits almost always hold one.
# The horizon bounds the worst case at one commit-list read plus this many
# exact-name reads.
readonly MAX_COMMITS=30

mkdir -p "$destination"

# Every request here is a read, so a failed one is safe to repeat. A response
# that is not the JSON object the caller asked for counts as a failed read:
# an empty or truncated body must never parse as "no artifacts".
readonly ATTEMPTS=3
gh_read_to() {
  local kind=$1 output=$2 path=$3 attempt
  for ((attempt = 1; attempt <= ATTEMPTS; attempt++)); do
    if gh api "$path" > "$output"; then
      case "$kind" in
        json-array) jq -e 'type == "array"' "$output" > /dev/null 2>&1 && return 0 ;;
        json-artifacts) jq -e '.artifacts | type == "array"' "$output" > /dev/null 2>&1 && return 0 ;;
        bytes) return 0 ;;
      esac
      echo "GitHub API read of ${path} returned an unexpected body." >&2
    fi
    if ((attempt < ATTEMPTS)); then
      echo "GitHub API read of ${path} failed (attempt ${attempt} of ${ATTEMPTS}); retrying." >&2
      sleep "${FETCH_ARTIFACT_RETRY_SECONDS:-5}"
    fi
  done
  echo "::error::GitHub API read of ${path} failed ${ATTEMPTS} times." >&2
  return 1
}
read_file="$(mktemp)"
trap 'rm -f "$read_file"' EXIT

gh_read_to json-array "$read_file" "repos/${GH_REPO}/commits?sha=main&per_page=${MAX_COMMITS}"
main_shas=()
while IFS= read -r sha; do
  main_shas+=("$sha")
done < <(jq -r '.[].sha' "$read_file")
if ((${#main_shas[@]} == 0)); then
  echo "::error::GitHub listed no commits on main." >&2
  exit 1
fi

artifact_id=""
for sha in "${main_shas[@]}"; do
  gh_read_to json-artifacts "$read_file" \
    "repos/${GH_REPO}/actions/artifacts?name=${prefix}${sha}&per_page=10"
  artifact_id="$(jq -r \
    '[.artifacts[]
      | select(.expired == false)
      | select(.workflow_run.head_branch == "main")]
      | .[0].id // empty' "$read_file")"
  if [[ -n "$artifact_id" ]]; then
    break
  fi
done

if [[ -z "$artifact_id" ]]; then
  echo "::notice::No unexpired main artifact matching '${prefix}' was found for the newest ${#main_shas[@]} main commits."
  exit 0
fi

zip_path="${destination}/artifact.zip"
gh_read_to bytes "$zip_path" "repos/${GH_REPO}/actions/artifacts/${artifact_id}/zip"

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
