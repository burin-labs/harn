#!/usr/bin/env bash
# Open the `Release vX.Y.Z` pull request from green, the newest main commit
# whose full CI suite passed. The pull request still targets main.
#
# The decision runs first and needs no Harn build:
#   1. main must declare X.Y.Z-dev. Any other workspace version means the
#      release for it already merged and the development bump has not landed,
#      so there is nothing to release yet.
#   2. A mergeable release stays frozen at its recorded immutable attempt.
#      Modified folded fragments or release-generated outputs can supersede a
#      conflicted attempt once green contains their current blobs.
#   3. Without unreleased changelog fragments there is nothing to release.
# Otherwise the opener prepares on a local release/vX.Y.Z branch, runs
# `release_ship.sh --prepare --materialize-candidate` (which folds the
# fragments, bumps the version to X.Y.Z, and regenerates derived files),
# publishes that tree as one GitHub-signed commit, opens the pull request, and
# enrolls Smart Ship at once (scripts/lib/release_ship_enrollment.sh). The pull
# request still merges only through its required checks and review.
#
# Usage: open_release_pr.sh [--plan] [--existing-only]
#   --plan         decide only. Writes action=open|existing|reconcile|blocked|none to
#                  $GITHUB_OUTPUT.
#   --existing-only only inspect or replace an existing release. A push to main
#                  runs this way, so adding a fragment cannot start a release.
# Without --plan the decision is taken again (it may be minutes newer than the
# plan) and action=opened|existing|blocked|none is written, with version and
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
source "$script_root/scripts/lib/release_attempt_plan.sh"
source "$script_root/scripts/lib/release_ship_enrollment.sh"

mode=open
existing_only=0
receipt_path="${HARN_EXT_RELEASE_OPENER_RECEIPT:-}"
if [[ -n "$receipt_path" ]]; then
  rm -f -- "$receipt_path"
fi
for arg in "$@"; do
  case "$arg" in
    --plan) mode=plan ;;
    --existing-only) existing_only=1 ;;
    *)
      echo "usage: open_release_pr.sh [--plan] [--existing-only]" >&2
      exit 2
      ;;
  esac
done

# An outcome is an observation of this invocation, never permission to publish.
# The workflow exports it so consumers need not infer decisions from notices.
receipt_action=pending
receipt_version=""
receipt_pr_url=""
receipt_release_source=""
supersession_plan='{"superseded":[]}'
receipt_source="$(git rev-parse HEAD)"
if [[ -n "$receipt_path" ]]; then
  if [[ -z "${GITHUB_REPOSITORY:-}" ]] \
    || ! [[ "${GITHUB_RUN_ID:-}" =~ ^[1-9][0-9]*$ ]] \
    || ! [[ "${GITHUB_RUN_ATTEMPT:-}" =~ ^[1-9][0-9]*$ ]]; then
    echo "error: an opener receipt requires repository, run ID and attempt" >&2
    exit 1
  fi
fi

write_outcome() {
  local exit_code=$? temporary
  trap - EXIT
  if [[ -n "${body_file:-}" ]]; then
    rm -f -- "$body_file"
  fi
  if [[ -n "$receipt_path" ]]; then
    mkdir -p "$(dirname "$receipt_path")"
    temporary="$(mktemp "${receipt_path}.XXXXXX")"
    jq -n --arg repository "$GITHUB_REPOSITORY" \
      --argjson run_id "$GITHUB_RUN_ID" --argjson run_attempt "$GITHUB_RUN_ATTEMPT" \
      --arg source_sha "$receipt_source" --arg phase "$mode" \
      --arg decision "$receipt_action" --arg version "$receipt_version" \
      --arg pr_url "$receipt_pr_url" --arg release_source_sha "$receipt_release_source" \
      --argjson recovery "$supersession_plan" \
      --argjson exit_code "$exit_code" \
      '{schema:"harn.release-opener.v1", repository:$repository,
        workflow:"bump-release.yml", run_id:$run_id, run_attempt:$run_attempt,
        source_sha:$source_sha, phase:$phase, decision:$decision,
        version:$version, pr_url:$pr_url, release_source_sha:$release_source_sha,
        exit_code:$exit_code,stage:"release-opener",recovery:$recovery,
        next_action:(if $exit_code != 0 then "inspect-opener" elif $decision == "blocked" then "wait-for-green"
          elif $decision == "reconcile" then "complete-supersession" elif $decision == "open" then "prepare"
          elif $decision == "opened" or $decision == "existing" then "wait-for-delivery" else "none" end)}' > "$temporary"
    mv -- "$temporary" "$receipt_path"
  fi
  exit "$exit_code"
}
trap write_outcome EXIT

