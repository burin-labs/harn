#!/usr/bin/env bash
set -euo pipefail

root="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
verifier="$root/scripts/verify_release_archive_provenance.sh"
tmp="$(mktemp -d)"
trap 'rm -rf "$tmp"' EXIT

readonly repo="burin-labs/harn"
readonly tag="v0.10.40"
readonly source_commit="1111111111111111111111111111111111111111"
readonly policy_revision="2222222222222222222222222222222222222222"
readonly archives=(
  harn-aarch64-apple-darwin.tar.gz
  harn-aarch64-unknown-linux-gnu.tar.gz
  harn-x86_64-apple-darwin.tar.gz
  harn-x86_64-pc-windows-msvc.zip
  harn-x86_64-unknown-linux-gnu.tar.gz
)

target_for_archive() {
  case "$1" in
    harn-aarch64-apple-darwin.tar.gz) echo aarch64-apple-darwin ;;
    harn-aarch64-unknown-linux-gnu.tar.gz) echo aarch64-unknown-linux-gnu ;;
    harn-x86_64-apple-darwin.tar.gz) echo x86_64-apple-darwin ;;
    harn-x86_64-pc-windows-msvc.zip) echo x86_64-pc-windows-msvc ;;
    harn-x86_64-unknown-linux-gnu.tar.gz) echo x86_64-unknown-linux-gnu ;;
    *) return 1 ;;
  esac
}

sha256_file() {
  if command -v sha256sum >/dev/null 2>&1; then
    sha256sum "$1" | awk '{print $1}'
  else
    shasum -a 256 "$1" | awk '{print $1}'
  fi
}

mkdir -p "$tmp/bin"
cat >"$tmp/bin/gh" <<'MOCK'
#!/usr/bin/env bash
set -euo pipefail

sha256_file() {
  if command -v sha256sum >/dev/null 2>&1; then
    sha256sum "$1" | awk '{print $1}'
  else
    shasum -a 256 "$1" | awk '{print $1}'
  fi
}

case "${1:-} ${2:-}" in
  "api repos/"*)
    if [[ "$2" == *"/git/ref/tags/"* ]]; then
      cat "$FIXTURE_DIR/tag-ref.json"
    else
      cat "$FIXTURE_DIR/tag-object.json"
    fi
    ;;
  "release view")
    cat "$FIXTURE_DIR/release.json"
    ;;
  "attestation verify")
    artifact="$3"
    shift 3
    signer_digest=""
    observed_repo=""
    observed_signer_workflow=""
    observed_predicate_type=""
    observed_format=""
    observed_limit=""
    while (($#)); do
      case "$1" in
        --signer-digest) signer_digest="${2:-}"; shift 2 ;;
        --repo) observed_repo="${2:-}"; shift 2 ;;
        --signer-workflow) observed_signer_workflow="${2:-}"; shift 2 ;;
        --predicate-type) observed_predicate_type="${2:-}"; shift 2 ;;
        --format) observed_format="${2:-}"; shift 2 ;;
        --limit) observed_limit="${2:-}"; shift 2 ;;
        *) shift ;;
      esac
    done
    [[ "$observed_repo" == "burin-labs/harn" ]]
    [[ "$observed_signer_workflow" == \
      "burin-labs/harn/.github/workflows/build-release-binaries.yml" ]]
    [[ "$observed_predicate_type" == \
      "https://harnlang.com/attestations/release-archive/v1" ]]
    [[ "$observed_format" == "json" ]]
    [[ "$observed_limit" == "100" ]]
    fixture="$FIXTURE_DIR/attest/$(basename "$artifact").json"
    [[ -f "$fixture" ]] || exit 1
    [[ "$(sha256_file "$artifact")" == "$(jq -r '.expectedSha' "$fixture")" ]] || exit 1
    if [[ -n "$signer_digest" ]]; then
      [[ "$signer_digest" == "$(jq -r '.signerDigest' "$fixture")" ]] || exit 1
    fi
    jq '.results' "$fixture"
    ;;
  *)
    echo "unexpected gh invocation: $*" >&2
    exit 2
    ;;
esac
MOCK
chmod +x "$tmp/bin/gh"

