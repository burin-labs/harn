#!/usr/bin/env bash
set -euo pipefail

script_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=scripts/lib/candidate_archive_contract.sh
source "$script_dir/lib/candidate_archive_contract.sh"
# shellcheck source=scripts/lib/release_version.sh
source "$script_dir/lib/release_version.sh"

usage() {
  cat <<'EOF'
Usage: scripts/verify_candidate_archive_promotion.sh \
  --manifest PATH --tag vX.Y.Z --repo OWNER/REPO --artifacts-dir DIR \
  [--expected-source-commit SHA] [--expected-policy-revision SHA] \
  [--expected-run-id ID]

Verifies that promoted release archives match the candidate archive manifest and
that the release tag peels to the manifest source commit.
EOF
}

manifest=""
tag=""
repo=""
artifacts_dir=""
expected_source_commit=""
expected_policy_revision=""
expected_run_id=""

while (($#)); do
  case "$1" in
    --manifest) manifest="${2:-}"; shift 2 ;;
    --tag) tag="${2:-}"; shift 2 ;;
    --repo) repo="${2:-}"; shift 2 ;;
    --artifacts-dir) artifacts_dir="${2:-}"; shift 2 ;;
    --expected-source-commit) expected_source_commit="${2:-}"; shift 2 ;;
    --expected-policy-revision) expected_policy_revision="${2:-}"; shift 2 ;;
    --expected-run-id) expected_run_id="${2:-}"; shift 2 ;;
    -h|--help) usage; exit 0 ;;
    *) echo "error: unknown argument: $1" >&2; usage >&2; exit 2 ;;
  esac
done

if [[ -z "$manifest" || -z "$tag" || -z "$repo" || -z "$artifacts_dir" ]]; then
  echo "error: --manifest, --tag, --repo, and --artifacts-dir are required" >&2
  usage >&2
  exit 2
fi
if ! release_tag_is_canonical "$tag"; then
  echo "error: expected canonical release tag, got '$tag'" >&2
  exit 2
fi
if [[ ! "$repo" =~ ^[^/]+/[^/]+$ ]]; then
  echo "error: expected repo OWNER/REPO, got '$repo'" >&2
  exit 2
fi
if [[ ! -f "$manifest" ]]; then
  echo "error: manifest does not exist: $manifest" >&2
  exit 1
fi
if [[ ! -d "$artifacts_dir" ]]; then
  echo "error: artifacts directory does not exist: $artifacts_dir" >&2
  exit 1
fi
if [[ -n "$expected_source_commit" && ! "$expected_source_commit" =~ ^[0-9a-f]{40}$ ]]; then
  echo "error: --expected-source-commit must be a 40-character lowercase hex SHA" >&2
  exit 2
fi
if [[ -n "$expected_policy_revision" && ! "$expected_policy_revision" =~ ^[0-9a-f]{40}$ ]]; then
  echo "error: --expected-policy-revision must be a 40-character lowercase hex SHA" >&2
  exit 2
fi
if [[ -n "$expected_run_id" && ! "$expected_run_id" =~ ^[0-9]+$ ]]; then
  echo "error: --expected-run-id must be a decimal string" >&2
  exit 2
fi

if ! validate_candidate_manifest_json "$manifest"; then
  exit 1
fi

manifest_source_commit="$(jq -r '.sourceCommit' "$manifest")"
manifest_policy_revision="$(jq -r '.policyRevision' "$manifest")"
manifest_run_id="$(jq -r '.runId' "$manifest")"
manifest_run_attempt="$(jq -r '.runAttempt' "$manifest")"

if [[ -n "$expected_source_commit" && "$manifest_source_commit" != "$expected_source_commit" ]]; then
  echo "error: manifest sourceCommit $manifest_source_commit does not match expected $expected_source_commit" >&2
  exit 1
fi
if [[ -n "$expected_policy_revision" && "$manifest_policy_revision" != "$expected_policy_revision" ]]; then
  echo "error: manifest policyRevision $manifest_policy_revision does not match expected $expected_policy_revision" >&2
  exit 1
fi
if [[ -n "$expected_run_id" && "$manifest_run_id" != "$expected_run_id" ]]; then
  echo "error: manifest runId $manifest_run_id does not match expected $expected_run_id" >&2
  exit 1
fi

