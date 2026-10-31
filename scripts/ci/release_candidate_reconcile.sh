#!/usr/bin/env bash
# Keep one release candidate in flight while main owes a stable release.
#
# Promotion publishes from any successful main candidate of main's declared
# stable version, so a failed candidate strands nothing: a newer main commit
# can still qualify. Promotion's planner runs this after every completed main
# build that left nothing to promote. It reads the measured state and does one
# of four things:
#
#   - nothing owed (development version, or already published): exit 0;
#   - a candidate build or a candidate's promotion is in flight: report it, exit 0;
#   - main's newest commit has had no source candidate: dispatch one, exit 0;
#   - main's newest commit already had one and none succeeded: once the
#     version commit is older than the allowance, open one tracking issue and
#     fail, because only a change on main can produce a new candidate.
#
# Every read that fails is a failure, never "nothing in flight".
#
# Inputs (environment):
#   GITHUB_REPOSITORY  owner/name
#   MAIN_SHA           main commit to judge (its version is main's declaration)
#   SOURCE_DIRECTORY   checkout with main's first-parent history
#   SELF_RUN_ID        this promotion run, excluded from the in-flight census
#   RELEASE_OVERDUE_HOURS  allowance after the version commit (default 4)
#   NOW_EPOCH          clock override for tests
set -euo pipefail

script_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=scripts/lib/release_version.sh
source "$script_dir/../lib/release_version.sh"
# shellcheck source=scripts/lib/release_candidate_run.sh
source "$script_dir/../lib/release_candidate_run.sh"

repository="${GITHUB_REPOSITORY:?repository required}"
main_sha="${MAIN_SHA:?main commit required}"
source_directory="${SOURCE_DIRECTORY:-.}"
self_run="${SELF_RUN_ID:-0}"
allowance_hours="${RELEASE_OVERDUE_HOURS:-4}"
now="${NOW_EPOCH:-$(date -u +%s)}"
[[ "$main_sha" =~ ^[0-9a-f]{40}$ && "$allowance_hours" =~ ^[1-9][0-9]*$ && "$now" =~ ^[1-9][0-9]*$ ]] || {
  echo '::error::Invalid main commit, overdue allowance or clock.' >&2
  exit 1
}
git -C "$source_directory" cat-file -e "$main_sha^{commit}" 2>/dev/null ||
  git -C "$source_directory" fetch --quiet --no-tags origin "$main_sha"

version="$(git -C "$source_directory" show "$main_sha:Cargo.toml" | release_workspace_version)"
owed=0
release_version_is_owed "$repository" "$version" || owed=$?
case "$owed" in
  0) ;;
  1) exit 0 ;;
  *) echo "::error::Cannot read published releases; v$version publication is unproven." >&2; exit 1 ;;
esac
tag="v$version"
release_commit="$(cd "$source_directory" && release_recovery_transition "$main_sha" "$version")" || {
  echo "::error::$tag is unpublished and its version commit cannot be found from $main_sha." >&2
  exit 1
}

