#!/usr/bin/env bash

# Shell projection of std/semver's release-version boundary for bootstrap and
# GitHub Actions code that cannot execute Harn yet. Keep the fixture matrix in
# scripts/tests/release_version_test.sh aligned with std/semver conformance.

RELEASE_VERSION_LIB_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# Generated from scripts/release_contract.harn. This bootstrap shell cannot
# execute Harn, so it consumes the mechanically checked projection.
source "$RELEASE_VERSION_LIB_DIR/../release_contract.env"

release_version_is_canonical() {
  local version="${1:-}"
  if [[ ! "$version" =~ ^(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)(-([0-9A-Za-z.-]+))?$ ]]; then
    return 1
  fi
  local prerelease="${BASH_REMATCH[5]:-}"
  if [[ -z "$prerelease" ]]; then
    return 0
  fi
  local identifiers=()
  IFS=. read -r -a identifiers <<<"$prerelease"
  local identifier
  for identifier in "${identifiers[@]}"; do
    if [[ -z "$identifier" || ! "$identifier" =~ ^[0-9A-Za-z-]+$ ]]; then
      return 1
    fi
    if [[ "$identifier" =~ ^[0-9]+$ && ${#identifier} -gt 1 && "$identifier" == 0* ]]; then
      return 1
    fi
  done
}

release_version_is_prerelease() {
  release_version_is_canonical "${1:-}" && [[ "$1" == *-* ]]
}

release_next_patch_development() {
  local stable="${1:-}"
  if [[ ! "$stable" =~ ^(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)$ ]]; then
    return 1
  fi
  printf '%s.%s.%s-%s\n' \
    "${BASH_REMATCH[1]}" \
    "${BASH_REMATCH[2]}" \
    "$(( 10#${BASH_REMATCH[3]} + 1 ))" \
    "$HARN_RELEASE_DEVELOPMENT_PRERELEASE"
}

release_development_target_matches_stable() {
  local development="${1:-}"
  local stable="${2:-}"
  local expected
  expected="$(release_next_patch_development "$stable")" || return 1
  [[ "$development" == "$expected" ]]
}

# True when a development identity names the same patch that has since been
# selected by a stable tag. This is the recovery shape where certification
# tagged an immutable release candidate without merging that candidate back to
# main: main remains on X.Y.Z-dev until publication completes, then advances to
# X.Y.(Z+1)-dev.
release_development_target_precedes_stable() {
  local development="${1:-}"
  local stable="${2:-}"
  release_version_is_canonical "$stable" || return 1
  release_version_is_prerelease "$stable" && return 1
  [[ "$development" == "${stable}-${HARN_RELEASE_DEVELOPMENT_PRERELEASE}" ]]
}

# Compare numeric release generations without shell integer overflow. A newer
# workspace retires an unpublished older source, while a published source is
# handled before this guard so its remaining distribution steps can finish.
release_workspace_supersedes_source() {
  local source="${1:-}" workspace="${2:-}" index left right
  release_version_is_canonical "$source" || return 2
  release_version_is_prerelease "$source" && return 2
  release_version_is_canonical "$workspace" || return 2
  local source_parts=() workspace_parts=()
  IFS=. read -r -a source_parts <<< "$source"
  IFS=. read -r -a workspace_parts <<< "${workspace%%-*}"
  for index in 0 1 2; do
    left="${source_parts[$index]}" right="${workspace_parts[$index]}"
    if [[ ${#right} -gt ${#left} ]]; then return 0; fi
    if [[ ${#right} -lt ${#left} ]]; then return 1; fi
    if [[ "$right" > "$left" ]]; then return 0; fi
    if [[ "$right" < "$left" ]]; then return 1; fi
  done
  return 1
}

release_published_version_for_workspace() {
  local workspace="${1:-}"
  if [[ "$workspace" =~ ^(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)$ ]]; then
    printf '%s\n' "$workspace"
    return 0
  fi
  if [[ "$workspace" =~ ^(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)-${HARN_RELEASE_DEVELOPMENT_PRERELEASE}$ ]] \
    && (( 10#${BASH_REMATCH[3]} > 0 )); then
    printf '%s.%s.%s\n' \
      "${BASH_REMATCH[1]}" \
      "${BASH_REMATCH[2]}" \
      "$(( 10#${BASH_REMATCH[3]} - 1 ))"
    return 0
  fi
  return 1
}

release_head_is_release_commit_for_version() {
  local version="${1:-}"
  release_version_is_canonical "$version" || return 1
  release_version_is_prerelease "$version" && return 1
  local subject
  subject="$(git log -1 --format='%s' HEAD)" || return 1
  [[ "$subject" =~ ^Release\ v"$version"([[:space:]].*)?$ ]] || return 1
  git show HEAD -- Cargo.toml \
    | grep -Eq "^\+version = \"$version\"$"
}

# A required squash merge replaces a certified release commit with a new commit
# carrying the same patch. Accept that fold without weakening the ancestry check
# to a matching version or subject: every commit unique to the tag's history
# must have a patch-equivalent commit in HEAD.
release_tag_is_represented_in_head() {
  local tag="${1:-}"
  [[ -n "$tag" ]] || return 1

  git merge-base --is-ancestor "$tag" HEAD && return 0

  local cherry line saw_commit=false
  cherry="$(git cherry HEAD "$tag" 2>/dev/null)" || return 1
  while IFS= read -r line; do
    [[ -n "$line" ]] || continue
    saw_commit=true
    [[ "$line" == "- "* ]] || return 1
  done <<< "$cherry"
  [[ "$saw_commit" == true ]]
}

# Decide whether a published stable workspace needs the next development
# identity. This is release state, not branch-tip authorship: after the tag is
# public, unrelated commits may sit above the release commit without changing
# which stable version the workspace still declares.
#
# Results are returned in RELEASE_DEVELOPMENT_BUMP_* globals so workflow source
# and fixture tests consume one decision owner.
# shellcheck disable=SC2034 # public result globals are consumed by sourcing callers
release_development_bump_plan() {
  local workspace_version="${1:-}"
  local latest_tag="${2:-}"
  local publication_complete="${3:-false}"

  RELEASE_DEVELOPMENT_BUMP_REQUIRED=false
  RELEASE_DEVELOPMENT_BUMP_VERSION=""
  RELEASE_DEVELOPMENT_BUMP_REASON=""

  if [[ -z "$latest_tag" ]]; then
    RELEASE_DEVELOPMENT_BUMP_REASON="no_stable_release_tag"
    return 0
  fi
  if ! release_tag_is_canonical "$latest_tag" \
    || release_version_is_prerelease "${latest_tag#v}"; then
    RELEASE_DEVELOPMENT_BUMP_REASON="latest_tag_is_not_stable"
    return 0
  fi
  if [[ "$publication_complete" != true ]]; then
    RELEASE_DEVELOPMENT_BUMP_REASON="latest_stable_release_not_published"
    return 0
  fi

  local latest_version="${latest_tag#v}"
  if release_development_target_precedes_stable "$workspace_version" "$latest_version"; then
    RELEASE_DEVELOPMENT_BUMP_VERSION="$(release_next_patch_development "$latest_version")" \
      || return 1
    RELEASE_DEVELOPMENT_BUMP_REQUIRED=true
    RELEASE_DEVELOPMENT_BUMP_REASON="published_candidate_supersedes_development_identity"
    return 0
  fi
  if [[ "$workspace_version" != "$latest_version" ]]; then
    RELEASE_DEVELOPMENT_BUMP_REASON="workspace_does_not_match_latest_stable"
    return 0
  fi
  if ! release_tag_is_represented_in_head "$latest_tag"; then
    RELEASE_DEVELOPMENT_BUMP_REASON="latest_stable_tag_is_not_in_head_ancestry"
    return 0
  fi

  RELEASE_DEVELOPMENT_BUMP_VERSION="$(release_next_patch_development "$workspace_version")" \
    || return 1
  RELEASE_DEVELOPMENT_BUMP_REQUIRED=true
  RELEASE_DEVELOPMENT_BUMP_REASON="published_stable_needs_development_identity"
}

# Explicit repair of a stable identity that never became a release. Historical
# failure is necessary but insufficient: publication and publishers are read
# again every time this is called, including immediately before opening a PR.
release_validate_retirement_request() {
  local field count=0
  for field in RETIRE_SOURCE_SHA RETIRE_PRODUCER_RUN RETIRE_PROMOTION_RUN \
    RETIRE_RESOLVER_JOB RETIRE_CONSUMER_JOB RETIRE_AUTHORIZATION_JOB RETIRE_CONSUMER_RUN RETIRE_FAILED_JOB RETIRE_RELEASE_PR; do
    [[ -z "${!field:-}" ]] || count=$((count + 1))
  done
  if [[ "$count" != 0 && "$count" != 9 ]]; then
    echo "error: incomplete unpublished retirement request; refusing partial authority" >&2
    return 1
  fi
}

release_require_unpublished_retirement() (
  set -euo pipefail
  release_validate_retirement_request
  local repository="${GITHUB_REPOSITORY:?repository required}"
  local version="${1:?workspace version required}" published_tag="${2:?published predecessor required}"
  local source="${RETIRE_SOURCE_SHA:?retired source required}"
  local GH_TOKEN="${RELEASE_OBSERVATION_TOKEN:-${GH_TOKEN:-}}"
  export GH_TOKEN
  local producer="${RETIRE_PRODUCER_RUN:?producer required}"
  local parent="${RETIRE_PROMOTION_RUN:?failed promotion required}"
  local release_pr="${RETIRE_RELEASE_PR:?owning release pull request required}"
  local tags attempts releases runs workflow observation scratch artifacts manifest source_cargo producer_run census status control pull release_head
  # shellcheck source=scripts/lib/candidate_archive_contract.sh
  source "$RELEASE_VERSION_LIB_DIR/candidate_archive_contract.sh"
  release_version_is_canonical "$version"
  if release_version_is_prerelease "$version"; then return 1; fi
  release_tag_is_canonical "$published_tag"
  [[ "$version" != "${published_tag#v}" ]]
  [[ "$source" =~ ^[0-9a-f]{40}$ && "$producer" =~ ^[1-9][0-9]*$ && "$parent" =~ ^[1-9][0-9]*$ && "$release_pr" =~ ^[1-9][0-9]*$ ]]
  # The same producer/source validation used by promotion, including main ancestry.
  scratch="$(mktemp -d)"
  trap 'rm -rf "$scratch"' EXIT
  GITHUB_OUTPUT="$scratch/resolved" CANDIDATE_RUN_ID="$producer" EXPECTED_SOURCE_SHA="$source" \
    bash "$RELEASE_VERSION_LIB_DIR/../resolve-release-promotion-source.sh"
  producer_run="$(gh api "repos/$repository/actions/runs/$producer")"
  jq -e '.run_attempt | type == "number" and . > 0 and . == floor' <<< "$producer_run" >/dev/null
  source_cargo="$(gh api "repos/$repository/contents/Cargo.toml?ref=$source" --jq '.content | @base64d')"
  [[ "$(release_workspace_version <<< "$source_cargo")" == "$version" ]]
  pull="$(gh api "repos/$repository/pulls/$release_pr")"
  release_head="$(jq -er --arg repository "$repository" --arg number "$release_pr" --arg source "$source" '
    select((.number | tostring) == $number and .state == "closed" and .merged == true and
      (.merged_at | type == "string" and length > 0) and .merge_commit_sha == $source and
      .base.ref == "main" and .base.repo.full_name == $repository and .head.repo.full_name == $repository)
    | .head.sha | select(type == "string" and test("^[0-9a-f]{40}$"))
  ' <<< "$pull")"
  # Bind the canonical retained manifest, rather than inferring a candidate
  # from a green run or from a title. Only this small artifact is downloaded.
  artifacts="$(gh api "repos/$repository/actions/runs/$producer/artifacts?name=candidate-manifest-$source")"
  jq -e --arg name "candidate-manifest-$source" --arg source "$source" --arg producer "$producer" '
    .total_count == 1 and (.artifacts | length) == 1 and
    .artifacts[0].name == $name and .artifacts[0].expired == false and
    (.artifacts[0].id | type == "number" and . > 0) and
    (.artifacts[0].workflow_run.id | tostring) == $producer and
    .artifacts[0].workflow_run.head_sha == $source
  ' <<< "$artifacts" >/dev/null
  gh run download "$producer" --repo "$repository" --name "candidate-manifest-$source" --dir "$scratch/manifest"
  manifest="$scratch/manifest/candidate-manifest.json"
  jq -e --arg repository "$repository" --arg source "$source" --arg producer "$producer" \
    --arg schema "$CANDIDATE_MANIFEST_SCHEMA" --arg predicate "$RELEASE_ARCHIVE_PREDICATE_TYPE" \
    --argjson targets "$(candidate_archive_expected_targets_json)" --argjson producer_run "$producer_run" '
    .schemaVersion == $schema and .repository == $repository and
    .sourceCommit == $source and (.runId | tostring) == $producer and
    (.runAttempt | tostring) == ($producer_run.run_attempt | tostring) and
    ([.artifacts[] | select(.kind == "archive")] | length) == 5 and
    ([.artifacts[] | select(.kind == "archive") | .target] | sort) ==
      ($targets | sort) and
    all(.artifacts[] | select(.kind == "archive");
      (.sha256 | type == "string" and test("^[0-9a-f]{64}$")) and
      .attestationPredicateType == $predicate)
  ' "$manifest" >/dev/null
  observation="$(release_authenticated_failed_rehearsal "$repository" "$parent" "$producer" "$source" \
    "${RETIRE_RESOLVER_JOB:?resolver job required}" "${RETIRE_CONSUMER_JOB:?consumer job required}" \
    "${RETIRE_AUTHORIZATION_JOB:?authorization job required}" \
    "${RETIRE_CONSUMER_REPOSITORY:?consumer repository required}" \
    "${RETIRE_CONSUMER_RUN:?consumer run required}" "${RETIRE_FAILED_JOB:?failed consumer job required}")"
  # Force a known positive through both authoritative absence paths. Errors,
  # empty lists and incomplete pagination are not evidence of nonpublication.
  tags="$(git ls-remote --tags origin)"
  awk -v known="refs/tags/$published_tag" -v retired="refs/tags/v$version" '
    NF != 2 || $1 !~ /^[0-9a-f]+$/ || length($1) != 40 || $2 !~ /^refs\/tags\// {bad=1}
    $2 == known {seen++}
    $2 == retired || $2 == retired "^{}" {bad=1}
    END {exit(bad || seen != 1)}
  ' <<< "$tags"
  attempts="$(git ls-remote --refs origin refs/heads/main "refs/heads/release-attempt/v$version/*")"
  # The immutable attempt records the release PR head. A merge queue gives
  # the landed, certified source a different SHA; the actual merged PR binds
  # those identities, never a matching title or a patch-equivalence guess.
  awk -v prefix="refs/heads/release-attempt/v$version/" -v source="$release_head" '
    NF != 2 || $1 !~ /^[0-9a-f]+$/ || length($1) != 40 {bad=1}
    $2 == "refs/heads/main" {seen++; next}
    $2 != prefix source || $1 != source {bad=1}
    END {exit(bad || seen != 1)}
  ' <<< "$attempts"
  releases="$(gh api --paginate --slurp "repos/$repository/releases?per_page=100")"
  jq -e --arg known "$published_tag" --arg retired "v$version" '
    type == "array" and length > 0 and all(.[]; type == "array" and length <= 100) and
    all(.[:-1][]; length == 100) and
    ([.[][] | select(.tag_name == $known and .draft == false and .prerelease == false and
      (.published_at | type == "string" and length > 0))] | length) == 1 and
    all(.[][]; (.id | type == "number" and . > 0) and
      (.tag_name | type == "string" and length > 0) and .tag_name != $retired)
  ' <<< "$releases" >/dev/null
  # Read the two publication owners. A known nonempty read through the same
  # workflow-runs endpoint distinguishes measured zero from measuring nothing.
  # Then paginate each active status completely, without walking terminal
  # history whose size cannot change whether publication is in flight.
  for workflow in promote-release.yml publish-release.yml; do
    control="$(gh api "repos/$repository/actions/workflows/$workflow/runs?per_page=1")"
    jq -e --arg repository "$repository" --arg path ".github/workflows/$workflow" '
      (.total_count | type == "number" and . >= 1 and . == floor) and
      (.workflow_runs | length) == 1 and
      (.workflow_runs[0].id | type == "number" and . > 0) and
      .workflow_runs[0].repository.full_name == $repository and .workflow_runs[0].path == $path and
      (.workflow_runs[0].status as $status |
        ["completed","queued","requested","waiting","pending","in_progress"] | index($status) != null)
    ' <<< "$control" >/dev/null
    for status in queued requested waiting pending in_progress; do
      runs="$(gh api --paginate --slurp "repos/$repository/actions/workflows/$workflow/runs?status=$status&per_page=100")"
      census="$(jq -ce --arg repository "$repository" --arg path ".github/workflows/$workflow" --arg status "$status" '
      if type == "array" and length > 0 and
      all(.[]; (.workflow_runs | type) == "array" and
        (.total_count | type == "number" and . >= 0 and . == floor))
      then . else error("missing publication-owner census") end |
      [.[].workflow_runs[]] as $runs |
      if all(.[]; .total_count == ($runs | length)) and
      ([$runs[].id] | unique | length) == ($runs | length) and
      all($runs[]; (.id | type == "number" and . > 0) and .repository.full_name == $repository and
        .path == $path and .status == $status and .conclusion == null)
      then $runs | {observed:length, pending:length, unfinished:map({id,status,path})}
      else error("incomplete publication-owner census") end
      ' <<< "$runs")"
      echo "Retirement publication census workflow=$workflow status=$status control_observed=1 $census" >&2
      jq -e '.pending == 0' <<< "$census" >/dev/null
    done
  done
  jq -ce --arg release_pr "$release_pr" --arg attempt_source "$release_head" \
    '. + {release_pr:$release_pr, attempt_source:$attempt_source}' <<< "$observation"
)

# Print the root workspace version from a Cargo.toml read on stdin: the first
# top-level `version = "..."` line, which is `[workspace.package]` in this
# repository. Prints nothing when there is none.
release_workspace_version() {
  sed -n 's/^version = "\([^"]*\)"$/\1/p' | head -n 1
}

# A push to main is a release candidate exactly when it changes the workspace
# version to a canonical stable X.Y.Z. The version decides, never the commit
# subject: a development bump, a prerelease, or an unchanged version is not a
# release, and a missing previous version (a root commit) is still a change.
release_push_is_stable_version_change() {
  local previous="${1:-}"
  local current="${2:-}"
  release_version_is_canonical "$current" \
    && ! release_version_is_prerelease "$current" \
    && [[ "$current" != "$previous" ]]
}

# Recovery may certify a corrected descendant of an unpublished stable cut.
# Find the version transition on its first-parent history; missing history is
# not evidence of a release. The caller still checks tags and certification.
release_recovery_transition() {
  local source="${1:?source commit required}" version="${2:?version required}"
  local commits commit toml current owner="$source"
  release_version_is_canonical "$version" && ! release_version_is_prerelease "$version" || return 1
  toml="$(git show "$source:Cargo.toml")" || return 1
  [[ "$(release_workspace_version <<< "$toml")" == "$version" ]] || return 1
  commits="$(git rev-list --first-parent "$source" -- Cargo.toml)" || return 1
  while IFS= read -r commit; do
    [[ -n "$commit" ]] || return 1
    toml="$(git show "$commit:Cargo.toml")" || return 1
    current="$(release_workspace_version <<< "$toml")"
    release_version_is_canonical "$current" || return 1
    if [[ "$current" != "$version" ]]; then
      printf '%s\n' "$owner"
      return 0
    fi
    owner="$commit"
  done <<< "$commits"
  return 1
}

# The commits in base..head (first parent, oldest first) that change the
# workspace version to a stable X.Y.Z. A merge queue lands several entries in
# one push, so the pushed head's parent need not be the previous main: a push is
# judged over its whole range, never by HEAD^ alone.
release_range_release_commits() {
  local base="${1:?base commit required}"
  local head="${2:?head commit required}"
  local previous current commit
  previous="$(git show "$base:Cargo.toml" | release_workspace_version)"
  while read -r commit; do
    current="$(git show "$commit:Cargo.toml" | release_workspace_version)"
    if release_push_is_stable_version_change "$previous" "$current"; then
      printf '%s\n' "$commit"
    fi
    previous="$current"
  done < <(git rev-list --reverse --first-parent "$base..$head")
}

release_tag_is_canonical() {
  [[ "${1:-}" == v* ]] && release_version_is_canonical "${1#v}"
}

# Resolve publication identity from the repository's complete stable tag list.
# A certified tag may name an immutable candidate that is not in main's
# ancestry, so callers must not narrow this inventory with `--merged`.
release_latest_stable_tag() {
  local tag
  while IFS= read -r tag; do
    if release_tag_is_canonical "$tag" \
      && ! release_version_is_prerelease "${tag#v}"; then
      printf '%s\n' "$tag"
      return 0
    fi
  done < <(git tag --list 'v*' --sort=-v:refname)
  return 1
}

release_branch_is_canonical() {
  [[ "${1:-}" == release/v* ]] && release_version_is_canonical "${1#release/v}"
}

# Project a canonical release version into the public channel policy (GitHub
# latest flag and container tags) that publication applies. Results are returned in RELEASE_* globals so the
# workflow and its fixture test share one policy owner.
# shellcheck disable=SC2034 # public result globals are consumed by sourcing callers
release_publication_plan() {
  local version="${1:-}"
  local make_latest="${2:-false}"

  release_version_is_canonical "$version" || return 1
  [[ "$make_latest" == "true" || "$make_latest" == "false" ]] || return 1

  RELEASE_IS_PRERELEASE=false
  if release_version_is_prerelease "$version"; then
    RELEASE_IS_PRERELEASE=true
    [[ "$make_latest" == "false" ]] || return 1
  fi

  RELEASE_MAKE_LATEST="$make_latest"
  RELEASE_CONTAINER_TAGS=("$version")
  if [[ "$make_latest" == "true" ]]; then
    RELEASE_CONTAINER_TAGS+=("${version%.*}" latest)
  fi
}
