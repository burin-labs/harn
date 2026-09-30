#!/usr/bin/env bash
# Open the `Release vX.Y.Z` pull request from main.
#
# The decision runs first and needs no Harn build:
#   1. main must declare X.Y.Z-dev. Any other workspace version means the
#      release for it already merged and the development bump has not landed,
#      so there is nothing to release yet.
#   2. An open pull request titled `Release vX.Y.Z`, or from release/vX.Y.Z,
#      stops the opener, which names it, unless main has gained changelog
#      fragments since that pull request was prepared. Then the opener refolds
#      it in place: it prepares again from current main with the same version
#      and resets release/vX.Y.Z to that one signed commit. The pull request
#      stays open, so a late fix rides the release without a close and reopen.
#   3. Without unreleased changelog fragments there is nothing to release.
# Otherwise the opener branches release/vX.Y.Z, runs
# `release_ship.sh --prepare --materialize-candidate` (which folds the
# fragments, bumps the version to X.Y.Z, and regenerates derived files),
# publishes that tree as one GitHub-signed commit, opens the pull request, and
# arms auto-merge on it at once (scripts/lib/release_auto_merge.sh). The pull
# request still merges only through its required checks and review.
#
# Usage: open_release_pr.sh [--plan] [--refold-only]
#   --plan         decide only. Writes action=open|refold|existing|none to
#                  $GITHUB_OUTPUT.
#   --refold-only  never open a new release pull request; only refold an open
#                  one. A push to main runs this way, so merging a fragment
#                  does not start a release by itself.
# Without --plan the decision is taken again (it may be minutes newer than the
# plan) and action=opened|refolded|existing|none is written, with version and
# pr_url.
#
# Requires GH_TOKEN. Opening also requires HARN_BIN (the release-source Harn
# executable) and GITHUB_REPOSITORY.
set -euo pipefail

script_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
root="${HARN_RELEASE_ROOT:-$script_root}"
release_ship="${HARN_RELEASE_SHIP_SCRIPT:-$script_root/scripts/release_ship.sh}"
cd "$root"
source "$script_root/scripts/lib/release_version.sh"
source "$script_root/scripts/lib/release_tree_guard.sh"
source "$script_root/scripts/lib/release_auto_merge.sh"

mode=open
refold_only=0
for arg in "$@"; do
  case "$arg" in
    --plan) mode=plan ;;
    --refold-only) refold_only=1 ;;
    *)
      echo "usage: open_release_pr.sh [--plan] [--refold-only]" >&2
      exit 2
      ;;
  esac
done

emit() {
  if [[ -n "${GITHUB_OUTPUT:-}" ]]; then
    printf '%s\n' "$@" >> "$GITHUB_OUTPUT"
  fi
}

if [[ -z "${GH_TOKEN:-}" ]]; then
  echo "error: GH_TOKEN is required" >&2
  exit 1
fi

current="$(release_workspace_version < Cargo.toml)"
development_suffix="-$HARN_RELEASE_DEVELOPMENT_PRERELEASE"
version="${current%"$development_suffix"}"
if [[ "$version" == "$current" ]] \
  || ! [[ "$version" =~ ^(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)$ ]]; then
  echo "::notice title=Nothing to release::main declares ${current:-no workspace version}, not an X.Y.Z$development_suffix development version. The next release starts once the development bump lands."
  emit action=none version= pr_url=
  exit 0
fi
title="Release v$version"
branch="release/v$version"

# Changelog fragment paths in one commit, filtered as unfolded_fragment_paths
# filters the working tree.
fragment_paths_at() {
  local fragment name
  git ls-tree --name-only "$1" changelog.d/ | while IFS= read -r fragment; do
    name="${fragment##*/}"
    [[ "$name" == README* || "$name" == _* ]] && continue
    [[ "$name" =~ \.(breaking|added|changed|deprecated|removed|fixed|security)\.md$ ]] || continue
    printf '%s\n' "$fragment"
  done
}