new_fixture() {
  fixture="$tmp/fixture"
  rm -rf "$fixture"
  mkdir -p "$fixture/artifacts" "$fixture/attest"
  jq -n \
    --argjson names "$(printf '%s\n' "${archives[@]}" | jq -R . | jq -s .)" \
    '{assets: [$names[] | {name: .}]}' >"$fixture/release.json"
  jq -n --arg sha aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa \
    '{object: {type: "tag", sha: $sha}}' >"$fixture/tag-ref.json"
  jq -n --arg source "$source_commit" \
    '{object: {type: "commit", sha: $source}, verification: {verified: true}}' \
    >"$fixture/tag-object.json"
  for archive in "${archives[@]}"; do
    printf 'fixture bytes for %s\n' "$archive" >"$fixture/artifacts/$archive"
  done
}

write_attestation() {
  local archive="$1"
  local run_id="$2"
  local predicate_tag="${3:-$tag}"
  local predicate_source="${4:-$source_commit}"
  local target digest
  target="$(target_for_archive "$archive")"
  digest="$(sha256_file "$fixture/artifacts/$archive")"
  jq -n \
    --arg expected "$digest" \
    --arg repo "$repo" \
    --arg tag "$predicate_tag" \
    --arg source "$predicate_source" \
    --arg archive "$archive" \
    --arg target "$target" \
    --arg digest "$digest" \
    --arg policy "$policy_revision" \
    --arg run "$run_id" \
    '{
      expectedSha: $expected,
      signerDigest: $policy,
      results: [{
        verificationResult: {
          statement: {
            predicate: {
              schemaVersion: "harn.release_archive_provenance.v1",
              repository: $repo,
              tag: $tag,
              sourceCommit: $source,
              archive: $archive,
              target: $target,
              sha256: $digest,
              buildPolicy: {
                workflow: ".github/workflows/build-release-binaries.yml",
                revision: $policy,
                ref: "burin-labs/harn/.github/workflows/build-release-binaries.yml@refs/heads/main"
              },
              workflow: {
                runId: $run,
                runAttempt: "1",
                job: "build",
                url: ("https://github.com/burin-labs/harn/actions/runs/" + $run)
              }
            }
          }
        }
      }]
    }' >"$fixture/attest/$archive.json"
}

attest_all() {
  local run_id="$1"
  for archive in "${archives[@]}"; do
    write_attestation "$archive" "$run_id"
  done
}

run_verifier() {
  FIXTURE_DIR="$fixture" PATH="$tmp/bin:$PATH" \
    "$verifier" --artifacts-dir "$fixture/artifacts" --tag "$tag" --repo "$repo" "$@"
}

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

# Original five-target release.
new_fixture
attest_all 100
expect_success primary run_verifier

# One-target recovery and its nonpublishing fixture: four archives retain
# original-run bindings while one comes from a recovery run; all resolve to the
# exact same peeled signed tag commit.
write_attestation harn-x86_64-unknown-linux-gnu.tar.gz 200
expect_success recovery run_verifier

# Metadata-only recovery is the same archive set and remains idempotent.
before_metadata="$(find "$fixture/artifacts" -type f -exec shasum -a 256 {} \; | LC_ALL=C sort)"
expect_success metadata_only run_verifier
after_metadata="$(find "$fixture/artifacts" -type f -exec shasum -a 256 {} \; | LC_ALL=C sort)"
if [[ "$before_metadata" != "$after_metadata" ]]; then
  echo "FAIL: metadata-only provenance verification mutated release archives" >&2
  exit 1
fi

# Replaced bytes cannot satisfy the subject digest.
printf 'tamper\n' >>"$fixture/artifacts/harn-aarch64-apple-darwin.tar.gz"
expect_failure tampered run_verifier

# Wrong source and mixed-version predicates fail even with intact bytes.
new_fixture
attest_all 100
write_attestation harn-aarch64-apple-darwin.tar.gz 100 "$tag" \
  3333333333333333333333333333333333333333
expect_failure wrong_source run_verifier
write_attestation harn-aarch64-apple-darwin.tar.gz 100 v9.9.9 "$source_commit"
expect_failure mixed_version run_verifier

# A self-asserted policy revision must also match the signer digest carried by
# the verified certificate.
new_fixture
attest_all 100
jq '.results[0].verificationResult.statement.predicate.buildPolicy.revision =
  "3333333333333333333333333333333333333333"' \
  "$fixture/attest/harn-aarch64-apple-darwin.tar.gz.json" \
  >"$fixture/attest/policy.tmp"
