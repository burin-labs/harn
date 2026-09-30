#!/usr/bin/env bash
# The per-target receipt writer and the candidate manifest assembler, end to
# end on fixture archives, with the refusals that keep a manifest from
# recording a file this run did not build.
set -euo pipefail

root="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
writer="$root/scripts/write_candidate_archive_receipt.sh"
assembler="$root/scripts/assemble_candidate_manifest.sh"
# shellcheck source=scripts/lib/candidate_archive_contract.sh
source "$root/scripts/lib/candidate_archive_contract.sh"
tmp="$(mktemp -d)"
trap 'rm -rf "$tmp"' EXIT

readonly repository="burin-labs/harn"
readonly source_commit="1111111111111111111111111111111111111111"
readonly policy_revision="2222222222222222222222222222222222222222"
readonly run_id="9001"
readonly run_attempt="1"

fail() {
  echo "FAIL: $*" >&2
  exit 1
}

mkdir -p "$tmp/archives" "$tmp/receipts" "$tmp/files"
for target in $(candidate_archive_expected_targets_json | jq -r '.[]'); do
  archive="$(archive_for_target "$target")"
  printf 'archive bytes for %s\n' "$target" > "$tmp/archives/$archive"
  signing=not_applicable
  notarization=not_applicable
  if [[ "$target" == *-apple-darwin ]]; then
    signing=signed
    notarization=notarized
  fi
  "$writer" \
    --output "$tmp/receipts/$target.json" \
    --source-commit "$source_commit" \
    --target "$target" \
    --archive "$archive" \
    --sha256 "$(sha256_file "$tmp/archives/$archive")" \
    --policy-revision "$policy_revision" \
    --signing-status "$signing" \
    --notarization-status "$notarization" \
    --attestation-identity "$RELEASE_ARCHIVE_PREDICATE_TYPE" \
    --run-id "$run_id" \
    --run-attempt "$run_attempt" >/dev/null
done
printf 'abc  harn-x86_64-unknown-linux-gnu.tar.gz\n' > "$tmp/files/SHA256SUMS"
printf '{"assets":[]}\n' > "$tmp/files/release-assets.json"
printf '## v1.2.3\n\nNotes.\n' > "$tmp/files/release-notes.md"

assemble() {
  local output=$1
  shift
  "$assembler" \
    --output "$output" \
    --repository "$repository" \
    --source-commit "$source_commit" \
    --policy-revision "$policy_revision" \
    --run-id "$run_id" \
    --run-attempt "$run_attempt" \
    --receipts-dir "${RECEIPTS:-$tmp/receipts}" \
    --archives-dir "${ARCHIVES:-$tmp/archives}" \
    --release-files-dir "${FILES:-$tmp/files}" \
    "$@"
}

expect_refusal() {
  local description=$1
  local output=$2
  shift 2
  if "$@" > "$tmp/refusal.out" 2>&1; then
    fail "$description"
  fi
  [[ ! -e "$output" ]] || fail "$description (a manifest was written anyway)"
}

manifest="$tmp/candidate-manifest.json"
assemble "$manifest" >/dev/null
jq -e \
  --arg schema "$CANDIDATE_MANIFEST_SCHEMA" \
  --arg repository "$repository" \
  --arg commit "$source_commit" \
  --arg predicate "$RELEASE_ARCHIVE_PREDICATE_TYPE" '
  .schemaVersion == $schema and
  .repository == $repository and
  .sourceCommit == $commit and
  .runId == "9001" and .runAttempt == "1" and
  ([.artifacts[] | select(.kind == "archive") | .target] | sort) == [
    "aarch64-apple-darwin", "aarch64-unknown-linux-gnu", "x86_64-apple-darwin",
    "x86_64-pc-windows-msvc", "x86_64-unknown-linux-gnu"] and
  all(.artifacts[] | select(.kind == "archive");
    .artifact == ("harn-" + .target) and
    ((.target | endswith("apple-darwin")) == (.signingStatus == "signed")) and
    ((.target | endswith("apple-darwin")) == (.notarizationStatus == "notarized"))) and
  ([.artifacts[] | select(.kind != "archive") | [.kind, .artifact, .file]] == [
    ["checksums", "harn-release-files", "SHA256SUMS"],
    ["asset-index", "harn-release-files", "release-assets.json"],
    ["notes", "harn-release-files", "release-notes.md"]]) and
  all(.artifacts[] | select(.kind != "archive");
    (has("target") | not) and .signingStatus == "not_applicable"
    and .notarizationStatus == "not_applicable") and
  all(.artifacts[]; .attestationPredicateType == $predicate and (.sha256 | test("^[0-9a-f]{64}$")))
' "$manifest" >/dev/null || fail "manifest does not have the expected shape: $(cat "$manifest")"
recorded="$(jq -r '.artifacts[] | select(.file == "release-notes.md") | .sha256' "$manifest")"
[[ "$recorded" == "$(sha256_file "$tmp/files/release-notes.md")" ]] \
  || fail "release notes digest is not the file's digest"
recorded="$(jq -r '.artifacts[] | select(.target == "x86_64-pc-windows-msvc") | .sha256' "$manifest")"
[[ "$recorded" == "$(sha256_file "$tmp/archives/harn-x86_64-pc-windows-msvc.zip")" ]] \
  || fail "archive digest is not the archive's digest"

# Negative control: an archive whose bytes are not the ones its receipt records.
cp -R "$tmp/archives" "$tmp/archives-substituted"
printf 'substituted\n' >> "$tmp/archives-substituted/harn-aarch64-apple-darwin.tar.gz"
ARCHIVES="$tmp/archives-substituted" expect_refusal \
  "assembler recorded an archive whose digest differs from its receipt" \
  "$tmp/substituted.json" assemble "$tmp/substituted.json"
grep -Fq 'harn-aarch64-apple-darwin.tar.gz has sha256' "$tmp/refusal.out" \
  || fail "substituted-archive refusal did not name the archive: $(cat "$tmp/refusal.out")"

# Negative control: a receipt another commit's build wrote.
cp -R "$tmp/receipts" "$tmp/receipts-other"
jq '.sourceCommit = "3333333333333333333333333333333333333333"' \
  "$tmp/receipts/x86_64-unknown-linux-gnu.json" > "$tmp/receipts-other/x86_64-unknown-linux-gnu.json"
RECEIPTS="$tmp/receipts-other" expect_refusal \
  "assembler accepted a receipt from another commit" \
  "$tmp/other-commit.json" assemble "$tmp/other-commit.json"

# Negative control: a missing target.
cp -R "$tmp/receipts" "$tmp/receipts-missing"
rm "$tmp/receipts-missing/x86_64-apple-darwin.json"
RECEIPTS="$tmp/receipts-missing" expect_refusal \
  "assembler wrote a manifest without every target" \
  "$tmp/missing-target.json" assemble "$tmp/missing-target.json"

# Negative control: a release file that was never built.
cp -R "$tmp/files" "$tmp/files-missing"
rm "$tmp/files-missing/release-notes.md"
FILES="$tmp/files-missing" expect_refusal \
  "assembler wrote a manifest without the release notes" \
  "$tmp/missing-notes.json" assemble "$tmp/missing-notes.json"

expect_refusal "assembler overwrote an existing manifest" "$tmp/none.json" \
  assemble "$manifest"

echo "candidate_manifest_test: ok"
