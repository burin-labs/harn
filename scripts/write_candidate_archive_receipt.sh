#!/usr/bin/env bash
set -euo pipefail

script_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=scripts/lib/candidate_archive_contract.sh
source "$script_dir/lib/candidate_archive_contract.sh"

usage() {
  cat <<'EOF'
Usage: scripts/write_candidate_archive_receipt.sh \
  --output PATH --source-commit SHA --target TRIPLE --archive NAME --sha256 HEX \
  --policy-revision SHA --signing-status STATUS --notarization-status STATUS \
  --attestation-identity STR --run-id ID --run-attempt N \
  [--workflow-url URL] [--build-inputs-json JSON]

Writes a single-target candidate archive receipt JSON.
EOF
}

output=""
source_commit=""
target=""
archive=""
sha256=""
policy_revision=""
signing_status=""
notarization_status=""
attestation_identity=""
run_id=""
run_attempt=""
workflow_url=""
build_inputs_json=""

while (($#)); do
  case "$1" in
    --output) output="${2:-}"; shift 2 ;;
    --source-commit) source_commit="${2:-}"; shift 2 ;;
    --target) target="${2:-}"; shift 2 ;;
    --archive) archive="${2:-}"; shift 2 ;;
    --sha256) sha256="${2:-}"; shift 2 ;;
    --policy-revision) policy_revision="${2:-}"; shift 2 ;;
    --signing-status) signing_status="${2:-}"; shift 2 ;;
    --notarization-status) notarization_status="${2:-}"; shift 2 ;;
    --attestation-identity) attestation_identity="${2:-}"; shift 2 ;;
    --run-id) run_id="${2:-}"; shift 2 ;;
    --run-attempt) run_attempt="${2:-}"; shift 2 ;;
    --workflow-url) workflow_url="${2:-}"; shift 2 ;;
    --build-inputs-json) build_inputs_json="${2:-}"; shift 2 ;;
    -h|--help) usage; exit 0 ;;
    *) echo "error: unknown argument: $1" >&2; usage >&2; exit 2 ;;
  esac
done

if [[ -z "$output" || -z "$source_commit" || -z "$target" || -z "$archive" ||
      -z "$sha256" || -z "$policy_revision" || -z "$signing_status" ||
      -z "$notarization_status" || -z "$attestation_identity" ||
      -z "$run_id" || -z "$run_attempt" ]]; then
  echo "error: all required receipt fields must be provided" >&2
  usage >&2
  exit 2
fi

if [[ ! "$source_commit" =~ ^[0-9a-f]{40}$ ]]; then
  echo "error: --source-commit must be a 40-character lowercase hex SHA" >&2
  exit 2
fi
if [[ ! "$policy_revision" =~ ^[0-9a-f]{40}$ ]]; then
  echo "error: --policy-revision must be a 40-character lowercase hex SHA" >&2
  exit 2
fi
if [[ ! "$sha256" =~ ^[0-9a-f]{64}$ ]]; then
  echo "error: --sha256 must be a 64-character lowercase hex digest" >&2
  exit 2
fi
if [[ ! "$run_id" =~ ^[0-9]+$ || ! "$run_attempt" =~ ^[0-9]+$ ]]; then
  echo "error: --run-id and --run-attempt must be decimal strings" >&2
  exit 2
fi
if [[ "$signing_status" != "signed" && "$signing_status" != "not_applicable" ]]; then
  echo "error: --signing-status must be signed or not_applicable" >&2
  exit 2
fi
if [[ "$notarization_status" != "notarized" && "$notarization_status" != "not_applicable" ]]; then
  echo "error: --notarization-status must be notarized or not_applicable" >&2
  exit 2
fi
if [[ -z "$attestation_identity" ]]; then
  echo "error: --attestation-identity must be non-empty" >&2
  exit 2
fi

expected_archive="$(archive_for_target "$target")"
if [[ "$archive" != "$expected_archive" ]]; then
  echo "error: archive $archive does not match target $target (expected $expected_archive)" >&2
  exit 2
fi
if [[ "$target" != "$(target_for_archive "$archive")" ]]; then
  echo "error: target/archive binding is inconsistent" >&2
  exit 2
fi

archive_is_expected=false
for expected in "${EXPECTED_RELEASE_ARCHIVES[@]}"; do
  if [[ "$archive" == "$expected" ]]; then
    archive_is_expected=true
    break
  fi
done
if [[ "$archive_is_expected" != "true" ]]; then
  echo "error: archive $archive is not one of the five canonical release archives" >&2
  exit 2
fi

if [[ -n "$build_inputs_json" ]] &&
   ! jq -e . >/dev/null <<<"$build_inputs_json"; then
  echo "error: --build-inputs-json must be valid JSON" >&2
  exit 2
fi

if [[ -e "$output" ]]; then
  echo "error: refusing to overwrite existing receipt: $output" >&2
  exit 1
fi

mkdir -p "$(dirname "$output")"

jq -n \
  --arg schema "$CANDIDATE_RECEIPT_SCHEMA" \
  --arg sourceCommit "$source_commit" \
  --arg target "$target" \
  --arg archive "$archive" \
  --arg sha256 "$sha256" \
  --arg policyRevision "$policy_revision" \
  --arg signingStatus "$signing_status" \
  --arg notarizationStatus "$notarization_status" \
  --arg attestationIdentity "$attestation_identity" \
  --arg runId "$run_id" \
  --arg runAttempt "$run_attempt" \
  --arg workflowUrl "$workflow_url" \
  --argjson buildInputs "${build_inputs_json:-null}" \
  '
  {
    schemaVersion: $schema,
    sourceCommit: $sourceCommit,
    target: $target,
    archive: $archive,
    sha256: $sha256,
    policyRevision: $policyRevision,
    signingStatus: $signingStatus,
    notarizationStatus: $notarizationStatus,
    attestationIdentity: $attestationIdentity,
    runId: $runId,
    runAttempt: $runAttempt
  }
  + (if ($workflowUrl | length) > 0 then {workflowUrl: $workflowUrl} else {} end)
  + (if $buildInputs != null then {buildInputs: $buildInputs} else {} end)
  ' >"$output"

echo "wrote candidate archive receipt: $output ($target -> $archive)"