# Find the open release pull request for this version and set existing_url and
# existing_branch (both empty when none is open). A failed lookup must not read
# as "no pull request": opening a second release pull request for one version
# is the failure this check exists to prevent.
read_release_pr() {
  local found
  if ! found="$(gh pr list --state open --base main --limit 1000 \
    --json url,title,headRefName \
    --jq "[.[] | select(.title == \"$title\" or .headRefName == \"$branch\")] | .[0] // empty | \"\(.url) \(.headRefName)\"")"; then
    echo "error: could not list open pull requests; refusing to open $title on unproved state" >&2
    exit 1
  fi
  existing_url=""
  existing_branch=""
  if [[ -n "$found" ]]; then
    existing_url="${found%% *}"
    existing_branch="${found#* }"
  fi
}

# Stop, naming it, when the open release pull request already folds every
# fragment on main, or is not on the branch this opener owns. Otherwise it is
# due a refold: name the fragments it is missing and return.
stop_unless_refold_due() {
  if [[ "$existing_branch" != "$branch" ]]; then
    echo "::notice title=Release pull request already open::$title is open: $existing_url"
    emit action=existing "version=$version" "pr_url=$existing_url"
    exit 0
  fi
  # The release branch is one commit on the main commit it was prepared from.
  local release_base
  if ! git fetch --quiet --depth=2 origin "refs/heads/$branch" \
    || ! release_base="$(git rev-parse --verify --quiet 'FETCH_HEAD^')"; then
    echo "error: could not read the base of $branch; refusing to refold $title on unproved state" >&2
    exit 1
  fi
  local missing=()
  local fragment
  while IFS= read -r fragment; do
    [[ -n "$fragment" ]] && missing+=("$fragment")
  done < <(comm -23 <(unfolded_fragment_paths | sort) <(fragment_paths_at "$release_base" | sort))
  if (( ${#missing[@]} == 0 )); then
    echo "::notice title=Release pull request already open::$title is open and folds every fragment on main: $existing_url"
    emit action=existing "version=$version" "pr_url=$existing_url"
    exit 0
  fi
  echo "$title ($existing_url) was prepared from $release_base and misses ${#missing[@]} fragment(s) now on main:"
  printf '  - %s\n' "${missing[@]}"
}

read_release_pr
if [[ -n "$existing_url" ]]; then
  stop_unless_refold_due
elif (( refold_only )); then
  echo "::notice title=Nothing to refold::no $title pull request is open, and this run only refolds one."
  emit action=none "version=$version" pr_url=
  exit 0
fi

fragments=()
while IFS= read -r fragment; do
  [[ -n "$fragment" ]] && fragments+=("$fragment")
done < <(unfolded_fragment_paths)
if (( ${#fragments[@]} == 0 )); then
  echo "::notice title=Nothing to release::main has no unreleased changelog fragments, so there is no $title to open."
  emit action=none "version=$version" pr_url=
  exit 0
fi
echo "$title: ${#fragments[@]} unreleased changelog fragment(s) on main"
printf '  - %s\n' "${fragments[@]}"

if [[ "$mode" == plan ]]; then
  if [[ -n "$existing_url" ]]; then
    emit action=refold "version=$version" "pr_url=$existing_url"
  else
    emit action=open "version=$version" pr_url=
  fi
  exit 0
fi
refolding_url="$existing_url"

harn_bin="${HARN_BIN:-}"
if [[ -z "$harn_bin" || ! -x "$harn_bin" ]]; then
  echo "error: HARN_BIN must name the already-built release-source Harn executable" >&2
  exit 1
fi
if [[ -z "${GITHUB_REPOSITORY:-}" ]]; then
  echo "error: GITHUB_REPOSITORY is required" >&2
  exit 1
fi
if [[ -n "$(git status --porcelain --untracked-files=normal)" ]]; then
  echo "error: opening a release pull request requires a clean checkout of main" >&2
  exit 1
fi

base_oid="$(git rev-parse HEAD)"
git switch --quiet -c "$branch"
HARN_BIN="$harn_bin" "$release_ship" --prepare --materialize-candidate --bump patch

actual="$(release_workspace_version < Cargo.toml)"
if [[ "$actual" != "$version" ]]; then
  echo "error: expected workspace version $version after prepare, got $actual" >&2
  exit 1
fi

# Decide again against main itself, just before publishing. The checkout is as
# old as this job, and preparing takes minutes: if this version's release pull
# request merged meanwhile, the checkout still reads $current and the open-list
# above no longer shows the merged pull request, so without this re-read the
# run would open a second one for a version already on main. An unreadable main
# refuses rather than proceeding on unproved state.
if ! git fetch --quiet origin main; then
  echo "error: could not fetch origin/main to re-check $title; refusing to open it on unproved state" >&2
  exit 1
fi
main_version="$(git show FETCH_HEAD:Cargo.toml | release_workspace_version)"
if [[ "$main_version" != "$current" ]]; then
  echo "::notice title=Nothing to release::main moved from $current to ${main_version:-no workspace version} while this run prepared $title, so it is no longer due."
  emit action=none "version=$version" pr_url=
  exit 0
fi
# The pull request may have merged, closed, or opened while this run prepared.
read_release_pr
if [[ "$existing_url" != "$refolding_url" ]]; then
  if [[ -n "$existing_url" ]]; then
    echo "::notice title=Release pull request already open::$title is open: $existing_url"
    emit action=existing "version=$version" "pr_url=$existing_url"
  else
    echo "::notice title=Nothing to refold::$title closed while this run prepared it."
    emit action=none "version=$version" pr_url=
  fi
  exit 0
fi

# Opening creates the branch; refolding resets it to this one commit on the new
# base. Either way the branch carries exactly one release commit.
HARN_BRANCH_COMMIT_TOKEN="$GH_TOKEN" \
  HARN_BRANCH_COMMIT_BRANCH="$branch" \
  HARN_BRANCH_COMMIT_BASE_OID="$base_oid" \
  HARN_BRANCH_COMMIT_HEADLINE="$title" \
  "$harn_bin" run --no-sandbox "$script_root/scripts/bump-driver/publish_branch_commit.harn"

body_file="$(mktemp)"
trap 'rm -f "$body_file"' EXIT
cat > "$body_file" <<EOF
Moves the workspace from $current to $version and folds ${#fragments[@]} changelog fragment(s) into the \`## v$version\` section of CHANGELOG.md, deleting them.

When this merges, the push to main builds and checks the release candidate at that commit, and promotion publishes exactly those files. Prepared by \`scripts/open_release_pr.sh\` from main at $base_oid.
EOF
if [[ -n "$refolding_url" ]]; then
  gh pr edit "$refolding_url" --body-file "$body_file" >/dev/null
  echo "Refolded $title onto main at $base_oid: $refolding_url"
  emit "version=$version" "pr_url=$refolding_url"
  release_arm_auto_merge "$refolding_url"
  emit action=refolded
  exit 0
fi
pr_url="$(gh pr create --base main --head "$branch" --title "Release v$version" --body-file "$body_file")"
echo "Opened $title: $pr_url"
# Arm now, before the checks settle. The URL is emitted first so a failed arm
# still names the pull request it left unarmed.
emit "version=$version" "pr_url=$pr_url"
release_arm_auto_merge "$pr_url"
emit action=opened
