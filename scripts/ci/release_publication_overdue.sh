#!/usr/bin/env bash
# Fail loudly when main declares a stable version that was never published.
#
# Promotion is the only path that publishes, and it starts only from a
# successful candidate. When the candidate fails, or its push run fails before
# promotion starts, every later main push reports "nothing to promote" and
# main sits on an unpublished version with nothing red anywhere. Promotion's
# planner calls this on that path: once the version commit is older than the
# allowance, it opens one tracking issue and fails the run.
#
# Inputs (environment):
#   GITHUB_REPOSITORY  owner/name
#   HEAD_SHA           main commit the planner is judging
#   VERSION            workspace version at HEAD_SHA
#   SOURCE_DIRECTORY   checkout of HEAD_SHA with full first-parent history
#   RELEASE_OVERDUE_HOURS  allowance after the version commit (default 4)
#   NOW_EPOCH          clock override for tests
set -euo pipefail

script_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=scripts/lib/release_version.sh
source "$script_dir/../lib/release_version.sh"

repository="${GITHUB_REPOSITORY:?repository required}"
head_sha="${HEAD_SHA:?head commit required}"
version="${VERSION:?version required}"
source_directory="${SOURCE_DIRECTORY:-.}"
allowance_hours="${RELEASE_OVERDUE_HOURS:-4}"
now="${NOW_EPOCH:-$(date -u +%s)}"
[[ "$allowance_hours" =~ ^[1-9][0-9]*$ && "$now" =~ ^[1-9][0-9]*$ ]] || {
  echo '::error::Invalid overdue allowance or clock.' >&2
  exit 1
}

# Development and prerelease versions owe no publication.
if ! release_version_is_canonical "$version" || release_version_is_prerelease "$version"; then
  exit 0
fi
tag="v$version"
published="$(gh api "repos/$repository/releases?per_page=50" \
  --jq "[.[] | select(.draft == false) | .tag_name] | index(\"$tag\") != null")" || {
  echo "::error::Cannot read published releases; $tag publication is unproven." >&2
  exit 1
}
[[ "$published" == true ]] && exit 0
[[ "$published" == false ]] || {
  echo "::error::Unexpected release read '$published'; $tag publication is unproven." >&2
  exit 1
}

release_commit="$(cd "$source_directory" && release_recovery_transition "$head_sha" "$version")" || {
  echo "::error::$tag is unpublished and its version commit cannot be found from $head_sha." >&2
  exit 1
}
committed="$(git -C "$source_directory" show -s --format=%ct "$release_commit")"
age_hours=$(( (now - committed) / 3600 ))
if (( now - committed < allowance_hours * 3600 )); then
  echo "::notice::$tag is unpublished; its version commit $release_commit landed ${age_hours}h ago (allowance ${allowance_hours}h)."
  exit 0
fi

title="[Release] $tag is unpublished on main"
message="$tag has been main's version since $release_commit (${age_hours}h ago) and no release was published. Automatic promotion only follows a successful candidate; read the failed candidate or push run of build-release-binaries.yml at that commit, then recover per docs/src/maintainer-release.md#recover-a-failed-run."
echo "::error::$message"
existing="$(gh issue list --repo "$repository" --state open --search "\"$title\" in:title" \
  --json number,title --jq "[.[] | select(.title == \"$title\") | .number][0] // empty")" || {
  echo '::error::Cannot read open issues; the overdue release is reported here only.' >&2
  exit 1
}
if [[ -z "$existing" ]]; then
  gh issue create --repo "$repository" --title "[Release] $tag is unpublished on main" \
    --body "$message"$'\n\n'"Detected by promote-release.yml at $head_sha. Close this issue once $tag is published." >/dev/null
fi
exit 1
