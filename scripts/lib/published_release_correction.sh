#!/usr/bin/env bash

# A corrected stable source is authority only after the owning publisher has
# published it. Permanent, signed release metadata survives run/artifact expiry.
release_require_published_correction() (
  set -euo pipefail
  local repository="${1:?repository required}" tag="${2:?tag required}" source="${3:?source required}"
  local scratch release manifest index run attempt digest verified
  # Closed GitHub release-app actor, not a display-name or commit-author claim.
  local publisher='{"id":278545796,"login":"harn-release-bot[bot]","type":"Bot"}'
  local library_dir
  library_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
  # shellcheck source=scripts/lib/candidate_archive_contract.sh
  source "$library_dir/candidate_archive_contract.sh"
  [[ "$repository" =~ ^[A-Za-z0-9_.-]+/[A-Za-z0-9_.-]+$ && "$source" =~ ^[0-9a-f]{40}$ ]] || return 1
  scratch="$(mktemp -d "${TMPDIR:-/tmp}/harn-published-correction.XXXXXX")" || return 1
  trap 'rm -rf "$scratch"' EXIT

  # GitHub's verified commit record is separate from the unsigned publication
  # tag. A same-version commit without a valid source signature is not admitted.
  gh api "repos/$repository/git/commits/$source" | jq -e --arg source "$source" '
    .sha == $source and .verification.verified == true and .verification.reason == "valid"
  ' >/dev/null || return 1
  release="$(gh api "repos/$repository/releases/tags/$tag")" || return 1
  jq -e --arg tag "$tag" --arg repository "$repository" --argjson publisher "$publisher" '
    .tag_name == $tag and .draft == false and .prerelease == false and
    (.id | type == "number" and . > 0) and
    (.published_at | type == "string" and length > 0) and
    .html_url == ("https://github.com/" + $repository + "/releases/tag/" + $tag) and
    (.author | {id,login,type}) == $publisher and
    (.assets | type == "array" and length > 0) and
    ([.assets[].name] | unique | length) == (.assets | length)
  ' <<< "$release" >/dev/null || return 1
  gh release download "$tag" --repo "$repository" --pattern candidate-manifest.json \
    --pattern release-assets.json --dir "$scratch" || return 1
  manifest="$scratch/candidate-manifest.json"
  index="$scratch/release-assets.json"
  # Both downloaded files must be exactly the bytes of their public assets.
  for file in candidate-manifest.json release-assets.json; do
    digest="$(sha256_file "$scratch/$file")" || return 1
    jq -e --arg file "$file" --arg digest "sha256:$digest" --argjson publisher "$publisher" '
      [.assets[] | select(.name == $file)] as $matches |
      ($matches | length) == 1 and all($matches[];
        .state == "uploaded" and .digest == $digest and (.uploader | {id,login,type}) == $publisher and
        (.id | type == "number" and . > 0) and (.size | type == "number" and . > 0))
    ' <<< "$release" >/dev/null || return 1
  done
  run="$(jq -er '.runId | tostring | select(test("^[1-9][0-9]*$"))' "$manifest")" || return 1
  attempt="$(jq -er '.runAttempt | tostring | select(test("^[1-9][0-9]*$"))' "$manifest")" || return 1
  digest="$(sha256_file "$index")" || return 1
  jq -e --arg schema "$CANDIDATE_MANIFEST_SCHEMA" --arg repository "$repository" \
    --arg source "$source" --arg tag "$tag" --arg predicate "$RELEASE_ARCHIVE_PREDICATE_TYPE" \
    --arg digest "$digest" --argjson targets "$(candidate_archive_expected_targets_json)" \
    --argjson release "$release" --argjson publisher "$publisher" --slurpfile index "$index" '
    . as $manifest | $index[0] as $index |
    .schemaVersion == $schema and .repository == $repository and .sourceCommit == $source and
    $index.tag == $tag and $index.version == ($tag | ltrimstr("v")) and
    $index.release_url == ("https://github.com/" + $repository + "/releases/tag/" + $tag) and
    ($index.assets | keys | sort) == ($targets | sort) and
    ([.artifacts[] | select(.kind == "archive") | .target] | sort) == ($targets | sort) and
    ([.artifacts[] | select(.kind == "asset-index" and .file == "release-assets.json" and
      .sha256 == $digest and .attestationPredicateType == $predicate)] | length) == 1 and
    all($targets[]; . as $target | $index.assets[$target] as $asset |
      [$manifest.artifacts[] | select(.kind == "archive" and .target == $target)] as $entries |
      ($entries | length) == 1 and $entries[0].sha256 == $asset.sha256 and
      $entries[0].attestationPredicateType == $predicate and
      $entries[0].artifact == ("harn-" + $target) and $entries[0].file == $asset.filename and
      ($asset.sha256 | type == "string" and test("^[0-9a-f]{64}$")) and
      $asset.filename == ("harn-" + $target + (if $target == "x86_64-pc-windows-msvc" then ".zip" else ".tar.gz" end)) and
      $asset.url == ("https://github.com/" + $repository + "/releases/download/" + $tag + "/" + $asset.filename) and
      ($asset.size | type == "number" and . > 0 and . == floor) and
      [$release.assets[] | select(.name == $asset.filename)] as $published |
      ($published | length) == 1 and $published[0].state == "uploaded" and
      ($published[0].uploader | {id,login,type}) == $publisher and
      $published[0].size == $asset.size and $published[0].digest == ("sha256:" + $asset.sha256))
  ' "$manifest" >/dev/null || return 1

  # Verification checks signatures and transparency timestamps, not a mutable
  # API assertion that a run succeeded. The certificate supplies the immutable
  # source/workflow/run identity; the typed predicate binds release semantics.
  verified="$(gh attestation verify "$index" --repo "$repository" \
    --signer-workflow "$repository/.github/workflows/build-release-binaries.yml" \
    --source-digest "$source" --source-ref refs/heads/main \
    --predicate-type "$RELEASE_ARCHIVE_PREDICATE_TYPE" --format json)" || return 1
  jq -e --arg repository "$repository" --arg source "$source" --arg version "${tag#v}" \
    --arg run "$run" --arg attempt "$attempt" --arg digest "$digest" \
    --arg predicate "$RELEASE_ARCHIVE_PREDICATE_TYPE" '
    type == "array" and length > 0 and any(.[];
      .verificationResult as $verified | $verified.signature.certificate as $certificate |
      $verified.statement as $statement |
      ($verified.verifiedTimestamps | type == "array" and length > 0) and
      $certificate.issuer == "https://token.actions.githubusercontent.com" and
      $certificate.sourceRepositoryURI == ("https://github.com/" + $repository) and
      $certificate.sourceRepositoryDigest == $source and $certificate.sourceRepositoryRef == "refs/heads/main" and
      $certificate.buildSignerURI == ("https://github.com/" + $repository + "/.github/workflows/build-release-binaries.yml@refs/heads/main") and
      $certificate.buildSignerDigest == $source and
      ($certificate.buildTrigger | IN("push", "workflow_dispatch")) and
      $certificate.runInvocationURI == ("https://github.com/" + $repository + "/actions/runs/" + $run + "/attempts/" + $attempt) and
      $statement.predicateType == $predicate and
      any($statement.subject[]; .name == "release-assets.json" and .digest.sha256 == $digest) and
      $statement.predicate.schemaVersion == "harn.release_files_provenance.v1" and
      $statement.predicate.phase == "candidate" and $statement.predicate.repository == $repository and
      $statement.predicate.sourceCommit == $source and $statement.predicate.version == $version and
      ($statement.predicate.workflow.runId | tostring) == $run and
      ($statement.predicate.workflow.runAttempt | tostring) == $attempt)
  ' <<< "$verified" >/dev/null
)
