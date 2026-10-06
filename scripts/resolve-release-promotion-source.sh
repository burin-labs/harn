#!/usr/bin/env bash
# Resolve the existing producer's source before checking out candidate code.
# Publication still verifies the manifest, file digests and attestations.
set -euo pipefail

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
    and (.event == "merge_group" or (.event == "push" and .head_branch == "main")))
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
{
  echo "source_sha=$sha"
  echo "run_id=$run_id"
} >> "${GITHUB_OUTPUT:?output required}"
