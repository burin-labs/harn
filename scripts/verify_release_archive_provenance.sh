#!/usr/bin/env bash
set -euo pipefail

readonly predicate_type="https://harnlang.com/attestations/release-archive/v1"
readonly predicate_schema="harn.release_archive_provenance.v1"
readonly signer_workflow=".github/workflows/build-release-binaries.yml"
readonly legacy_max_version="0.10.40"

usage() {
  cat <<'EOF'
Usage: scripts/verify_release_archive_provenance.sh \
  --artifacts-dir DIR --tag vX.Y.Z --repo OWNER/REPO \
  [--legacy-override JSON]

Verifies every release archive against its GitHub artifact attestation before
release metadata is regenerated or the release is marked latest.

The optional legacy override is only for releases predating attestations. It
must be an audited, exact binding:
{"tag":"vX.Y.Z","sourceCommit":"<40-hex>","reason":"...","archives":{"<filename>":"<sha256>"}}
EOF
}

artifacts_dir=""
tag=""
repo=""
legacy_override=""
while (($#)); do
  case "$1" in
    --artifacts-dir) artifacts_dir="${2:-}"; shift 2 ;;
    --tag) tag="${2:-}"; shift 2 ;;
    --repo) repo="${2:-}"; shift 2 ;;
    --legacy-override) legacy_override="${2:-}"; shift 2 ;;
    -h|--help) usage; exit 0 ;;
    *) echo "error: unknown argument: $1" >&2; usage >&2; exit 2 ;;
  esac
done

if [[ -z "$artifacts_dir" || -z "$tag" || -z "$repo" ]]; then
  echo "error: --artifacts-dir, --tag, and --repo are required" >&2
  usage >&2
  exit 2
fi
if [[ ! "$tag" =~ ^v[0-9]+\.[0-9]+\.[0-9]+$ ]]; then
  echo "error: expected tag vX.Y.Z, got '$tag'" >&2
  exit 2
fi
if [[ ! "$repo" =~ ^[^/]+/[^/]+$ ]]; then
  echo "error: expected repo OWNER/REPO, got '$repo'" >&2
  exit 2
fi
if [[ ! -d "$artifacts_dir" ]]; then
  echo "error: artifacts directory does not exist: $artifacts_dir" >&2
  exit 1
fi

declare -A target_for_archive=(
  ["harn-aarch64-apple-darwin.tar.gz"]="aarch64-apple-darwin"
  ["harn-aarch64-unknown-linux-gnu.tar.gz"]="aarch64-unknown-linux-gnu"
  ["harn-x86_64-apple-darwin.tar.gz"]="x86_64-apple-darwin"
  ["harn-x86_64-pc-windows-msvc.zip"]="x86_64-pc-windows-msvc"
  ["harn-x86_64-unknown-linux-gnu.tar.gz"]="x86_64-unknown-linux-gnu"
)
readonly expected_archives=(
  harn-aarch64-apple-darwin.tar.gz
  harn-aarch64-unknown-linux-gnu.tar.gz
  harn-x86_64-apple-darwin.tar.gz
  harn-x86_64-pc-windows-msvc.zip
  harn-x86_64-unknown-linux-gnu.tar.gz
)

sha256_file() {
  if command -v sha256sum >/dev/null 2>&1; then
    sha256sum "$1" | awk '{print $1}'
  else
    shasum -a 256 "$1" | awk '{print $1}'
  fi
}

semver_le() {
  local left_major left_minor left_patch right_major right_minor right_patch
  IFS=. read -r left_major left_minor left_patch <<<"$1"
  IFS=. read -r right_major right_minor right_patch <<<"$2"
  (( left_major < right_major ||
    (left_major == right_major && left_minor < right_minor) ||
    (left_major == right_major && left_minor == right_minor && left_patch <= right_patch) ))
}

