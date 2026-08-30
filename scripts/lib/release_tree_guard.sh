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

# Fail loud if unfolded `changelog.d/<id>.<category>.md` fragments remain.
#
# The fold (fragments -> `## vX.Y.Z` CHANGELOG.md section) lives in the
# bump-fleet `release_harn.harn prepare` flow (apply_draft_release_notes ->
# lib/changelog.harn). `release_ship.sh` does not fold. Invoking release_ship
# directly with fragments still present would ship a release whose CHANGELOG
# has no entries for them and whose --finalize renders empty release notes.
require_no_unfolded_fragments() {
  local dir="changelog.d"
  [[ -d "$dir" ]] || return 0
  local frags=()
  local category fragment base
  for category in breaking added changed deprecated removed fixed security; do
    for fragment in "$dir"/*."$category".md; do
      [[ -e "$fragment" ]] || continue
      base="$(basename "$fragment")"
      [[ "$base" == README* || "$base" == _* ]] && continue
      frags+=("$fragment")
    done
  done
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
    echo "error: ${#frags[@]} unfolded changelog fragment(s) remain in $dir/:"
    printf '  - %s\n' "${frags[@]}"
    echo "hint: release_ship.sh does not fold changelog fragments; the fold is"
    echo "      part of the release_harn.harn 'prepare' flow. Either:"
    echo "        (a) drive the release through 'release_harn.harn ... prepare'"
    echo "            (recommended; it folds fragments, drafts + repairs notes), or"
    echo "        (b) fold them into CHANGELOG.md's top '## vX.Y.Z' section by hand"
    echo "            and 'git rm' the fragment files, then re-run."
    echo "      Shipping now would omit these entries from the release notes."
    echo "      If the release is ALREADY TAGGED, neither remedy can reach the"
    echo "      tag's tree; use --allow-unfolded-fragments with --finalize to"
    echo "      complete it and record the omission."
  } >&2
  exit 1
}
