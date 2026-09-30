#!/usr/bin/env bash
# Release-tree identity and unfolded-changelog policy.
#
# Callers select the authoritative release tree first, then apply the fragment
# guard to that same checkout. Keeping both decisions here prevents a fast
# branch-tree check from standing in for the immutable tag-tree check.

FINALIZE_TAG=""

require_existing_release_tag_checkout() {
  local base_branch="$1"
  local branch
  branch="$(git branch --show-current)"
  if [[ -n "$branch" ]]; then
    echo "error: release_ship.sh --finalize must run from $base_branch or detached at a stable release tag; current branch is $branch"
    exit 1
  fi

  local release_tags=()
  local candidate
  while IFS= read -r candidate; do
    if [[ "$candidate" =~ ^v[0-9]+\.[0-9]+\.[0-9]+$ ]]; then
      release_tags+=("$candidate")
    fi
  done < <(git tag --points-at HEAD)

  if (( ${#release_tags[@]} == 0 )); then
    echo "error: release_ship.sh --finalize is detached, but HEAD is not selected by a stable release tag"
    exit 1
  fi
  if (( ${#release_tags[@]} > 1 )); then
    echo "error: release_ship.sh --finalize is detached at a commit selected by multiple stable release tags:"
    printf '  - %s\n' "${release_tags[@]}"
    exit 1
  fi

  FINALIZE_TAG="${release_tags[0]}"
  echo "Finalize recovery from existing tag $FINALIZE_TAG at $(git rev-parse HEAD)"
}

# Print each unfolded `changelog.d/<id>.<category>.md` fragment in the current
# checkout, one path per line. Shell callers that must decide before a Harn
# binary exists read fragments here. `scripts/release_changelog_fold.harn` owns
# the fold and names the same categories; `release_ship.sh --prepare` runs the
# guard below after the fold, so a fragment this listing sees and the fold
# skips fails prepare instead of shipping without its entry.
unfolded_fragment_paths() {
  local dir="changelog.d"
  [[ -d "$dir" ]] || return 0
  local category fragment base
  for category in breaking added changed deprecated removed fixed security; do
    for fragment in "$dir"/*."$category".md; do
      [[ -e "$fragment" ]] || continue
      base="$(basename "$fragment")"
      [[ "$base" == README* || "$base" == _* ]] && continue
      printf '%s\n' "$fragment"
    done
  done
}

# Fail loud if unfolded `changelog.d/<id>.<category>.md` fragments remain.
#
# `release_ship.sh --prepare` folds fragments into the `## vX.Y.Z` section
# (scripts/release_changelog_fold.harn) and then runs this guard. Finalizing a
# tree that still carries fragments would ship a release whose CHANGELOG has no
# entries for them and whose --finalize renders empty release notes.
require_no_unfolded_fragments() {
  local frags=()
  local fragment
  while IFS= read -r fragment; do
    [[ -n "$fragment" ]] && frags+=("$fragment")
  done < <(unfolded_fragment_paths)
  if (( ${#frags[@]} == 0 )); then
    return 0
  fi

  # A tagged merge tree may legitimately carry fragments that landed after
  # its immutable release candidate. Resolve the candidate by the changelog
  # blob both trees share, then defer only fragments absent from that candidate.
  # Failure to resolve exactly one candidate, or any candidate-owned fragment,
  # keeps the existing fail-closed behavior below.
  if [[ -n "${FINALIZE_TAG:-}" && ${ALLOW_UNFOLDED_FRAGMENTS:-0} != 1 ]]; then
    local scope owned_count deferred_count
    scope="$(bash "$SCRIPT_DIR/lib/release_fragment_scope.sh" \
      --repo "$ROOT_DIR" --tree HEAD --version "${FINALIZE_TAG#v}")"
    owned_count="$(jq -er '.owned | length' <<< "$scope")"
    deferred_count="$(jq -er '.deferred | length' <<< "$scope")"
    if [[ "$(jq -r '.resolved' <<< "$scope")" == "true" \
      && "$owned_count" -eq 0 && "$deferred_count" -eq "${#frags[@]}" ]]; then
      echo "warning: deferring ${deferred_count} post-candidate changelog fragment(s) to the next release:" >&2
      jq -r '.deferred[] | "  - " + .' <<< "$scope" >&2
      return 0
    fi
  fi

  # An already-tagged release cannot be corrected on a branch. The recovery
  # escape records the immutable omission rather than hiding it and is limited
  # by release_ship.sh to finalize mode.
  if (( ${ALLOW_UNFOLDED_FRAGMENTS:-0} == 1 )); then
    {
      echo "warning: finalizing with ${#frags[@]} unfolded changelog fragment(s):"
      printf '  - %s\n' "${frags[@]}"
      echo "These entries are NOT in this release's notes. They remain on the"
      echo "default branch, so the next release folds them and they appear"
      echo "under that version instead. This is recovery for a release that"
      echo "was tagged before its fragments were folded; the tag's tree cannot"
      echo "be corrected, so the omission is recorded here instead."
    } >&2
    return 0
  fi

  {
    echo "error: ${#frags[@]} unfolded changelog fragment(s) remain in changelog.d/:"
    printf '  - %s\n' "${frags[@]}"
    echo "hint: the Release vX.Y.Z pull request folds changelog fragments. Either:"
    echo "        (a) open it with the 'Open release PR' workflow"
    echo "            (.github/workflows/bump-release.yml), which runs"
    echo "            'release_ship.sh --prepare' and folds every fragment, or"
    echo "        (b) run 'harn run scripts/release_changelog_fold.harn -- fold"
    echo "            --version X.Y.Z' and commit the result, then re-run."
    echo "      Shipping now would omit these entries from the release notes."
    echo "      If the release is ALREADY TAGGED, neither remedy can reach the"
    echo "      tag's tree; use --allow-unfolded-fragments with --finalize to"
    echo "      complete it and record the omission."
  } >&2
  exit 1
}
