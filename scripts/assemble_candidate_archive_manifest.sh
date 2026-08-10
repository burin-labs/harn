#!/usr/bin/env bash
set -euo pipefail

script_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=scripts/lib/candidate_archive_contract.sh
source "$script_dir/lib/candidate_archive_contract.sh"

usage() {
  cat <<'EOF'
Usage: scripts/assemble_candidate_archive_manifest.sh \
  --output PATH --source-commit SHA --policy-revision SHA --run-id ID --run-attempt N \
  --receipts-dir DIR [--workflow-url URL]

Reads single-target candidate archive receipts and writes the aggregate manifest.
EOF
}

output=""
source_commit=""
policy_revision=""
run_id=""
run_attempt=""
receipts_dir=""
workflow_url=""

while (($#)); do
  case "$1" in
    --output) output="${2:-}"; shift 2 ;;
    --source-commit) source_commit="${2:-}"; shift 2 ;;
    --policy-revision) policy_revision="${2:-}"; shift 2 ;;
    --run-id) run_id="${2:-}"; shift 2 ;;
    --run-attempt) run_attempt="${2:-}"; shift 2 ;;
    --receipts-dir) receipts_dir="${2:-}"; shift 2 ;;
    --workflow-url) workflow_url="${2:-}"; shift 2 ;;
    -h|--help) usage; exit 0 ;;
    *) echo "error: unknown argument: $1" >&2; usage >&2; exit 2 ;;
  esac
done

if [[ -z "$output" || -z "$source_commit" || -z "$policy_revision" ||
      -z "$run_id" || -z "$run_attempt" || -z "$receipts_dir" ]]; then
  echo "error: --output, --source-commit, --policy-revision, --run-id, --run-attempt, and --receipts-dir are required" >&2
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
if [[ ! "$run_id" =~ ^[0-9]+$ || ! "$run_attempt" =~ ^[0-9]+$ ]]; then
  echo "error: --run-id and --run-attempt must be decimal strings" >&2
  exit 2
fi
if [[ ! -d "$receipts_dir" ]]; then
  echo "error: receipts directory does not exist: $receipts_dir" >&2
  exit 1
fi
if [[ -e "$output" ]]; then
  echo "error: refusing to overwrite existing manifest: $output" >&2
  exit 1
fi

mapfile -t receipt_files < <(
  find "$receipts_dir" -maxdepth 1 -type f -name '*.json' -print | LC_ALL=C sort
)
if ((${#receipt_files[@]} != 5)); then
  echo "error: expected exactly five receipt files in $receipts_dir (found ${#receipt_files[@]})" >&2
  exit 1
fi

declare -A seen_targets=()
receipt_schema="$CANDIDATE_RECEIPT_SCHEMA"
expected_targets_json="$(candidate_archive_expected_targets_json)"

for receipt_file in "${receipt_files[@]}"; do
  if ! jq -e \
    --arg schema "$receipt_schema" \
    --arg expectedSource "$source_commit" \
    --arg expectedPolicy "$policy_revision" \
    --arg expectedRunId "$run_id" \
    --arg expectedRunAttempt "$run_attempt" \
    '
    .schemaVersion == $schema and
    .sourceCommit == $expectedSource and
    .policyRevision == $expectedPolicy and
    .runId == $expectedRunId and
    .runAttempt == $expectedRunAttempt and
    (.target | type == "string" and length > 0) and
    (.archive | type == "string" and length > 0) and
    (.sha256 | type == "string" and test("^[0-9a-f]{64}$")) and
    (.signingStatus | IN("signed", "not_applicable")) and
    (.notarizationStatus | IN("notarized", "not_applicable")) and
    (.attestationIdentity | type == "string" and length > 0)
    ' "$receipt_file" >/dev/null; then
    echo "error: receipt does not match the expected candidate archive contract: $receipt_file" >&2
    exit 1
  fi

  target="$(jq -r '.target' "$receipt_file")"
  if [[ -n "${seen_targets[$target]+x}" ]]; then
    echo "error: duplicate receipt target in $receipts_dir: $target" >&2
    exit 1
  fi
  seen_targets["$target"]=1

  expected_archive="$(archive_for_target "$target")"
  archive="$(jq -r '.archive' "$receipt_file")"
  if [[ "$archive" != "$expected_archive" ]]; then
    echo "error: receipt $receipt_file binds unexpected archive for $target" >&2
    exit 1
  fi
done

mapfile -t observed_targets < <(printf '%s\n' "${!seen_targets[@]}" | LC_ALL=C sort)
mapfile -t expected_targets < <(jq -r '.[]' <<<"$expected_targets_json" | LC_ALL=C sort)
if [[ "${observed_targets[*]}" != "${expected_targets[*]}" ]]; then
  echo "error: receipt target set is not the exact five-target contract" >&2
  printf 'observed: %s\n' "${observed_targets[*]:-<none>}" >&2
  exit 1
fi

archives_object="$(
  jq -s 'map({
    key: .target,
    value: {
      archive: .archive,
      sha256: .sha256,
      signingStatus: .signingStatus,
      notarizationStatus: .notarizationStatus,
      attestationIdentity: .attestationIdentity,
      runId: .runId,
      runAttempt: .runAttempt
    }
  }) | from_entries' "${receipt_files[@]}"
)"

mkdir -p "$(dirname "$output")"
jq -n \
  --arg schemaVersion "$CANDIDATE_ARCHIVE_SCHEMA" \
  --arg sourceCommit "$source_commit" \
  --arg policyRevision "$policy_revision" \
  --arg runId "$run_id" \
  --arg runAttempt "$run_attempt" \
  --arg workflowUrl "$workflow_url" \
  --argjson archives "$archives_object" \
  '
  {
    schemaVersion: $schemaVersion,
    sourceCommit: $sourceCommit,
    policyRevision: $policyRevision,
    runId: $runId,
    runAttempt: $runAttempt,
    archives: $archives
  }
  + (if ($workflowUrl | length) > 0 then {workflowUrl: $workflowUrl} else {} end)
  ' >"$output"

if ! validate_candidate_manifest_json "$output"; then
  exit 1
fi

echo "assembled candidate archive manifest: $output"
