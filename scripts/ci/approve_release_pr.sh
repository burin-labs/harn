#!/usr/bin/env bash
# Approve the release bot's own version-only pull requests.
#
# Two pull requests exist only to move the workspace version: `Release vX.Y.Z`
# (release-attempt/vX.Y.Z/<head>, written by scripts/open_release_pr.sh) and `Start X.Y.Z-dev
# development` (automation/development-X.Y.Z-dev). The bot that opens them
# cannot approve its own pull request, and the automatic reviewer skips
# bot-authored pull requests, so each waited for a person. This script approves
# one only when the diff is provably nothing but that move.
#
# Approval is decided from the pull request's own data, never from code it
# carries:
#   1. Author is the release bot, branch and title agree on one version, the
#      head is in this repository, and every commit is GitHub-verified.
#   2. Every changed file is one of three kinds. CHANGELOG.md gains lines and
#      loses none. A changelog.d fragment is only deleted. Anything else must be
#      edited in place (never added, removed, renamed or mode-changed) and
#      version-only: once the old and new version strings are replaced by one
#      token, the lines removed from the file equal the lines added to it. A new
#      dependency, a changed checksum or a reordered table breaks that equality.
#   3. No review thread is open and no review requests changes.
#
# Any other outcome exits non-zero without approving, naming the reason.
#
# Usage: approve_release_pr.sh <pull-request-number>
# Environment: GH_TOKEN (pull-requests: write), GITHUB_REPOSITORY,
# RELEASE_BOT_LOGIN (default harn-release-bot[bot]), APPROVE_DRY_RUN=1 to decide
# without approving.
set -euo pipefail

pr=${1:?usage: approve_release_pr.sh <pull-request-number>}
[[ "$pr" =~ ^[0-9]+$ ]] || { echo "pull request number must be numeric: $pr" >&2; exit 2; }
repo=${GITHUB_REPOSITORY:?GITHUB_REPOSITORY is required}
bot=${RELEASE_BOT_LOGIN:-harn-release-bot[bot]}

refuse() {
  echo "::error title=Release approval refused::#$pr: $1" >&2
  echo "Not approving #$pr: $1" >&2
  exit 1
}

