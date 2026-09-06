#!/usr/bin/env bash
# Download the exact certified producer artifacts, never dispatch a replacement build.
set -euo pipefail
script_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=scripts/lib/candidate_archive_contract.sh
source "$script_dir/lib/candidate_archive_contract.sh"
if [[ $# != 7 ]]; then
  echo "usage: download_candidate_archive_promotion.sh REPO RUN MANIFEST_ID MANIFEST_DIGEST SOURCE POLICY OUTPUT" >&2
  exit 2
fi
repo="$1" run="$2" manifest_id="$3" manifest_digest="$4" source_sha="$5" policy="$6" output="$7"
[[ "$repo" =~ ^[A-Za-z0-9_.-]+/[A-Za-z0-9_.-]+$ && "$run" =~ ^[1-9][0-9]*$ && "$manifest_id" =~ ^[1-9][0-9]*$ && "$manifest_digest" =~ ^sha256:[0-9a-f]{64}$ && "$source_sha" =~ ^[0-9a-f]{40}$ && "$policy" =~ ^[0-9a-f]{40}$ ]] || { echo 'error: malformed certified archive identity' >&2; exit 2; }
[[ ! -e "$output" ]] || { echo 'error: promotion output must be fresh' >&2; exit 2; }
mkdir -p "$output"
scratch="$(mktemp -d)"
trap 'rm -rf "$scratch"' EXIT
gh api "repos/$repo/actions/runs/$run" > "$scratch/run.json"
jq -e --arg run "$run" --arg policy "$policy" '
  (.id | tostring) == $run and .event == "workflow_dispatch" and
  .path == ".github/workflows/build-release-binaries.yml" and
  .head_branch == "main" and .head_sha == $policy and
  .status == "completed" and .conclusion == "success"
' "$scratch/run.json" >/dev/null
attempt="$(jq -er '.run_attempt | tostring' "$scratch/run.json")"
gh api --paginate --slurp "repos/$repo/actions/runs/$run/artifacts?per_page=100" > "$scratch/pages.json"
jq '[.[].artifacts[]]' "$scratch/pages.json" > "$scratch/artifacts.json"
download_artifact() {
  local id="$1" name="$2" destination="$3" member="$4"
  local digest
  jq -e --arg id "$id" --arg name "$name" --arg run "$run" --arg policy "$policy" '
    [.[] | select((.id | tostring) == $id and .name == $name)] |
    length == 1 and (.[0] | .expired == false and
      (.workflow_run.id | tostring) == $run and .workflow_run.head_sha == $policy and
      (.digest | test("^sha256:[0-9a-f]{64}$")))
  ' "$scratch/artifacts.json" >/dev/null
  digest="$(jq -er --arg id "$id" '.[] | select((.id | tostring) == $id) | .digest' "$scratch/artifacts.json")"
  gh api "repos/$repo/actions/artifacts/$id/zip" > "$scratch/$id.zip"
  [[ "sha256:$(sha256_file "$scratch/$id.zip")" == "$digest" ]] || { echo 'error: artifact ZIP digest mismatch' >&2; return 1; }
  # Each canonical upload contains one flat file. Reject traversal and extras.
  [[ "$(unzip -Z1 "$scratch/$id.zip")" == "$member" ]] || { echo 'error: unexpected artifact ZIP members' >&2; return 1; }
  unzip -p "$scratch/$id.zip" "$member" > "$destination"
}
manifest_name="candidate-archive-manifest-$source_sha"
# The artifact name has no extension, its sole member does.
manifest_file="$output/candidate-archive-manifest.json"
jq -e --arg id "$manifest_id" --arg name "$manifest_name" '
  [.[] | select((.id | tostring) == $id and .name == $name)] | length == 1
' "$scratch/artifacts.json" >/dev/null
# Normalize only the transport member name, preserving the verified API identity.
manifest_zip="$scratch/manifest.zip"
gh api "repos/$repo/actions/artifacts/$manifest_id/zip" > "$manifest_zip"
jq -e --arg id "$manifest_id" --arg digest "$manifest_digest" --arg run "$run" --arg policy "$policy" '
  .[] | select((.id | tostring) == $id) |
  .expired == false and .digest == $digest and
  (.workflow_run.id | tostring) == $run and .workflow_run.head_sha == $policy
' "$scratch/artifacts.json" >/dev/null
[[ "sha256:$(sha256_file "$manifest_zip")" == "$manifest_digest" ]] || { echo 'error: manifest ZIP digest mismatch' >&2; exit 1; }
[[ "$(unzip -Z1 "$manifest_zip")" == "$manifest_name.json" ]] || { echo 'error: unexpected manifest ZIP members' >&2; exit 1; }
unzip -p "$manifest_zip" "$manifest_name.json" > "$manifest_file"
validate_candidate_manifest_json "$manifest_file"
jq -e --arg source "$source_sha" --arg policy "$policy" --arg run "$run" --arg attempt "$attempt" '
  .sourceCommit == $source and .policyRevision == $policy and
  .runId == $run and .runAttempt == $attempt and
  all(.archives[]; .runAttempt == $attempt and
    .attestationIdentity == "https://harnlang.com/attestations/release-archive/v1") and
  all(.archives | to_entries[];
    if (.key | contains("apple-darwin")) then
      .value.signingStatus == "signed" and .value.notarizationStatus == "notarized"
    else .value.signingStatus == "not_applicable" and .value.notarizationStatus == "not_applicable" end)
' "$manifest_file" >/dev/null
while IFS= read -r target; do
  archive="$(archive_for_target "$target")"
  # Inventory identity and immutable archive digest bind transport to the manifest.
  artifact_id="$(jq -er --arg name "harn-$target" '
    [.[] | select(.name == $name)] | if length == 1 then .[0].id | tostring else error("ambiguous or missing archive artifact") end
  ' "$scratch/artifacts.json")"
  download_artifact "$artifact_id" "harn-$target" "$output/$archive" "$archive"
  expected="$(jq -er --arg target "$target" '.archives[$target].sha256' "$manifest_file")"
  [[ "$(sha256_file "$output/$archive")" == "$expected" ]] || { echo "error: archive digest mismatch: $archive" >&2; exit 1; }
done < <(jq -r '.archives | keys[]' "$manifest_file")
echo "Verified five archive bytes from candidate $source_sha, producer $run/$attempt, original policy $policy"
