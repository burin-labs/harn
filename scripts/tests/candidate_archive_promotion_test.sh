#!/usr/bin/env bash
set -euo pipefail

root="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
verifier="$root/scripts/verify_candidate_archive_promotion.sh"
writer="$root/scripts/write_candidate_archive_receipt.sh"
assembler="$root/scripts/assemble_candidate_archive_manifest.sh"
discoverer="$root/scripts/discover_candidate_archive_run.sh"
contract="$root/scripts/lib/candidate_archive_contract.sh"
tmp="$(mktemp -d)"
trap 'rm -rf "$tmp"' EXIT

readonly repo="burin-labs/harn"
readonly tag="v0.10.40"
readonly source_commit="1111111111111111111111111111111111111111"
readonly policy_revision="2222222222222222222222222222222222222222"
readonly run_id="9001"
readonly run_attempt="1"
readonly workflow_url="https://github.com/burin-labs/harn/actions/runs/$run_id"
readonly archives=(
  harn-aarch64-apple-darwin.tar.gz
  harn-aarch64-unknown-linux-gnu.tar.gz
  harn-x86_64-apple-darwin.tar.gz
  harn-x86_64-pc-windows-msvc.zip
  harn-x86_64-unknown-linux-gnu.tar.gz
)

for script in "$verifier" "$writer" "$assembler" "$discoverer" "$contract"; do
  if [[ ! -f "$script" ]]; then
    echo "FAIL: missing script: $script" >&2
    exit 1
  fi
done

# shellcheck source=scripts/lib/candidate_archive_contract.sh
source "$contract"

# macOS release jobs use /bin/bash 3.2. Associative-array compound assignment
# of keys like harn-*.tar.gz is arithmetic under `set -u` there; keep the
# contract sourcable on that interpreter.
if [[ -x /bin/bash ]]; then
  /bin/bash -c '
    set -euo pipefail
    # shellcheck disable=SC1090
    source "$1"
    target_for_archive harn-x86_64-apple-darwin.tar.gz >/dev/null
    archive_for_target x86_64-pc-windows-msvc >/dev/null
  ' bash "$contract"
fi

sha256_file_local() {
  sha256_file "$1"
}

mkdir -p "$tmp/bin"
cat >"$tmp/bin/gh" <<'MOCK'
#!/usr/bin/env bash
set -euo pipefail

case "${1:-} ${2:-}" in
  "api repos/"*)
    if [[ "$2" == *"/git/ref/tags/"* ]]; then
      cat "$FIXTURE_DIR/tag-ref.json"
      exit 0
    fi
    if [[ "$2" == *"/actions/runs/"*"/artifacts" ]]; then
      run_id="${2#*runs/}"
      run_id="${run_id%%/*}"
      cat "$FIXTURE_DIR/runs/$run_id/artifacts.json"
      exit 0
    fi
    if [[ "$2" == *"/actions/runs?"* ]]; then
      cat "$FIXTURE_DIR/runs-index.json"
      exit 0
    fi
    cat "$FIXTURE_DIR/tag-object.json"
    ;;
  *)
    echo "unexpected gh invocation: $*" >&2
    exit 2
    ;;
esac
MOCK
chmod +x "$tmp/bin/gh"

expect_success() {
  local name="$1"
  shift
  if ! "$@" >"$tmp/$name.out" 2>"$tmp/$name.err"; then
    cat "$tmp/$name.out" "$tmp/$name.err" >&2
    echo "FAIL: expected success: $name" >&2
    exit 1
  fi
}

expect_failure() {
  local name="$1"
  shift
  if "$@" >"$tmp/$name.out" 2>"$tmp/$name.err"; then
    cat "$tmp/$name.out" "$tmp/$name.err" >&2
    echo "FAIL: expected failure: $name" >&2
    exit 1
  fi
}

expect_output() {
  local name="$1"
  local expected="$2"
  shift 2
  expect_success "$name" "$@"
  if [[ "$(<"$tmp/$name.out")" != "$expected" ]]; then
    echo "FAIL: $name expected '$expected', got '$(<"$tmp/$name.out")'" >&2
    exit 1
  fi
}