tag_ref_json="$(gh api "repos/$repo/git/ref/tags/$tag")"
tag_object_type="$(jq -r '.object.type // empty' <<<"$tag_ref_json")"
tag_object_sha="$(jq -r '.object.sha // empty' <<<"$tag_ref_json")"
if [[ "$tag_object_type" != "tag" || ! "$tag_object_sha" =~ ^[0-9a-f]{40}$ ]]; then
  echo "error: $tag is not an annotated tag" >&2
  exit 1
fi

tag_object_json="$(gh api "repos/$repo/git/tags/$tag_object_sha")"
tag_source_commit="$(jq -r '.object.sha // empty' <<<"$tag_object_json")"
if [[ "$(jq -r '.object.type // empty' <<<"$tag_object_json")" != "commit" ||
      ! "$tag_source_commit" =~ ^[0-9a-f]{40}$ ]]; then
  echo "error: $tag is not an annotated tag pointing directly to a commit" >&2
  exit 1
fi
if [[ "$tag_source_commit" != "$manifest_source_commit" ]]; then
  echo "error: $tag peels to $tag_source_commit but manifest binds $manifest_source_commit" >&2
  exit 1
fi

mapfile -t downloaded_archives < <(
  find "$artifacts_dir" -maxdepth 1 -type f \
    \( -name 'harn-*.tar.gz' -o -name 'harn-*.zip' \) \
    -exec basename {} \; | LC_ALL=C sort
)
if [[ "${downloaded_archives[*]}" != "$(printf '%s\n' "${EXPECTED_RELEASE_ARCHIVES[@]}" | LC_ALL=C sort | paste -sd ' ' -)" ]]; then
  echo "error: artifacts directory does not contain the exact five-target contract" >&2
  printf 'observed: %s\n' "${downloaded_archives[*]:-<none>}" >&2
  exit 1
fi

while IFS= read -r target; do
  [[ -n "$target" ]] || continue
  archive="$(jq -r --arg target "$target" '.archives[$target].archive' "$manifest")"
  expected_digest="$(jq -r --arg target "$target" '.archives[$target].sha256' "$manifest")"
  signing_status="$(jq -r --arg target "$target" '.archives[$target].signingStatus' "$manifest")"
  notarization_status="$(jq -r --arg target "$target" '.archives[$target].notarizationStatus' "$manifest")"
  attestation_identity="$(jq -r --arg target "$target" '.archives[$target].attestationIdentity' "$manifest")"
  entry_run_id="$(jq -r --arg target "$target" '.archives[$target].runId' "$manifest")"
  entry_run_attempt="$(jq -r --arg target "$target" '.archives[$target].runAttempt' "$manifest")"

  if [[ "$signing_status" != "signed" && "$signing_status" != "not_applicable" ]]; then
    echo "error: manifest entry for $target has invalid signingStatus: $signing_status" >&2
    exit 1
  fi
  if [[ "$notarization_status" != "notarized" && "$notarization_status" != "not_applicable" ]]; then
    echo "error: manifest entry for $target has invalid notarizationStatus: $notarization_status" >&2
    exit 1
  fi
  if [[ -z "$attestation_identity" ]]; then
    echo "error: manifest entry for $target is missing attestationIdentity" >&2
    exit 1
  fi
  if [[ "$entry_run_id" != "$manifest_run_id" ]]; then
    echo "error: manifest entry for $target runId does not match manifest runId" >&2
    exit 1
  fi
  if ((10#$entry_run_attempt > 10#$manifest_run_attempt)); then
    echo "error: manifest entry for $target was produced after the manifest attempt" >&2
    exit 1
  fi

  path="$artifacts_dir/$archive"
  if [[ ! -f "$path" ]]; then
    echo "error: missing promoted archive: $archive" >&2
    exit 1
  fi
  observed_digest="$(sha256_file "$path")"
  if [[ "$observed_digest" != "$expected_digest" ]]; then
    echo "error: archive digest mismatch for $archive (observed $observed_digest, expected $expected_digest)" >&2
    exit 1
  fi

  echo "verified candidate archive promotion: $archive -> $tag@$manifest_source_commit"
done < <(jq -r '.archives | keys[]' "$manifest" | LC_ALL=C sort)

echo "verified candidate archive promotion manifest against $tag@$manifest_source_commit"