emit() {
  local field
  for field in "$@"; do
    case "$field" in
      action=*) receipt_action="${field#action=}" ;;
      version=*) receipt_version="${field#version=}" ;;
      pr_url=*) receipt_pr_url="${field#pr_url=}" ;;
    esac
  done
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
receipt_version="$version"
if [[ "$version" == "$current" ]] \
  || ! [[ "$version" =~ ^(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)$ ]]; then
  echo "::notice title=Nothing to release::main declares ${current:-no workspace version}, not an X.Y.Z$development_suffix development version. The next release starts once the development bump lands."
  emit action=none version= pr_url=
  exit 0
fi
title="Release v$version"
branch="release/v$version"
canonical_branch="$branch"

# Find all open release pull requests for this version. A failed lookup must not read
# as "no pull request": opening a second release pull request for one version
# is the failure this check exists to prevent.
read_release_pr() {
  local found
  if ! found="$(gh pr list --state open --base main --limit 1000 \
    --json url,title,headRefName \
    --jq "[.[] | select(.title == \"$title\" or .headRefName == \"$canonical_branch\" or (.headRefName | startswith(\"release-attempt/v$version/\")))]")"; then
    echo "error: could not list open pull requests; refusing to open $title on unproved state" >&2
    exit 1
  fi
  existing_prs="$(jq -ce 'if type == "array" then . else error("unmeasured release PR list") end' <<< "$found")"
}

# Resolve frozen, conflicted, and partially delivered attempts before preparing.
resolve_existing_attempts() {
  local plan action current_url current_sha
  if ! plan="$(release_attempt_plan "$version" "$receipt_source" "$existing_prs")"; then
    echo "error: refusing to reuse $title on unproved state" >&2
    exit 1
  fi
  supersession_plan="$plan"
  action="$(jq -r .action <<< "$plan")"
  case "$action" in
    wait-green)
      echo "::notice::Release inputs changed on main; waiting for green to include those exact blobs before preparing $title."
      emit action=blocked "version=$version" pr_url=
      exit 0 ;;
    open)
      echo "::notice::Preparing a coherent green cut as a new immutable $title attempt."
      return 0 ;;
    existing|reconcile)
      current_url="$(jq -r .current.url <<< "$plan")"
      current_sha="$(jq -r .current.head_sha <<< "$plan")"
      receipt_release_source="$current_sha"
      if [[ "$action" == reconcile ]]; then
        if [[ "$mode" == plan ]]; then
          emit action=reconcile "version=$version" "pr_url=$current_url"
          exit 0
        fi
        release_deliver_attempt "$plan" "$current_url" "$current_sha"
      fi
      echo "::notice title=Release pull request already open::$title is frozen at $current_sha: $current_url. Later fragments wait for the next release."
      emit action=existing "version=$version" "pr_url=$current_url"
      exit 0 ;;
    *) echo 'error: unmeasured release-attempt recovery decision' >&2; exit 1 ;;
  esac
}

read_release_pr
if (( $(jq length <<< "$existing_prs") > 0 )); then
  resolve_existing_attempts