write_receipt() {
  local target="$1"
  local archive="$2"
  local digest="$3"
  local attestation_identity="${4:-burin-labs/harn/.github/workflows/build-release-binaries.yml@$policy_revision}"
  local signing_status="${5:-signed}"
  local notarization_status="${6:-not_applicable}"
  local producer_attempt="${7:-$run_attempt}"
  "$writer" \
    --output "$fixture/receipts/$target.json" \
    --source-commit "$source_commit" \
    --target "$target" \
    --archive "$archive" \
    --sha256 "$digest" \
    --policy-revision "$policy_revision" \
    --signing-status "$signing_status" \
    --notarization-status "$notarization_status" \
    --attestation-identity "$attestation_identity" \
    --run-id "$run_id" \
    --run-attempt "$producer_attempt" \
    --workflow-url "$workflow_url"
}

new_fixture() {
  fixture="$tmp/fixture"
  rm -rf "$fixture"
  mkdir -p "$fixture/artifacts" "$fixture/receipts" "$fixture/runs/$run_id"
  jq -n --arg sha aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa \
    '{object: {type: "tag", sha: $sha}}' >"$fixture/tag-ref.json"
  jq -n --arg source "$source_commit" \
    '{object: {type: "commit", sha: $source}}' \
    >"$fixture/tag-object.json"
  jq -n \
    --arg run "$run_id" \
    --arg source "$source_commit" \
    --arg manifest "candidate-archive-manifest-$source_commit" \
    '{
      workflow_runs: [{
        id: ($run | tonumber),
        conclusion: "success",
        path: ".github/workflows/build-release-binaries.yml",
        display_title: ("candidate " + $source),
        name: ("Build release binaries " + $source)
      }]
    }' >"$fixture/runs-index.json"
  jq -n \
    --arg manifest "candidate-archive-manifest-$source_commit" \
    '{artifacts: [{name: $manifest}]}' \
    >"$fixture/runs/$run_id/artifacts.json"

  local archive target digest
  for archive in "${archives[@]}"; do
    printf 'fixture bytes for %s\n' "$archive" >"$fixture/artifacts/$archive"
    target="$(target_for_archive "$archive")"
    digest="$(sha256_file_local "$fixture/artifacts/$archive")"
    write_receipt "$target" "$archive" "$digest"
  done

  "$assembler" \
    --output "$fixture/manifest.json" \
    --source-commit "$source_commit" \
    --policy-revision "$policy_revision" \
    --run-id "$run_id" \
    --run-attempt "$run_attempt" \
    --receipts-dir "$fixture/receipts" \
    --workflow-url "$workflow_url"
}

run_verifier() {
  FIXTURE_DIR="$fixture" PATH="$tmp/bin:$PATH" \
    "$verifier" \
    --manifest "$fixture/manifest.json" \
    --tag "$tag" \
    --repo "$repo" \
    --artifacts-dir "$fixture/artifacts" \
    "$@"
}

new_fixture
expect_success primary run_verifier

expect_output discover "$run_id" \
  env FIXTURE_DIR="$fixture" PATH="$tmp/bin:$PATH" \
  "$discoverer" --repo "$repo" --source-commit "$source_commit"

# Release recovery may certify the same immutable source more than once. The
# newest successful run is authoritative; an expired newer artifact is not.
jq --arg source "$source_commit" \
  '.workflow_runs += [{
    id: 9002,
    conclusion: "success",
    path: ".github/workflows/build-release-binaries.yml",
    display_title: ("candidate retry " + $source),
    name: ("Build release binaries " + $source)
  }]' "$fixture/runs-index.json" >"$fixture/runs-index.tmp"
mv "$fixture/runs-index.tmp" "$fixture/runs-index.json"
mkdir -p "$fixture/runs/9002"
jq -n --arg manifest "candidate-archive-manifest-$source_commit" \
  '{artifacts: [{name: $manifest, expired: false}]}' \
  >"$fixture/runs/9002/artifacts.json"
