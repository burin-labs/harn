#!/usr/bin/env bash
# Write the candidate manifest (burin-labs.candidate_manifest.v1) for one
# release candidate run: the five target archives, from their per-target
# receipts, plus the release files built beside them. Every digest is
# recomputed from the bytes on disk, so the manifest can only record files
# that exist and match their receipts. burin-labs/.github owns the schema and
# validates the result; this script owns what Harn's release contains.
set -euo pipefail

script_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=scripts/lib/candidate_archive_contract.sh
source "$script_dir/lib/candidate_archive_contract.sh"

usage() {
  cat <<'EOF'
Usage: scripts/assemble_candidate_manifest.sh \
  --output PATH --repository OWNER/NAME --source-commit SHA --policy-revision SHA \
  --run-id ID --run-attempt N --receipts-dir DIR --archives-dir DIR --release-files-dir DIR

Reads the five single-target candidate receipts, checks each archive in
--archives-dir against its receipt, and writes the candidate manifest listing
those archives and the release files in --release-files-dir (SHA256SUMS,
release-assets.json, release-notes.md).
EOF
}

output=""
repository=""
source_commit=""
policy_revision=""
run_id=""
run_attempt=""
receipts_dir=""
archives_dir=""
release_files_dir=""

while (($#)); do
  case "$1" in
    --output) output="${2:-}"; shift 2 ;;
    --repository) repository="${2:-}"; shift 2 ;;
    --source-commit) source_commit="${2:-}"; shift 2 ;;
    --policy-revision) policy_revision="${2:-}"; shift 2 ;;
    --run-id) run_id="${2:-}"; shift 2 ;;
    --run-attempt) run_attempt="${2:-}"; shift 2 ;;
    --receipts-dir) receipts_dir="${2:-}"; shift 2 ;;
    --archives-dir) archives_dir="${2:-}"; shift 2 ;;
    --release-files-dir) release_files_dir="${2:-}"; shift 2 ;;
    -h|--help) usage; exit 0 ;;
    *) echo "error: unknown argument: $1" >&2; usage >&2; exit 2 ;;
  esac
done

if [[ -z "$output" || -z "$repository" || -z "$source_commit" || -z "$policy_revision" ||
      -z "$run_id" || -z "$run_attempt" || -z "$receipts_dir" || -z "$archives_dir" ||
      -z "$release_files_dir" ]]; then
  echo "error: every argument is required" >&2
  usage >&2
  exit 2
fi
if [[ ! "$repository" =~ ^[A-Za-z0-9_.-]+/[A-Za-z0-9_.-]+$ ]]; then
  echo "error: --repository must be owner/name" >&2
  exit 2
fi
if [[ ! "$source_commit" =~ ^[0-9a-f]{40}$ || ! "$policy_revision" =~ ^[0-9a-f]{40}$ ]]; then
  echo "error: --source-commit and --policy-revision must be 40-character lowercase hex SHAs" >&2
  exit 2
fi
if [[ ! "$run_id" =~ ^[1-9][0-9]*$ || ! "$run_attempt" =~ ^[1-9][0-9]*$ ]]; then
  echo "error: --run-id and --run-attempt must be positive decimal strings" >&2
  exit 2
fi
for dir in "$receipts_dir" "$archives_dir" "$release_files_dir"; do
  if [[ ! -d "$dir" ]]; then
    echo "error: directory does not exist: $dir" >&2
    exit 1
  fi
done
if [[ -e "$output" ]]; then
  echo "error: refusing to overwrite existing manifest: $output" >&2
  exit 1
fi

entries=()
for target in $(candidate_archive_expected_targets_json | jq -r '.[]'); do
  receipt="$receipts_dir/${target}.json"
  if [[ ! -f "$receipt" ]]; then
    echo "error: missing candidate receipt for $target: $receipt" >&2
    exit 1
  fi
  if ! jq -e \
    --arg schema "$CANDIDATE_RECEIPT_SCHEMA" \
    --arg source "$source_commit" \
    --arg policy "$policy_revision" \
    --arg runId "$run_id" \
    --arg runAttempt "$run_attempt" \
    --arg target "$target" \
    --arg predicate "$RELEASE_ARCHIVE_PREDICATE_TYPE" \
    '
    .schemaVersion == $schema and
    .sourceCommit == $source and
    .policyRevision == $policy and
    .runId == $runId and
    (.runAttempt | type == "string" and test("^[1-9][0-9]*$")) and
    ((.runAttempt | tonumber) <= ($runAttempt | tonumber)) and
    .target == $target and
    (.sha256 | type == "string" and test("^[0-9a-f]{64}$")) and
    (.signingStatus | IN("signed", "not_applicable")) and
    (.notarizationStatus | IN("notarized", "not_applicable")) and
    .attestationIdentity == $predicate
    ' "$receipt" >/dev/null; then
    echo "error: receipt does not describe $target from this run at $source_commit: $receipt" >&2
    exit 1
  fi
  archive="$(jq -r '.archive' "$receipt")"
  if [[ "$archive" != "$(archive_for_target "$target")" ]]; then
    echo "error: receipt for $target names archive $archive" >&2
    exit 1
  fi
  if [[ ! -f "$archives_dir/$archive" ]]; then
    echo "error: missing candidate archive $archive in $archives_dir" >&2
    exit 1
  fi
  actual="$(sha256_file "$archives_dir/$archive")"
  if [[ "$actual" != "$(jq -r '.sha256' "$receipt")" ]]; then
    echo "error: $archive has sha256 $actual, not the $(jq -r '.sha256' "$receipt") its receipt records" >&2
    exit 1
  fi
  entries+=("$(jq -c --arg artifact "harn-$target" '{
    kind: "archive",
    target: .target,
    artifact: $artifact,
    file: .archive,
    sha256: .sha256,
    attestationPredicateType: .attestationIdentity,
    signingStatus: .signingStatus,
    notarizationStatus: .notarizationStatus
  }' "$receipt")")
done

# kind:file for the release files built beside the archives.
for release_file in checksums:SHA256SUMS asset-index:release-assets.json notes:release-notes.md; do
  kind="${release_file%%:*}"
  file="${release_file#*:}"
  if [[ ! -s "$release_files_dir/$file" ]]; then
    echo "error: missing or empty release file $file in $release_files_dir" >&2
    exit 1
  fi
  entries+=("$(jq -nc \
    --arg kind "$kind" \
    --arg artifact "$RELEASE_FILES_ARTIFACT" \
    --arg file "$file" \
    --arg sha256 "$(sha256_file "$release_files_dir/$file")" \
    --arg predicate "$RELEASE_ARCHIVE_PREDICATE_TYPE" \
    '{
      kind: $kind,
      artifact: $artifact,
      file: $file,
      sha256: $sha256,
      attestationPredicateType: $predicate,
      signingStatus: "not_applicable",
      notarizationStatus: "not_applicable"
    }')")
done

mkdir -p "$(dirname "$output")"
printf '%s\n' "${entries[@]}" | jq -s \
  --arg schemaVersion "$CANDIDATE_MANIFEST_SCHEMA" \
  --arg repository "$repository" \
  --arg sourceCommit "$source_commit" \
  --arg runId "$run_id" \
  --arg runAttempt "$run_attempt" \
  '{
    schemaVersion: $schemaVersion,
    repository: $repository,
    sourceCommit: $sourceCommit,
    runId: $runId,
    runAttempt: $runAttempt,
    artifacts: .
  }' >"$output"

echo "assembled candidate manifest: $output"