mv "$fixture/attest/policy.tmp" \
  "$fixture/attest/harn-aarch64-apple-darwin.tar.gz.json"
expect_failure wrong_policy_revision run_verifier

# Missing provenance and duplicate release filenames fail closed.
new_fixture
attest_all 100
rm "$fixture/attest/harn-aarch64-apple-darwin.tar.gz.json"
expect_failure missing_provenance run_verifier
write_attestation harn-aarch64-apple-darwin.tar.gz 100
jq '.assets += [{name: "harn-aarch64-apple-darwin.tar.gz"}]' \
  "$fixture/release.json" >"$fixture/release.tmp"
mv "$fixture/release.tmp" "$fixture/release.json"
expect_failure duplicate_filename run_verifier

# Older releases fail closed by default. An exact audited hash override can
# admit only the four unattested legacy archives while the rebuilt target still
# verifies normally.
new_fixture
attest_all 200
legacy_archives=("${archives[@]:0:4}")
for archive in "${legacy_archives[@]}"; do
  rm "$fixture/attest/$archive.json"
done
expect_failure legacy_default_closed run_verifier
legacy_json="$(
  jq -n \
    --arg tag "$tag" \
    --arg source "$source_commit" \
    --arg reason "Audited against retained original workflow artifacts." \
    --argjson archives "$(
      for archive in "${legacy_archives[@]}"; do
        jq -n \
          --arg name "$archive" \
          --arg digest "$(sha256_file "$fixture/artifacts/$archive")" \
          '{key: $name, value: $digest}'
      done | jq -s 'from_entries'
    )" \
    '{tag: $tag, sourceCommit: $source, reason: $reason, archives: $archives}'
)"
expect_success legacy_override run_verifier --legacy-override "$legacy_json"
printf 'replacement\n' >>"$fixture/artifacts/harn-aarch64-apple-darwin.tar.gz"
expect_failure legacy_override_tampered run_verifier --legacy-override "$legacy_json"
future_legacy_json="$(jq '.tag = "v0.10.41"' <<<"$legacy_json")"
expect_failure legacy_override_after_cutoff \
  env FIXTURE_DIR="$fixture" PATH="$tmp/bin:$PATH" \
  "$verifier" \
  --artifacts-dir "$fixture/artifacts" \
  --tag v0.10.41 \
  --repo "$repo" \
  --legacy-override "$future_legacy_json"

workflow="$root/.github/workflows/build-release-binaries.yml"
require_workflow_text() {
  local text="$1"
  if ! grep -Fq -- "$text" "$workflow"; then
    echo "FAIL: release workflow is missing: $text" >&2
    exit 1
  fi
}
workflow_line() {
  grep -nF -- "$1" "$workflow" | cut -d: -f1 | head -n1
}

require_workflow_text "actions/attest@f7c74d28b9d84cb8768d0b8ca14a4bac6ef463e6"
require_workflow_text "predicate-type: https://harnlang.com/attestations/release-archive/v1"
require_workflow_text "attestations: write"
require_workflow_text "attestations: read"
require_workflow_text "legacy_provenance_override:"
require_workflow_text "Release recovery must be dispatched from the main branch"
require_workflow_text "make_latest: \${{ needs.setup.outputs.make_latest }}"
require_workflow_text "enable=\${{ needs.setup.outputs.make_latest == 'true' }}"
require_workflow_text "needs.build.result == 'success' || needs.build.result == 'skipped'"
require_workflow_text "Only release metadata is missing"
require_workflow_text "gh release upload \"\$REF\" \"\$archive\" --repo \"\$GITHUB_REPOSITORY\" --clobber"
if grep -Fq "make_latest: true" "$workflow"; then
  echo "FAIL: historical recovery must not unconditionally move /releases/latest" >&2
  exit 1
fi

attest_line="$(workflow_line "name: Attest release archive provenance")"
publish_line="$(workflow_line "name: Publish archive to release (incremental)")"
verify_line="$(workflow_line "name: Verify release archive provenance")"
checksums_line="$(workflow_line "name: Generate SHA256SUMS")"
finalize_line="$(workflow_line "name: Finalize GitHub release")"
if ! (( attest_line < publish_line && verify_line < checksums_line && verify_line < finalize_line )); then
  echo "FAIL: provenance must be emitted before upload and verified before metadata/finalization" >&2
  exit 1
fi

echo "release archive provenance tests passed"