expect_output discover_prefers_newest 9002 \
  env FIXTURE_DIR="$fixture" PATH="$tmp/bin:$PATH" \
  "$discoverer" --repo "$repo" --source-commit "$source_commit"

jq --arg source "$source_commit" \
  '.workflow_runs += [{
    id: 9003,
    conclusion: "success",
    path: ".github/workflows/build-release-binaries.yml",
    display_title: ("expired candidate retry " + $source),
    name: ("Build release binaries " + $source)
  }]' "$fixture/runs-index.json" >"$fixture/runs-index.tmp"
mv "$fixture/runs-index.tmp" "$fixture/runs-index.json"
mkdir -p "$fixture/runs/9003"
jq -n --arg manifest "candidate-archive-manifest-$source_commit" \
  '{artifacts: [{name: $manifest, expired: true}]}' \
  >"$fixture/runs/9003/artifacts.json"
expect_output discover_skips_expired 9002 \
  env FIXTURE_DIR="$fixture" PATH="$tmp/bin:$PATH" \
  "$discoverer" --repo "$repo" --source-commit "$source_commit"

jq '.object.sha = "3333333333333333333333333333333333333333"' \
  "$fixture/tag-object.json" >"$fixture/tag-object.tmp"
mv "$fixture/tag-object.tmp" "$fixture/tag-object.json"
expect_failure wrong_tag_source run_verifier

new_fixture
jq '.object.sha = "3333333333333333333333333333333333333333"' \
  "$fixture/tag-object.json" >"$fixture/tag-object.tmp"
mv "$fixture/tag-object.tmp" "$fixture/tag-object.json"
expect_failure wrong_tag_source_with_expected \
  run_verifier --expected-source-commit "$source_commit"

new_fixture
printf 'tamper\n' >>"$fixture/artifacts/harn-aarch64-apple-darwin.tar.gz"
expect_failure wrong_archive_digest run_verifier

new_fixture
rm "$fixture/artifacts/harn-aarch64-apple-darwin.tar.gz"
expect_failure incomplete_target_set run_verifier

new_fixture
expect_failure wrong_policy_revision \
  run_verifier --expected-policy-revision 3333333333333333333333333333333333333333

new_fixture
expect_failure wrong_run_id run_verifier --expected-run-id 42

new_fixture
rm -f "$fixture/manifest.json"
rm -f "$fixture/receipts/aarch64-apple-darwin.json"
write_receipt \
  aarch64-apple-darwin \
  harn-aarch64-apple-darwin.tar.gz \
  "$(sha256_file_local "$fixture/artifacts/harn-aarch64-apple-darwin.tar.gz")" \
  "burin-labs/harn/.github/workflows/build-release-binaries.yml@$policy_revision" \
  signed \
  notarized \
  1
"$assembler" \
  --output "$fixture/manifest.json" \
  --source-commit "$source_commit" \
  --policy-revision "$policy_revision" \
  --run-id "$run_id" \
  --run-attempt 2 \
  --receipts-dir "$fixture/receipts" \
  --workflow-url "$workflow_url"
expect_success mixed_producer_attempts run_verifier

new_fixture
jq '.runAttempt = "2"' \
  "$fixture/receipts/aarch64-apple-darwin.json" >"$fixture/receipt.tmp"
mv "$fixture/receipt.tmp" "$fixture/receipts/aarch64-apple-darwin.json"
expect_failure assemble_rejects_future_producer_attempt \
  "$assembler" \
  --output "$fixture/manifest-future-attempt.json" \
  --source-commit "$source_commit" \
  --policy-revision "$policy_revision" \
  --run-id "$run_id" \
  --run-attempt "$run_attempt" \
  --receipts-dir "$fixture/receipts"

new_fixture
jq '.runId = "9002"' \
  "$fixture/receipts/aarch64-apple-darwin.json" >"$fixture/receipt.tmp"