elif (( existing_only )); then
  echo "::notice title=Nothing to release::no $title pull request is open, and this run only checks existing releases."
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
echo "$title: ${#fragments[@]} unreleased changelog fragment(s) on green"
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
  echo "error: opening a release pull request requires a clean checkout of green" >&2
  exit 1
fi

# Release only a commit whose full CI suite passed. The workflow checks out
# green, and every commit green has named carries a `green-*` checkpoint tag.
base_oid="$(git rev-parse HEAD)"
if ! checkpoints="$(git ls-remote --tags origin 'refs/tags/green-*')"; then
  echo "error: could not list green checkpoints; refusing to open $title on unproved state" >&2
  exit 1
fi
if ! awk -v sha="$base_oid" '$1 == sha && $2 ~ /^refs\/tags\/green-.*\^\{\}$/ { found = 1 } END { exit !found }' <<< "$checkpoints"; then
  echo "error: $base_oid is not a green checkpoint; $title is cut only from green" >&2
  exit 1
fi
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
if (( $(jq length <<< "$existing_prs") > 0 )); then
  resolve_existing_attempts
elif (( $(jq '.superseded | length' <<< "$supersession_plan") > 0 )); then
  echo '::notice::The conflicted release was closed while preparation ran; stopping replacement.'
  emit action=none "version=$version" pr_url=
  exit 0
fi

# Publish one signed release commit and freeze its identity.
publication_branch="release-staging/v$version/${GITHUB_RUN_ID:-$base_oid}-${GITHUB_RUN_ATTEMPT:-1}"
publication="$(HARN_BRANCH_COMMIT_TOKEN="$GH_TOKEN" \
  HARN_BRANCH_COMMIT_BRANCH="$publication_branch" \
  HARN_BRANCH_COMMIT_BASE_OID="$base_oid" \
  HARN_BRANCH_COMMIT_HEADLINE="$title" \
  "$harn_bin" run --no-sandbox "$script_root/scripts/bump-driver/publish_branch_commit.harn")"
published_oid="$(jq -er '.oid | select(type == "string" and test("^[0-9a-f]{40}$"))' <<< "$publication")"
receipt_release_source="$published_oid"
published_record="$(git ls-remote --refs origin "refs/heads/$publication_branch")"
if [[ "$published_record" != "$published_oid"$'\t'"refs/heads/$publication_branch" ]]; then
  echo "error: published branch read-back did not match the signed publisher receipt" >&2
  exit 1
fi
attempt_ref="refs/heads/release-attempt/v$version/$published_oid"
# Record the signed published commit before a pull request can enter the queue.
# The suffix and target are the existing release orchestrator's contract.
attempt_record="$(git ls-remote --refs origin "$attempt_ref")"
if [[ -z "$attempt_record" ]]; then
  gh api --method POST "repos/$GITHUB_REPOSITORY/git/refs" \
    -f "ref=$attempt_ref" -f "sha=$published_oid" >/dev/null
  attempt_record="$(git ls-remote --refs origin "$attempt_ref")"
fi
if [[ "$attempt_record" != "$published_oid"$'\t'"$attempt_ref" ]]; then
  echo "error: immutable release attempt read-back did not match the published commit" >&2
  exit 1
fi

body_file="$(mktemp)"
cat > "$body_file" <<EOF
Moves the workspace from $current to $version and folds ${#fragments[@]} changelog fragment(s) into the \`## v$version\` section of CHANGELOG.md, deleting them.

When this merges, the push to main builds and checks the release candidate at that commit, and promotion publishes exactly those files. Prepared by \`scripts/open_release_pr.sh\` from green at $base_oid.
EOF
pr_url="$(gh pr create --base main --head "${attempt_ref#refs/heads/}" --title "Release v$version" --body-file "$body_file")"
echo "Opened $title: $pr_url"
# Arm now, before the checks settle. The URL is emitted first so a failed arm
# still names the pull request it left unarmed.
emit "version=$version" "pr_url=$pr_url"
release_deliver_attempt "$supersession_plan" "$pr_url" "$published_oid" true
emit action=opened
