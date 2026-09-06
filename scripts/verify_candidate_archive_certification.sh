#!/usr/bin/env bash
# Authenticate the write-once candidate SHA -> certified archive run binding.
set -euo pipefail

usage() {
  echo "usage: verify_candidate_archive_certification.sh REPO SOURCE_SHA RUN_ID ARCHIVE_POLICY_SHA TRUST_POLICY_SHA [GIT_REPO]" >&2
  exit 2
}

[[ $# -ge 5 && $# -le 6 ]] || usage
repo="$1"
source_sha="$2"
run_id="$3"
archive_policy_sha="$4"
trust_policy_sha="$5"
git_repo="${6:-.}"
[[ "$repo" =~ ^[A-Za-z0-9_.-]+/[A-Za-z0-9_.-]+$ \
  && "$source_sha" =~ ^[0-9a-f]{40}$ \
  && "$run_id" =~ ^[1-9][0-9]*$ \
  && "$archive_policy_sha" =~ ^[0-9a-f]{40}$ \
  && "$trust_policy_sha" =~ ^[0-9a-f]{40}$ ]] || usage
[[ -d "$git_repo/.git" || -f "$git_repo/.git" ]] || {
  echo "error: not a Git worktree: $git_repo" >&2
  exit 2
}

remote_ref="refs/tags/harn-candidate-archive-certification/$source_sha"
remote_rows="$(git -C "$git_repo" ls-remote --tags origin "$remote_ref" "$remote_ref^{}")"
tag_object="$(awk -v ref="$remote_ref" '$2 == ref {print $1}' <<<"$remote_rows")"
tag_target="$(awk -v ref="$remote_ref^{}" '$2 == ref {print $1}' <<<"$remote_rows")"
if [[ ! "$tag_object" =~ ^[0-9a-f]{40}$ || "$tag_target" != "$source_sha" ]]; then
  echo "error: signed candidate archive certification is missing or targets a different source" >&2
  exit 1
fi

git -C "$git_repo" fetch --quiet --no-tags origin "$tag_object"
fetched_object="$(git -C "$git_repo" rev-parse FETCH_HEAD)"
[[ "$fetched_object" == "$tag_object" ]] || {
  echo "error: fetched candidate archive certification differs from remote read-back" >&2
  exit 1
}

policy_file="$(mktemp "${TMPDIR:-/tmp}/harn-candidate-archive-signers.XXXXXX")"
trap 'rm -f "$policy_file"' EXIT
git -C "$git_repo" show "$trust_policy_sha:.github/release-bot-allowed-signers" >"$policy_file" || {
  echo "error: could not read release signer policy at $trust_policy_sha" >&2
  exit 1
}
tag_body="$(git -C "$git_repo" cat-file tag "$tag_object")" || {
  echo "error: candidate archive certification is not an annotated tag" >&2
  exit 1
}
if [[ "$(grep -c '^-----BEGIN SSH SIGNATURE-----$' <<<"$tag_body" || true)" != 1 ]] \
  || grep -q '^-----BEGIN PGP SIGNATURE-----$' <<<"$tag_body" \
  || ! git -C "$git_repo" \
    -c "gpg.ssh.allowedSignersFile=$policy_file" \
    verify-tag "$tag_object" >/dev/null 2>&1; then
  echo "error: candidate archive certification has no trusted release signature" >&2
  exit 1
fi

prefix='Harn-Candidate-Archive-Certification: '
record_lines="$(sed '/^-----BEGIN SSH SIGNATURE-----/,$d' <<<"$tag_body" \
  | grep -F "$prefix" || true)"
if [[ "$(grep -c . <<<"$record_lines" || true)" != 1 \
  || "$record_lines" != "$prefix"* ]]; then
  echo "error: candidate archive certification must contain exactly one closed record" >&2
  exit 1
fi
record="${record_lines#"$prefix"}"

jq -e \
  --arg repo "$repo" \
  --arg source "$source_sha" \
  --arg run "$run_id" \
  --arg policy "$archive_policy_sha" '
  keys == ["receipt", "schema_version"] and
  .schema_version == "release_harn.candidate_archive_certification.v1" and
  (.receipt | type == "object") and
  (.receipt | ((keys - ["archive_digests"]) | sort) == ([
    "created_at", "event", "expected_policy_sha", "expected_source_sha",
    "observed_event", "observed_head_branch", "observed_head_sha", "policy_ref",
    "run_id", "run_url", "schema_version", "slug", "source_ref", "workflow"
  ] | sort)) and
  (.receipt.archive_digests? // {} | type == "object") and
  .receipt.schema_version == "release_harn.candidate_archive.v1" and
  .receipt.slug == $repo and
  .receipt.workflow == "build-release-binaries.yml" and
  .receipt.event == "workflow_dispatch" and
  .receipt.policy_ref == "main" and
  .receipt.expected_policy_sha == $policy and
  .receipt.observed_event == "workflow_dispatch" and
  .receipt.observed_head_branch == "main" and
  .receipt.observed_head_sha == $policy and
  .receipt.expected_source_sha == $source and
  (.receipt.run_id | tostring) == $run
' <<<"$record" >/dev/null || {
  echo "error: signed candidate archive certification does not match the requested source/run/policy tuple" >&2
  exit 1
}

echo "Verified signed candidate archive certification: $source_sha -> run $run_id at archive policy $archive_policy_sha"
