#!/usr/bin/env bash
# Resolve the existing producer's source before checking out candidate code.
# Publication still verifies the manifest, file digests and attestations.
set -euo pipefail

script_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=scripts/lib/release_version.sh
source "$script_dir/lib/release_version.sh"
# shellcheck source=scripts/lib/release_candidate_run.sh
source "$script_dir/lib/release_candidate_run.sh"
# shellcheck source=scripts/lib/candidate_archive_contract.sh
source "$script_dir/lib/candidate_archive_contract.sh"

repository="${GITHUB_REPOSITORY:?repository required}"
run_id="${CANDIDATE_RUN_ID:?candidate run required}"
expected_sha="${EXPECTED_SOURCE_SHA:?source required}"
[[ "$repository" =~ ^[A-Za-z0-9_.-]+/[A-Za-z0-9_.-]+$ &&
   "$run_id" =~ ^[1-9][0-9]*$ && "$expected_sha" =~ ^[0-9a-f]{40}$ ]] || {
  echo '::error::Invalid repository or candidate run identity.' >&2
  exit 1
}
run="$(gh api "repos/$repository/actions/runs/$run_id")"
sha="$(jq -er --arg repository "$repository" --arg run_id "$run_id" '
  select((.id | tostring) == $run_id
    and .repository.full_name == $repository
    and .head_repository.full_name == $repository
    and .path == ".github/workflows/build-release-binaries.yml"
    and .status == "completed" and .conclusion == "success"
    and (.event == "merge_group" or
      ((.event == "push" or .event == "workflow_dispatch") and .head_branch == "main")))
  | .head_sha | select(type == "string" and test("^[0-9a-f]{40}$"))
' <<< "$run")" || {
  echo "::error::Run $run_id is not a successful owning release producer." >&2
  exit 1
}
[[ "$sha" == "$expected_sha" ]] || {
  echo "::error::Candidate source does not match the requested publication identity." >&2
  exit 1
}
comparison="$(gh api "repos/$repository/compare/$sha...main")"
jq -e --arg sha "$sha" '
  (.status == "identical" or .status == "ahead") and .merge_base_commit.sha == $sha
' <<< "$comparison" >/dev/null || {
  echo "::error::Candidate $sha is not contained in main." >&2
  exit 1
}
if [[ "$(jq -r '.event' <<< "$run")" == workflow_dispatch ]]; then
  cargo="$(gh api "repos/$repository/contents/Cargo.toml?ref=$sha" \
    -H 'Accept: application/vnd.github.raw+json')"
  version="$(release_workspace_version <<< "$cargo")"
  if ! release_version_is_canonical "$version" || release_version_is_prerelease "$version"; then
    echo '::error::Manual recovery requires a canonical stable candidate version.' >&2
    exit 1
  fi
  expected="$(candidate_archive_expected_targets_json | jq -c --arg sha "$sha" \
    --arg files "$RELEASE_FILES_ARTIFACT" '[.[] | "harn-" + .] + ["candidate-manifest-" + $sha, $files]')"
  artifacts="$(gh api "repos/$repository/actions/runs/$run_id/artifacts?per_page=100")"
  if ! release_candidate_artifacts_complete "$artifacts" "$expected" ||
    ! jq -e --argjson run_id "$run_id" --arg sha "$sha" --argjson run "$run" '
      all(.artifacts[];
        .workflow_run.id == $run_id and .workflow_run.head_sha == $sha and
        .workflow_run.head_branch == "main" and
        .workflow_run.repository_id == $run.repository.id and
        .workflow_run.head_repository_id == $run.head_repository.id and
        (.workflow_run.repository_id | type == "number" and . > 0) and
        (.workflow_run.head_repository_id | type == "number" and . > 0))
    ' <<< "$artifacts" >/dev/null; then
    echo '::error::Manual producer lacks a complete, exact-source candidate artifact inventory.' >&2
    exit 1
  fi
fi
{
  echo "source_sha=$sha"
  echo "run_id=$run_id"
} >> "${GITHUB_OUTPUT:?output required}"
