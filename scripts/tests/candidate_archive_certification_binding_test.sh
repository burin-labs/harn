#!/usr/bin/env bash
set -euo pipefail

root="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
verifier="$root/scripts/verify_candidate_archive_certification.sh"
tmp="$(mktemp -d "${TMPDIR:-/tmp}/harn-candidate-binding-test.XXXXXX")"
trap 'rm -rf "$tmp"' EXIT

git init --bare -q "$tmp/origin.git"
git init -q -b main "$tmp/work"
git -C "$tmp/work" config user.name harn-release-bot
git -C "$tmp/work" config user.email harn-release-bot@example.com
git -C "$tmp/work" config commit.gpgSign false
git -C "$tmp/work" remote add origin "$tmp/origin.git"
ssh-keygen -q -t ed25519 -N '' -f "$tmp/signing-key"
mkdir -p "$tmp/work/.github"
printf 'harn-release-bot@example.com %s\n' "$(cat "$tmp/signing-key.pub")" \
  >"$tmp/work/.github/release-bot-allowed-signers"
printf 'candidate\n' >"$tmp/work/candidate.txt"
git -C "$tmp/work" add .
git -C "$tmp/work" commit -q -m candidate
source_sha="$(git -C "$tmp/work" rev-parse HEAD)"
policy_sha="$source_sha"
run_id=33988090262
git -C "$tmp/work" push -q -u origin main
git -C "$tmp/work" config gpg.format ssh
git -C "$tmp/work" config user.signingkey "$tmp/signing-key"

record="$(jq -cn \
  --arg source "$source_sha" \
  --arg policy "$policy_sha" \
  --argjson run "$run_id" '
  {
    schema_version: "release_harn.candidate_archive_certification.v1",
    receipt: {
      schema_version: "release_harn.candidate_archive.v1",
      slug: "burin-labs/harn",
      workflow: "build-release-binaries.yml",
      event: "workflow_dispatch",
      policy_ref: "main",
      expected_policy_sha: $policy,
      source_ref: ("release-attempt/v0.10.131/" + $source),
      expected_source_sha: $source,
      run_id: $run,
      run_url: ("https://github.com/burin-labs/harn/actions/runs/" + ($run | tostring)),
      observed_event: "workflow_dispatch",
      observed_head_branch: "main",
      observed_head_sha: $policy,
      created_at: "2026-09-05T00:00:00Z"
    }
  }')"
tag="harn-candidate-archive-certification/$source_sha"
git -C "$tmp/work" tag -s "$tag" -m "Harn candidate archive certification

Harn-Candidate-Archive-Certification: $record" "$source_sha"
git -C "$tmp/work" push -q origin "refs/tags/$tag"

"$verifier" burin-labs/harn "$source_sha" "$run_id" "$policy_sha" "$policy_sha" "$tmp/work" \
  >"$tmp/pass.out"
grep -Fq "$source_sha -> run $run_id" "$tmp/pass.out"

# Negative control: both producer runs may be green, but the signed record
# selected exactly one. Supplying a different successful run must still fail.
if "$verifier" burin-labs/harn "$source_sha" 33998624166 "$policy_sha" "$policy_sha" "$tmp/work" \
  >"$tmp/wrong-run.out" 2>&1; then
  echo "FAIL: a different successful producer run bypassed the signed binding" >&2
  exit 1
fi
grep -Fq 'does not match the requested source/run/policy tuple' "$tmp/wrong-run.out"

ssh-keygen -q -t ed25519 -N '' -f "$tmp/untrusted-key"
git -C "$tmp/work" config user.signingkey "$tmp/untrusted-key"
git -C "$tmp/work" tag -d "$tag" >/dev/null
git -C "$tmp/work" tag -s "$tag" -m "Harn candidate archive certification

Harn-Candidate-Archive-Certification: $record" "$source_sha"
git -C "$tmp/work" push -q --force origin "refs/tags/$tag"
if "$verifier" burin-labs/harn "$source_sha" "$run_id" "$policy_sha" "$policy_sha" "$tmp/work" \
  >"$tmp/untrusted.out" 2>&1; then
  echo "FAIL: untrusted candidate archive certification was accepted" >&2
  exit 1
fi
grep -Fq 'no trusted release signature' "$tmp/untrusted.out"

echo "candidate_archive_certification_binding_test: ok"
