#!/usr/bin/env bash
# Open the `Release vX.Y.Z` pull request from main.
#
# The decision runs first and needs no Harn build:
#   1. main must declare X.Y.Z-dev. Any other workspace version means the
#      release for it already merged and the development bump has not landed,
#      so there is nothing to release yet.
#   2. An open pull request titled `Release vX.Y.Z`, or from release/vX.Y.Z,
#      stops the opener, which names it.
#   3. Without unreleased changelog fragments there is nothing to release.
# Otherwise the opener branches release/vX.Y.Z, runs
# `release_ship.sh --prepare --materialize-candidate` (which folds the
# fragments, bumps the version to X.Y.Z, and regenerates derived files),
# publishes that tree as one GitHub-signed commit, and opens the pull request.
#
# Usage: open_release_pr.sh [--plan]
#   --plan  decide only. Writes action=open|existing|none to $GITHUB_OUTPUT.
# Without --plan the decision is taken again (it may be minutes newer than the
# plan) and action=opened|existing|none is written, with version and pr_url.
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

mode=open
case "${1:-}" in
  --plan) mode=plan ;;
  "") ;;
  *)
    echo "usage: open_release_pr.sh [--plan]" >&2
    exit 2
    ;;
esac

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

# A failed lookup must not read as "no pull request": opening a second release
# pull request for one version is the failure this check exists to prevent.
if ! existing="$(gh pr list --state open --base main --limit 1000 \
  --json url,title,headRefName \
  --jq "[.[] | select(.title == \"$title\" or .headRefName == \"$branch\")] | .[0].url // empty")"; then
  echo "error: could not list open pull requests; refusing to open $title on unproved state" >&2
  exit 1
fi
if [[ -n "$existing" ]]; then
  echo "::notice title=Release pull request already open::$title is open: $existing"
  emit action=existing "version=$version" "pr_url=$existing"
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
  emit action=open "version=$version" pr_url=
  exit 0
fi

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

HARN_BRANCH_COMMIT_TOKEN="$GH_TOKEN" \
  HARN_BRANCH_COMMIT_BRANCH="$branch" \
  HARN_BRANCH_COMMIT_BASE_OID="$base_oid" \
  HARN_BRANCH_COMMIT_HEADLINE="$title" \
  "$harn_bin" run --no-sandbox "$script_root/scripts/bump-driver/publish_branch_commit.harn"

body_file="$(mktemp)"
trap 'rm -f "$body_file"' EXIT
cat > "$body_file" <<EOF
Moves the workspace from $current to $version and folds ${#fragments[@]} changelog fragment(s) into the \`## v$version\` section of CHANGELOG.md, deleting them.

When this merges, the push to main builds and checks the release candidate at that commit, and promotion publishes exactly those files. Opened by \`scripts/open_release_pr.sh\` from main at $base_oid.
EOF
pr_url="$(gh pr create --base main --head "$branch" --title "$title" --body-file "$body_file")"
echo "Opened $title: $pr_url"
emit action=opened "version=$version" "pr_url=$pr_url"