mv "$fixture/receipt.tmp" "$fixture/receipts/aarch64-apple-darwin.json"
expect_failure assemble_rejects_other_run \
  "$assembler" \
  --output "$fixture/manifest-other-run.json" \
  --source-commit "$source_commit" \
  --policy-revision "$policy_revision" \
  --run-id "$run_id" \
  --run-attempt "$run_attempt" \
  --receipts-dir "$fixture/receipts"

new_fixture
jq '.archives["aarch64-apple-darwin"].attestationIdentity = ""' \
  "$fixture/manifest.json" >"$fixture/manifest.tmp"
mv "$fixture/manifest.tmp" "$fixture/manifest.json"
expect_failure missing_attestation_identity run_verifier

new_fixture
rm "$fixture/receipts/aarch64-apple-darwin.json"
expect_failure assemble_requires_five \
  "$assembler" \
  --output "$fixture/manifest-again.json" \
  --source-commit "$source_commit" \
  --policy-revision "$policy_revision" \
  --run-id "$run_id" \
  --run-attempt "$run_attempt" \
  --receipts-dir "$fixture/receipts"

new_fixture
expect_failure assemble_rejects_overwrite \
  "$assembler" \
  --output "$fixture/manifest.json" \
  --source-commit "$source_commit" \
  --policy-revision "$policy_revision" \
  --run-id "$run_id" \
  --run-attempt "$run_attempt" \
  --receipts-dir "$fixture/receipts"

new_fixture
jq '.policyRevision = "3333333333333333333333333333333333333333"' \
  "$fixture/receipts/aarch64-apple-darwin.json" >"$fixture/receipt.tmp"
mv "$fixture/receipt.tmp" "$fixture/receipts/aarch64-apple-darwin.json"
expect_failure assemble_rejects_mismatch \
  "$assembler" \
  --output "$fixture/manifest-mismatch.json" \
  --source-commit "$source_commit" \
  --policy-revision "$policy_revision" \
  --run-id "$run_id" \
  --run-attempt "$run_attempt" \
  --receipts-dir "$fixture/receipts"

new_fixture
rm -f "$fixture/manifest.json"
"$assembler" \
  --output "$fixture/manifest.json" \
  --source-commit "$source_commit" \
  --policy-revision "$policy_revision" \
  --run-id "$run_id" \
  --run-attempt "$run_attempt" \
  --receipts-dir "$fixture/receipts" \
  --workflow-url "$workflow_url"
expect_success roundtrip run_verifier
jq -e \
  --arg schema "$CANDIDATE_ARCHIVE_SCHEMA" \
  --arg source "$source_commit" \
  --arg policy "$policy_revision" \
  --arg run "$run_id" \
  --arg attempt "$run_attempt" \
  --arg url "$workflow_url" \
  '.schemaVersion == $schema and
   .sourceCommit == $source and
   .policyRevision == $policy and
   .runId == $run and
   .runAttempt == $attempt and
   .workflowUrl == $url and
   (.archives | length) == 5' \
  "$fixture/manifest.json" >/dev/null

workflow="$root/.github/workflows/build-release-binaries.yml"
require_workflow_text() {
  local text="$1"
  if ! grep -Fq -- "$text" "$workflow"; then
    echo "FAIL: release workflow is missing: $text" >&2
    exit 1
  fi
}
require_workflow_pattern() {
  local pattern="$1"
  local description="$2"
  if ! grep -Eq -- "$pattern" "$workflow"; then
    echo "FAIL: release workflow is missing: $description" >&2
    exit 1
  fi
}
require_workflow_text 'candidate_only:'
require_workflow_text 'promote_only:'
require_workflow_text 'candidate_run_id:'
require_workflow_text 'candidate-archive-manifest-'
require_workflow_text 'should_package_archives'
require_workflow_text 'name: Promote candidate archives'
require_workflow_pattern "build_mode == 'candidate'" 'candidate packaging gate'

echo "candidate archive promotion tests passed"