tag_ref_json="$(gh api "repos/$repo/git/ref/tags/$tag")"
tag_object_type="$(jq -r '.object.type // empty' <<<"$tag_ref_json")"
tag_object_sha="$(jq -r '.object.sha // empty' <<<"$tag_ref_json")"
if [[ "$tag_object_type" != "tag" || ! "$tag_object_sha" =~ ^[0-9a-f]{40}$ ]]; then
  echo "error: $tag is not an annotated signed tag" >&2
  exit 1
fi

tag_object_json="$(gh api "repos/$repo/git/tags/$tag_object_sha")"
source_commit="$(jq -r '.object.sha // empty' <<<"$tag_object_json")"
if [[ "$(jq -r '.object.type // empty' <<<"$tag_object_json")" != "commit" ||
      ! "$source_commit" =~ ^[0-9a-f]{40}$ ||
      "$(jq -r '.verification.verified // false' <<<"$tag_object_json")" != "true" ]]; then
  echo "error: $tag is not a GitHub-verified signed tag pointing directly to a commit" >&2
  exit 1
fi

release_json="$(gh release view "$tag" --repo "$repo" --json assets)"
for archive in "${expected_archives[@]}"; do
  count="$(jq --arg name "$archive" '[.assets[] | select(.name == $name)] | length' <<<"$release_json")"
  if [[ "$count" != "1" ]]; then
    echo "error: release $tag must contain exactly one asset named $archive (found $count)" >&2
    exit 1
  fi
  if [[ ! -f "$artifacts_dir/$archive" ]]; then
    echo "error: missing release archive: $archive" >&2
    exit 1
  fi
done

mapfile -t downloaded_archives < <(
  find "$artifacts_dir" -maxdepth 1 -type f \
    \( -name 'harn-*.tar.gz' -o -name 'harn-*.zip' \) \
    -exec basename {} \; | LC_ALL=C sort
)
if [[ "${downloaded_archives[*]}" != "$(printf '%s\n' "${expected_archives[@]}" | LC_ALL=C sort | paste -sd ' ' -)" ]]; then
  echo "error: downloaded release archive set is not the exact five-target contract" >&2
  printf 'observed: %s\n' "${downloaded_archives[*]:-<none>}" >&2
  exit 1
fi

if [[ -n "$legacy_override" ]]; then
  if ! semver_le "${tag#v}" "$legacy_max_version"; then
    echo "error: legacy provenance overrides are not allowed after v$legacy_max_version" >&2
    exit 1
  fi
  if ! jq -e \
    --arg tag "$tag" \
    --arg source "$source_commit" \
    '.tag == $tag and
     .sourceCommit == $source and
     (.reason | type == "string" and length > 0) and
     (.archives | type == "object" and length > 0) and
     all(.archives[]; type == "string" and test("^[0-9a-f]{64}$"))' \
    >/dev/null <<<"$legacy_override"; then
    echo "error: legacy provenance override is malformed or does not bind $tag@$source_commit" >&2
    exit 1
  fi
fi

tmp_dir="$(mktemp -d)"
trap 'rm -rf "$tmp_dir"' EXIT
declare -A legacy_used=()

predicate_matches() {
  local result_file="$1"
  local archive="$2"
  local target="$3"
  local digest="$4"
  local expected_policy_revision="${5:-}"
  jq -e \
    --arg schema "$predicate_schema" \
    --arg repo "$repo" \
    --arg tag "$tag" \
    --arg source "$source_commit" \
    --arg archive "$archive" \
    --arg target "$target" \
    --arg digest "$digest" \
    --arg workflow "$signer_workflow" \
    --arg expectedPolicyRevision "$expected_policy_revision" \
    'any(.[];
      .verificationResult.statement.predicate as $p |
      $p.schemaVersion == $schema and
      $p.repository == $repo and
      $p.tag == $tag and
      $p.sourceCommit == $source and
      $p.archive == $archive and
      $p.target == $target and
      $p.sha256 == $digest and
      $p.buildPolicy.workflow == $workflow and
      ($p.buildPolicy.revision | type == "string" and test("^[0-9a-f]{40}$")) and
      ($expectedPolicyRevision == "" or $p.buildPolicy.revision == $expectedPolicyRevision) and
      ($p.buildPolicy.ref | type == "string" and length > 0) and
      ($p.workflow.runId | type == "string" and test("^[0-9]+$")) and
      ($p.workflow.runAttempt | type == "string" and test("^[0-9]+$")) and
      $p.workflow.job == "build" and
      ($p.workflow.url | type == "string" and contains("/actions/runs/"))
    )' "$result_file" >/dev/null
}