meta=$(gh api "repos/$repo/pulls/$pr") || refuse "the pull request could not be read"
read -r state draft base author author_type head_repo head_ref head_sha < <(
  jq -r '[.state, .draft, .base.ref, .user.login, .user.type,
          (.head.repo.full_name // "-"), .head.ref, .head.sha] | @tsv' <<< "$meta"
) || refuse "the pull request metadata was malformed"
title=$(jq -r '.title' <<< "$meta")

[[ "$state" == open && "$draft" == false ]] || refuse "it is not an open, ready pull request"
[[ "$base" == main ]] || refuse "it does not target main"
[[ "$author" == "$bot" && "$author_type" == Bot ]] || refuse "its author is $author, not $bot"
[[ "$head_repo" == "$repo" ]] || refuse "its head is not in $repo"
[[ "$head_sha" =~ ^[0-9a-f]{40}$ ]] || refuse "its head SHA is malformed"

# The branch names the version; the title must say the same thing. Docs and
# manifests mention the new version, the patch before it, or the latest
# published release (which differs from the patch before it after a retired,
# never-published version), in stable or -dev form. Those are the only strings
# a version-only change may rewrite.
if [[ "$head_ref" =~ ^release-attempt/v([0-9]+)\.([0-9]+)\.([0-9]+)/([0-9a-f]{40})$ ]]; then
  major=${BASH_REMATCH[1]} minor=${BASH_REMATCH[2]} patch=${BASH_REMATCH[3]}
  [[ "${BASH_REMATCH[4]}" == "$head_sha" ]] || refuse "immutable attempt ref does not name its head"
  [[ "$title" == "Release v$major.$minor.$patch" ]] || refuse "title '$title' does not match branch $head_ref"
  kind=release
elif [[ "$head_ref" =~ ^release/v([0-9]+)\.([0-9]+)\.([0-9]+)$ ]]; then
  major=${BASH_REMATCH[1]} minor=${BASH_REMATCH[2]} patch=${BASH_REMATCH[3]}
  [[ "$title" == "Release v$major.$minor.$patch" ]] || refuse "title '$title' does not match branch $head_ref"
  kind=release
elif [[ "$head_ref" =~ ^automation/development-([0-9]+)\.([0-9]+)\.([0-9]+)-dev$ ]]; then
  major=${BASH_REMATCH[1]} minor=${BASH_REMATCH[2]} patch=${BASH_REMATCH[3]}
  [[ "$title" == "[Release] Start $major.$minor.$patch-dev development" ]] || refuse "title '$title' does not match branch $head_ref"
  kind=development
else
  refuse "branch $head_ref is not a release or development-bump branch"
fi
((patch > 0)) || refuse "version $major.$minor.$patch has no previous patch"
new="$major.$minor.$patch"
old="$major.$minor.$((patch - 1))"
# After a retirement the stable files name the published predecessor, which is
# older than the previous patch. An unreadable or malformed tag adds nothing.
published=$(gh api "repos/$repo/releases/latest" --jq '.tag_name' 2> /dev/null || true)
published=${published#v}
[[ "$published" =~ ^[0-9]+\.[0-9]+\.[0-9]+$ && "$published" != "$new" ]] || published=$old

commits=$(gh api --paginate --slurp "repos/$repo/pulls/$pr/commits?per_page=100" | jq 'add // []') \
  || refuse "the commits could not be read"
jq -e 'length > 0 and all(.[]; .commit.verification.verified == true)' <<< "$commits" > /dev/null \
  || refuse "a commit is missing or not GitHub-verified"

files=$(gh api --paginate --slurp "repos/$repo/pulls/$pr/files?per_page=100" | jq 'add // []') \
  || refuse "the changed files could not be read"
count=$(jq 'length' <<< "$files")
((count > 0 && count < 3000)) || refuse "the changed file list is empty or truncated ($count files)"

# Version-only means: with both version strings replaced by one token, a file's
# removed lines are exactly its added lines, in patch order: a reordered pair of
# lines is a behavior change, not a version move. Patch lines starting `---`/`+++`
# are content here, not headers, because the API patch carries only hunks.
version_only() {
  jq -r --arg old "$old" --arg new "$new" --arg published "$published" '
    # Longest first: X.Y.Z-dev contains X.Y.Z.
    def lit($v): "(?<![0-9.])" + ($v | gsub("\\."; "\\.")) + "(?![0-9])";
    def norm: reduce ([$new + "-dev", $old + "-dev", $published + "-dev", $new, $old, $published] | .[]) as $v (.; gsub(lit($v); "@V@"));
    def hunk_lines: split("\n") | map(select(test("^[-+]") ));
    .[]
    | select(.filename != "CHANGELOG.md" and (.filename | test("^changelog\\.d/[^/]+\\.md$") | not))
    | .filename as $f
    | if .status != "modified" or (.previous_filename // null) != null then "\($f): status \(.status) is not an in-place edit"
      elif (.patch // null) == null then "\($f): no textual patch"
      else
        (.patch | hunk_lines) as $l
        | [$l[] | select(startswith("-")) | .[1:] | norm] as $removed
        | [$l[] | select(startswith("+")) | .[1:] | norm] as $added
        | if $removed == $added then empty else "\($f): changes more than the version" end
      end' <<< "$files"
}

# A release folds the parked or fragment-built `## Unreleased` heading into
# `## v<new>`: that one removed line is allowed, and only when the very next
# patch line is its replacement, so the notes under it keep their heading.
problems=$(jq -r --arg new "$new" '
  .[]
  | if .filename == "CHANGELOG.md" then
      # No patch means a truncated or binary diff that cannot be shown to add only lines.
      (if (.patch // null) == null or .status != "modified" then "CHANGELOG.md: no textual patch to check"
       elif (.patch | split("\n")) as $l
            | [$l[] | select(test("^-") and . != "-## Unreleased")] | length > 0
         then "CHANGELOG.md: removes lines"
       elif (.patch | split("\n")) as $l
            | [range(0; $l | length) | select($l[.] == "-## Unreleased") | select($l[. + 1] != "+## v" + $new)]
            | length > 0
         then "CHANGELOG.md: drops the Unreleased heading without renaming it in place to v\($new)"
       else empty end)
    elif (.filename | test("^changelog\\.d/[^/]+\\.md$")) then
      (if .status == "removed" then empty else "\(.filename): not a deletion" end)
    else empty end' <<< "$files")
problems+=$'\n'$(version_only)
if [[ "$kind" == development ]]; then
  # A development bump folds no changelog.
  problems+=$'\n'$(jq -r '.[] | select(.filename == "CHANGELOG.md" or (.filename | startswith("changelog.d/"))) | "\(.filename): a development bump touches no changelog"' <<< "$files")
fi
problems=$(sed '/^$/d' <<< "$problems")
[[ -z "$problems" ]] || refuse "the diff is not version-only: $(head -3 <<< "$problems" | paste -sd ';' -)"

owner=${repo%%/*} name=${repo#*/}
review_state=$(gh api graphql -F owner="$owner" -F name="$name" -F number="$pr" -f query='
  query($owner:String!,$name:String!,$number:Int!){repository(owner:$owner,name:$name){pullRequest(number:$number){
    reviewDecision reviewThreads(first:100){pageInfo{hasNextPage} nodes{isResolved}}
    reviews(states:CHANGES_REQUESTED,first:1){totalCount}}}}' --jq '.data.repository.pullRequest') \
  || refuse "the review state could not be read"
jq -e '.reviewThreads.pageInfo.hasNextPage == false and ([.reviewThreads.nodes[] | select(.isResolved | not)] | length) == 0 and .reviews.totalCount == 0' <<< "$review_state" > /dev/null \
  || refuse "it has an open review thread, more threads than one page shows, or a change request"

echo "#$pr is a version-only $kind pull request ($new, $count files, head $head_sha)"
if [[ "${APPROVE_DRY_RUN:-0}" == 1 ]]; then
  echo "dry run: not approving"
  exit 0
fi

gh api -X POST "repos/$repo/pulls/$pr/reviews" -f commit_id="$head_sha" -f event=APPROVE \
  -f body="Version-only $kind pull request ($new), checked mechanically by scripts/ci/approve_release_pr.sh against head $head_sha." > /dev/null \
  || refuse "the approval could not be posted"
decision=$(gh api graphql -F owner="$owner" -F name="$name" -F number="$pr" -f query='
  query($owner:String!,$name:String!,$number:Int!){repository(owner:$owner,name:$name){pullRequest(number:$number){reviewDecision}}}' \
  --jq '.data.repository.pullRequest.reviewDecision')
echo "approved #$pr at $head_sha; reviewDecision=$decision"
[[ "$decision" == APPROVED ]] || refuse "the approval was posted but reviewDecision is $decision, so it does not count toward the required review"