builds="repos/$repository/actions/workflows/build-release-binaries.yml/runs"
census_rows() {
  local pages="$1" mode="$2"
  jq -r --arg mode "$mode" --arg release "$release_commit" --arg main "$main_sha" '
    def valid_run:
      (.id | type) == "number" and .id > 0 and .id == (.id | floor) and
      (.event | type) == "string" and (.event | length) > 0 and
      (.status | IN("queued", "in_progress", "waiting", "requested", "pending", "completed")) and
      (.head_branch | type) == "string" and (.head_branch | length) > 0 and
      (.head_sha | type) == "string" and (.head_sha | test("^[0-9a-f]{40}$")) and
      (.display_title == null or (.display_title | type) == "string");
    if type != "array" or length == 0 or
       (all(.[]; (.workflow_runs | type) == "array") | not) or
       (.[0].total_count | type) != "number" or
       ([.[].workflow_runs | length] | add) != .[0].total_count or
       (all(.[] .workflow_runs[]; valid_run) | not)
    then error("incomplete workflow run census")
    else [.[] .workflow_runs[] |
      select(.head_branch == "main") |
      select(if $mode == "in_flight" then
        .status != "completed" and
        (.event == "schedule" or .event == "workflow_dispatch" or
          (.event == "push" and .head_sha == $release))
      else .head_sha == $main and (.event == "schedule" or .event == "workflow_dispatch") end) |
      [.id, .event, (.display_title // "")] | @tsv] | join("\n") end
  ' <<< "$pages"
}
# A candidate build in flight: any main source candidate, or the version
# commit's own push.
build_rows=""
for state in queued in_progress waiting requested pending; do
  build_pages="$(gh api "$builds?branch=main&status=$state&per_page=100" --paginate --slurp)" || {
    echo "::error::Cannot read $state candidate builds on main; refusing to treat $tag as idle." >&2
    exit 1
  }
  rows="$(census_rows "$build_pages" in_flight)" || {
    echo "::error::Invalid $state candidate build census; refusing to treat $tag as idle." >&2
    exit 1
  }
  [[ -z "$rows" ]] || build_rows="${build_rows:+$build_rows$'\n'}$rows"
done
candidate_ids() {
  local rows="$1" run event title result ids=""
  while IFS=$'\t' read -r run event title; do
    [[ -n "$run" ]] || continue
    if [[ "$event" == workflow_dispatch ]]; then
      result=0
      release_dispatch_is_candidate "$repository" "$run" "$title" || result=$?
      case "$result" in
        0) ;;
        1) continue ;;
        *) echo "::error::Cannot establish the purpose of dispatch $run; refusing to treat $tag as idle." >&2; return 1 ;;
      esac
    fi
    ids="${ids:+$ids,}$run"
  done <<< "$rows"
  printf '%s\n' "$ids"
}
in_flight="$(candidate_ids "$build_rows")" || exit 1
# The success-only selector remains separate from authenticated archive
# recovery, whose original consumer may have failed.
promotion_state="$(node "$script_dir/../lib/release-candidate-state.mjs" promotions "$repository" "$self_run")" || {
  echo "::error::Promotion state is unmeasured; refusing to treat $tag as idle." >&2
  exit 1
}
promoting="$(jq -er 'select(.schema == "harn.release.active_promotions.v1") | .promoting | join(",")' <<< "$promotion_state")" || exit 1
echo "::notice::Promotion census: $(jq -c '{observed,promoting,recoveries,pending_jobs,bad_jobs}' <<< "$promotion_state")"

age_hours=$(( (now - $(git -C "$source_directory" show -s --format=%ct "$release_commit")) / 3600 ))
summary="$tag owed since $release_commit (${age_hours}h)"
if [[ -n "$in_flight" || -n "$promoting" ]]; then
  echo "::notice::$summary; candidate builds in flight: [${in_flight}]; candidate promotions in flight: [${promoting}]."
  exit 0
fi

# One source candidate per main commit. A failed one needs a new commit (or a
# rerun of its failed jobs), never an automatic loop over the same source.
attempt_pages="$(gh api "$builds?head_sha=$main_sha&per_page=100" --paginate --slurp)" || {
  echo "::error::Cannot read candidate attempts at $main_sha." >&2
  exit 1
}
attempt_rows="$(census_rows "$attempt_pages" attempted)" || {
  echo "::error::Cannot read candidate attempts at $main_sha." >&2
  exit 1
}
attempted="$(candidate_ids "$attempt_rows")" || exit 1
if [[ -z "$attempted" ]]; then
  gh workflow run build-release-binaries.yml --repo "$repository" --ref main -f source_candidate=true
  echo "::notice::$summary; no candidate in flight, so a source candidate of main ($main_sha or newer) was requested."
  exit 0
fi

if (( age_hours < allowance_hours )); then
  echo "::notice::$summary; main's newest commit $main_sha already had candidate run(s) [$attempted]; waiting for a new main commit (allowance ${allowance_hours}h)."
  exit 0
fi
title="[Release] $tag is unpublished on main"
message="$tag has been main's version since $release_commit (${age_hours}h ago) and no candidate of it has qualified. Main's newest commit $main_sha already had candidate run(s) $attempted, and nothing is in flight."
echo "::error::$message"
existing="$(gh issue list --repo "$repository" --state open --search "\"$title\" in:title" \
  --json number,title --jq "[.[] | select(.title == \"$title\") | .number][0] // empty")" || {
  echo '::error::Cannot read open issues; the overdue release is reported here only.' >&2
  exit 1
}
if [[ -z "$existing" ]]; then
  next="Read the failed jobs of run(s) $attempted in build-release-binaries.yml. For a source defect, land the fix on main: the next main push requests a fresh candidate and promotion publishes the first one that qualifies. For an infrastructure failure, rerun the failed jobs of that run; its success promotes automatically."
  gh issue create --repo "$repository" --title "[Release] $tag is unpublished on main" \
    --body "$message"$'\n\n'"$next"$'\n\n'"Detected by promote-release.yml. Close this issue once $tag is published." >/dev/null
fi
exit 1