for archive in "${expected_archives[@]}"; do
  path="$artifacts_dir/$archive"
  target="${target_for_archive[$archive]}"
  digest="$(sha256_file "$path")"
  first_result="$tmp_dir/$archive.first.json"
  verified=false

  if gh attestation verify "$path" \
    --repo "$repo" \
    --signer-workflow "$repo/$signer_workflow" \
    --predicate-type "$predicate_type" \
    --limit 100 \
    --format json >"$first_result" 2>"$tmp_dir/$archive.stderr" &&
    predicate_matches "$first_result" "$archive" "$target" "$digest"; then
    mapfile -t policy_revisions < <(
      jq -r \
        --arg schema "$predicate_schema" \
        --arg tag "$tag" \
        --arg source "$source_commit" \
        --arg archive "$archive" \
        --arg target "$target" \
        --arg digest "$digest" \
        '.[] |
         .verificationResult.statement.predicate as $p |
         select(
           $p.schemaVersion == $schema and
           $p.tag == $tag and
           $p.sourceCommit == $source and
           $p.archive == $archive and
           $p.target == $target and
           $p.sha256 == $digest
         ) |
         $p.buildPolicy.revision' \
        "$first_result" | LC_ALL=C sort -u
    )
    for policy_revision in "${policy_revisions[@]}"; do
      pinned_result="$tmp_dir/$archive.$policy_revision.json"
      if gh attestation verify "$path" \
        --repo "$repo" \
        --signer-workflow "$repo/$signer_workflow" \
        --signer-digest "$policy_revision" \
        --predicate-type "$predicate_type" \
        --limit 100 \
        --format json >"$pinned_result" 2>>"$tmp_dir/$archive.stderr" &&
        predicate_matches "$pinned_result" "$archive" "$target" "$digest" "$policy_revision"; then
        verified=true
        break
      fi
    done
  fi

  if [[ "$verified" == "true" ]]; then
    echo "verified release provenance: $archive -> $tag@$source_commit"
    continue
  fi

  override_digest=""
  if [[ -n "$legacy_override" ]]; then
    override_digest="$(jq -r --arg archive "$archive" '.archives[$archive] // empty' <<<"$legacy_override")"
  fi
  if [[ "$override_digest" == "$digest" ]]; then
    legacy_used["$archive"]=1
    echo "::warning title=Legacy release provenance override::$archive has no valid repository attestation; accepting its exact audited SHA-256 for $tag@$source_commit."
    continue
  fi

  cat "$tmp_dir/$archive.stderr" >&2 || true
  echo "error: no valid release provenance binds $archive ($digest) to $tag@$source_commit" >&2
  exit 1
done

if [[ -n "$legacy_override" ]]; then
  mapfile -t override_names < <(jq -r '.archives | keys[]' <<<"$legacy_override")
  for archive in "${override_names[@]}"; do
    if [[ -z "${target_for_archive[$archive]+x}" ]]; then
      echo "error: legacy override names unexpected archive: $archive" >&2
      exit 1
    fi
    if [[ -z "${legacy_used[$archive]+x}" ]]; then
      echo "error: legacy override entry was not needed: $archive" >&2
      exit 1
    fi
  done
fi

echo "verified all release archives against signed source $tag@$source_commit"
