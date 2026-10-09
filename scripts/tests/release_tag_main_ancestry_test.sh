#!/usr/bin/env bash
set -euo pipefail

root="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
verifier="${RELEASE_TAG_TEST_VERIFIER:-$root/scripts/verify_release_tag_main_ancestry.sh}"
tmp_root="$(mktemp -d "${TMPDIR:-/tmp}/harn-release-main-tag-test.XXXXXX")"
trap 'rm -rf "$tmp_root"' EXIT

git init -b main --bare -q "$tmp_root/origin.git"
git init -q -b main "$tmp_root/work"
git -C "$tmp_root/work" config user.name Test
git -C "$tmp_root/work" config user.email test@example.com
git -C "$tmp_root/work" config commit.gpgSign false
git -C "$tmp_root/work" config tag.gpgSign false
git -C "$tmp_root/work" remote add origin "$tmp_root/origin.git"
ssh-keygen -q -t ed25519 -N '' -f "$tmp_root/signing-key"
mkdir -p "$tmp_root/work/.github"
printf 'test@example.com %s\n' "$(cat "$tmp_root/signing-key.pub")" >"$tmp_root/work/.github/release-bot-allowed-signers"
printf '[workspace.package]\nversion = "1.2.2"\n' >"$tmp_root/work/Cargo.toml"
git -C "$tmp_root/work" add Cargo.toml .github
git -C "$tmp_root/work" commit -q -m bootstrap
git -C "$tmp_root/work" push -q -u origin main

printf '[workspace.package]\nversion = "1.2.3"\n' >"$tmp_root/work/Cargo.toml"
git -C "$tmp_root/work" add Cargo.toml
git -C "$tmp_root/work" commit -q -m 'Release v1.2.3 (#42)'
release_commit="$(git -C "$tmp_root/work" rev-parse HEAD)"
git -C "$tmp_root/work" tag -a v1.2.3 -m 'Release v1.2.3'
git -C "$tmp_root/work" push -q origin main refs/tags/v1.2.3

output="$($verifier --repo "$tmp_root/work" --tag v1.2.3)"
[[ "$output" == *"$release_commit"* ]] || {
  echo "FAIL: canonical merged-main release tag was not accepted" >&2
  exit 1
}
bootstrap_commit="$(git -C "$tmp_root/work" rev-parse HEAD^)"

# Finalization passes the commit it checked out; the verifier alone decides
# whether the tag selects it.
expect_selects() {
  local kind="$1"
  "$verifier" --repo "$tmp_root/work" --tag v1.2.3 --expect-commit "$release_commit" >/dev/null || {
    echo "FAIL: $kind tag on the expected Release commit was refused" >&2
    exit 1
  }
  if "$verifier" --repo "$tmp_root/work" --tag v1.2.3 --expect-commit "$bootstrap_commit" \
    >"$tmp_root/elsewhere.out" 2>&1; then
    echo "FAIL: $kind tag selecting a different commit than the checkout was accepted" >&2
    exit 1
  fi
  grep -Fq "selects $release_commit, not the expected commit $bootstrap_commit" "$tmp_root/elsewhere.out" || {
    echo "FAIL: $kind mismatch refusal did not name both commits: $(cat "$tmp_root/elsewhere.out")" >&2
    exit 1
  }
}
expect_selects annotated

if "$verifier" --repo "$tmp_root/work" --tag v9.9.9 >"$tmp_root/missing.out" 2>&1; then
  echo "FAIL: missing release tag was accepted" >&2
  exit 1
fi
grep -q 'missing or does not resolve to one exact commit' "$tmp_root/missing.out" || {
  echo "FAIL: missing-tag rejection did not name the remote tag invariant" >&2
  exit 1
}

# Promotion publishes through the Releases API, which tags the release commit
# with a lightweight ref. The same merged Release commit is accepted that way.
git -C "$tmp_root/origin.git" update-ref refs/tags/v1.2.3 "$release_commit"
lw_output="$($verifier --repo "$tmp_root/work" --tag v1.2.3)"
[[ "$lw_output" == *"$release_commit"*"trusted candidate=false"* ]] || {
  echo "FAIL: lightweight tag on the merged Release commit was not accepted: $lw_output" >&2
  exit 1
}
expect_selects lightweight
git -C "$tmp_root/work" push -q --force origin refs/tags/v1.2.3

if "$verifier" --repo "$tmp_root/work" --tag release-1.2.3 \
  >"$tmp_root/malformed.out" 2>&1; then
  echo "FAIL: malformed release tag was accepted" >&2
  exit 1
fi
grep -q 'expected canonical release tag' "$tmp_root/malformed.out" || {
  echo "FAIL: malformed-tag rejection did not name the input contract" >&2
  exit 1
}

git -C "$tmp_root/work" switch -q -c orphan HEAD^
printf '[workspace.package]\nversion = "1.2.4"\n' >"$tmp_root/work/Cargo.toml"
git -C "$tmp_root/work" add Cargo.toml
git -C "$tmp_root/work" commit -q -m 'Release v1.2.4 (#43)'
git -C "$tmp_root/work" tag -a v1.2.4 -m 'Release v1.2.4'
git -C "$tmp_root/work" push -q origin refs/tags/v1.2.4
if "$verifier" --repo "$tmp_root/work" --tag v1.2.4 >"$tmp_root/orphan.out" 2>&1; then
  echo "FAIL: orphaned release-attempt tag was accepted" >&2
  exit 1
fi
grep -q 'not reachable from origin/main' "$tmp_root/orphan.out" || {
  echo "FAIL: orphan rejection did not name the main-ancestry invariant" >&2
  exit 1
}

# A lightweight tag carries no signature, so off main it has no way in.
git -C "$tmp_root/origin.git" update-ref refs/tags/v1.2.4 "$(git -C "$tmp_root/work" rev-parse HEAD)"
if "$verifier" --repo "$tmp_root/work" --tag v1.2.4 >"$tmp_root/orphan-lw.out" 2>&1; then
  echo "FAIL: lightweight tag on an off-main commit was accepted" >&2
  exit 1
fi
grep -q 'a lightweight tag carries no candidate signature' "$tmp_root/orphan-lw.out" || {
  echo "FAIL: off-main lightweight rejection did not say why: $(cat "$tmp_root/orphan-lw.out")" >&2
  exit 1
}

# Real cryptographic controls: the same off-main commit becomes admissible only
# through the trusted release signer's exact candidate endorsement.
candidate_commit="$(git -C "$tmp_root/work" rev-parse HEAD)"
git -C "$tmp_root/work" config gpg.format ssh
git -C "$tmp_root/work" config user.signingkey "$tmp_root/signing-key"
git -C "$tmp_root/work" tag -d v1.2.4 >/dev/null
git -C "$tmp_root/work" tag -s v1.2.4 -m "Release v1.2.4

Harn-Release-Candidate: $candidate_commit"
git -C "$tmp_root/work" push -q --force origin refs/tags/v1.2.4
"$verifier" --repo "$tmp_root/work" --tag v1.2.4 >"$tmp_root/candidate.out"
grep -q 'trusted candidate=true' "$tmp_root/candidate.out"
"$root/scripts/stage_release_tools.sh" "$tmp_root/release-tools"
"$tmp_root/release-tools/verify_release_tag_main_ancestry.sh" \
  --repo "$tmp_root/work" --tag v1.2.4 >/dev/null
grep -Fq '"$SCRIPT_DIR/verify_release_tag_main_ancestry.sh"' "$tmp_root/release-tools/release_ship.sh"
grep -Fq -- '--expect-commit "$(git rev-parse HEAD)"' "$tmp_root/release-tools/release_ship.sh" || {
  echo "FAIL: release_ship.sh does not hand its checkout to the tag verifier" >&2
  exit 1
}
if grep -n 'ls-remote' "$tmp_root/release-tools/release_ship.sh"; then
  echo "FAIL: release_ship.sh reads a remote ref itself; the tag verifier owns that read" >&2
  exit 1
fi

# Main's corrected-source contract does not admit repaired off-main candidates,
# even when their signature and exact candidate metadata are valid.
git -C "$tmp_root/work" commit --allow-empty -q -m 'Repair an off-main candidate'
off_main_corrected="$(git -C "$tmp_root/work" rev-parse HEAD)"
git -C "$tmp_root/work" tag -d v1.2.4 >/dev/null
git -C "$tmp_root/work" tag -s v1.2.4 -m "Release v1.2.4

Harn-Release-Candidate: $off_main_corrected"
git -C "$tmp_root/work" push -q --force origin refs/tags/v1.2.4
if "$verifier" --repo "$tmp_root/work" --tag v1.2.4 >"$tmp_root/off-main-corrected.out" 2>&1; then
  echo "FAIL: corrected-source recovery escaped the main-ancestry boundary" >&2
  exit 1
fi
grep -q 'candidate parent is not reachable from origin/main' "$tmp_root/off-main-corrected.out"
git -C "$tmp_root/work" switch -q --detach "$candidate_commit"
git -C "$tmp_root/work" tag -d v1.2.4 >/dev/null
git -C "$tmp_root/work" tag -s v1.2.4 -m "Release v1.2.4

Harn-Release-Candidate: $candidate_commit"
git -C "$tmp_root/work" push -q --force origin refs/tags/v1.2.4

# Terminal cleanup may remove the certify ref; the signed endorsement remains.
git -C "$tmp_root/work" push -q origin "$candidate_commit:refs/heads/release-certify/$candidate_commit"
"$verifier" --repo "$tmp_root/work" --tag v1.2.4 >/dev/null
git -C "$tmp_root/work" push -q --force origin "$release_commit:refs/heads/release-certify/$candidate_commit"
if "$verifier" --repo "$tmp_root/work" --tag v1.2.4 >"$tmp_root/moved-certify.out" 2>&1; then
  echo "FAIL: moved certification ref accepted" >&2
  exit 1
fi
grep -q 'candidate certification ref moved' "$tmp_root/moved-certify.out"
git -C "$tmp_root/work" push -q origin ":refs/heads/release-certify/$candidate_commit"
"$verifier" --repo "$tmp_root/work" --tag v1.2.4 >/dev/null

# Even an otherwise trusted SSH tag must not carry a competing PGP envelope.
git -C "$tmp_root/work" tag -d v1.2.4 >/dev/null
git -C "$tmp_root/work" tag -s v1.2.4 -m "Release v1.2.4

Harn-Release-Candidate: $candidate_commit
-----BEGIN PGP SIGNATURE-----
non-SSH envelope is outside the release identity policy
-----END PGP SIGNATURE-----"
git -C "$tmp_root/work" push -q --force origin refs/tags/v1.2.4
if "$verifier" --repo "$tmp_root/work" --tag v1.2.4 >"$tmp_root/non-ssh.out" 2>&1; then
  echo "FAIL: non-SSH signature envelope accepted" >&2
  exit 1
fi
grep -q 'no trusted candidate signature' "$tmp_root/non-ssh.out"

git -C "$tmp_root/work" tag -d v1.2.4 >/dev/null
git -C "$tmp_root/work" tag -s v1.2.4 -m "Release v1.2.4

Harn-Release-Candidate: $release_commit"
git -C "$tmp_root/work" push -q --force origin refs/tags/v1.2.4
if "$verifier" --repo "$tmp_root/work" --tag v1.2.4 >"$tmp_root/wrong-marker.out" 2>&1; then
  echo "FAIL: trusted signature with wrong candidate metadata accepted" >&2
  exit 1
fi
grep -q 'signed candidate metadata' "$tmp_root/wrong-marker.out"

ssh-keygen -q -t ed25519 -N '' -f "$tmp_root/untrusted-key"
git -C "$tmp_root/work" config user.signingkey "$tmp_root/untrusted-key"
git -C "$tmp_root/work" tag -d v1.2.4 >/dev/null
git -C "$tmp_root/work" tag -s v1.2.4 -m "Release v1.2.4

Harn-Release-Candidate: $candidate_commit"
git -C "$tmp_root/work" push -q --force origin refs/tags/v1.2.4
if "$verifier" --repo "$tmp_root/work" --tag v1.2.4 >"$tmp_root/untrusted.out" 2>&1; then
  echo "FAIL: untrusted candidate signer accepted" >&2
  exit 1
fi
grep -q 'no trusted candidate signature' "$tmp_root/untrusted.out"

git -C "$tmp_root/work" switch -q main
git -C "$tmp_root/work" commit --allow-empty -q -m 'not a release squash'
git -C "$tmp_root/work" tag -a v1.2.5 -m 'Release v1.2.5'
git -C "$tmp_root/work" push -q origin main refs/tags/v1.2.5
if "$verifier" --repo "$tmp_root/work" --tag v1.2.5 >"$tmp_root/forged.out" 2>&1; then
  echo "FAIL: a tag whose commit did not introduce its version was accepted" >&2
  exit 1
fi
grep -Eq 'reports workspace version|did not introduce' "$tmp_root/forged.out" || {
  echo "FAIL: forged release rejection did not name the version invariant" >&2
  exit 1
}

git -C "$tmp_root/work" tag v1.2.6 "$release_commit"
git -C "$tmp_root/work" push -q origin refs/tags/v1.2.6
if "$verifier" --repo "$tmp_root/work" --tag v1.2.6 >"$tmp_root/lightweight.out" 2>&1; then
  echo "FAIL: lightweight release tag was accepted" >&2
  exit 1
fi

# Corrected stable publication keeps the version and tags a repaired main
# descendant. The version transition still belongs to the original cut.
printf 'corrected runtime\n' >"$tmp_root/work/repair.txt"
git -C "$tmp_root/work" add repair.txt
git -C "$tmp_root/work" config user.signingkey "$tmp_root/signing-key"
git -C "$tmp_root/work" -c commit.gpgSign=true commit -q -m 'Repair the release runtime'
corrected_commit="$(git -C "$tmp_root/work" rev-parse HEAD)"
git -C "$tmp_root/work" -c "gpg.ssh.allowedSignersFile=$tmp_root/work/.github/release-bot-allowed-signers" \
  verify-commit "$corrected_commit" >/dev/null 2>&1
git -C "$tmp_root/work" tag -d v1.2.3 >/dev/null
git -C "$tmp_root/work" tag -a v1.2.3 -m 'Corrected Release v1.2.3'
git -C "$tmp_root/work" push -q origin main
git -C "$tmp_root/work" push -q --force origin refs/tags/v1.2.3
mkdir -p "$tmp_root/bin"
cp "$root/scripts/tests/fixtures/release_publication_github.py" "$tmp_root/bin/gh"
chmod +x "$tmp_root/bin/gh"
# The real reader and Git history execute; only the GitHub transport is mocked.
# Unexpected requests refuse, including ephemeral run/artifact inventory reads.
# The live published-source check separately verifies real cryptographic bytes.
# shellcheck source=scripts/lib/candidate_archive_contract.sh
source "$root/scripts/lib/candidate_archive_contract.sh"
publication_verify() {
  PATH="$tmp_root/bin:$PATH" GITHUB_REPOSITORY=fixture/harn \
    PUBLICATION_SOURCE="$corrected_commit" PUBLICATION_TARGETS="$(candidate_archive_expected_targets_json)" \
    PUBLICATION_TRACE="$tmp_root/publication-calls" PUBLICATION_CASE="${PUBLICATION_CASE:-valid}" "$@"
}
# A genuinely signed, same-version commit on main is not publication authority.
if PUBLICATION_CASE=unpublished-source publication_verify "$verifier" --repo "$tmp_root/work" --tag v1.2.3 \
  >"$tmp_root/unpublished.out" 2>&1; then
  echo "FAIL: signed same-version main source without publication proof was accepted" >&2
  exit 1
fi
grep -q 'corrected source lacks durable exact-source publication proof' "$tmp_root/unpublished.out"
grep -q '^api repos/fixture/harn/releases/tags/v1.2.3$' "$tmp_root/publication-calls"
corrected_output="$(publication_verify "$verifier" --repo "$tmp_root/work" --tag v1.2.3 --expect-commit "$corrected_commit")"
[[ "$corrected_output" == *"transition=$release_commit"* ]] || {
  echo "FAIL: corrected source did not prove its original version transition: $corrected_output" >&2
  exit 1
}
publication_verify "$tmp_root/release-tools/verify_release_tag_main_ancestry.sh" \
  --repo "$tmp_root/work" --tag v1.2.3 --expect-commit "$corrected_commit" >/dev/null
git -C "$tmp_root/origin.git" update-ref refs/tags/v1.2.3 "$corrected_commit"
publication_verify "$verifier" --repo "$tmp_root/work" --tag v1.2.3 --expect-commit "$corrected_commit" >/dev/null
if "$verifier" --repo "$tmp_root/work" --tag v1.2.3 --expect-commit "$release_commit" \
  >"$tmp_root/corrected-mismatch.out" 2>&1; then
  echo "FAIL: corrected tag was accepted for the original source" >&2
  exit 1
fi
grep -Fq "selects $corrected_commit, not the expected commit $release_commit" "$tmp_root/corrected-mismatch.out"

for publication_case in unsigned-source http-failure draft-release nonpublisher-author \
  nonpublisher-manifest nonpublisher-index nonpublisher-archive wrong-manifest-source \
  wrong-manifest-bytes incomplete-assets wrong-archive-digest incomplete-index \
  attestation-failure empty-attestation wrong-workflow wrong-ref wrong-source wrong-run \
  wrong-attempt wrong-subject wrong-predicate; do
  : >"$tmp_root/publication-calls"
  if PUBLICATION_CASE="$publication_case" publication_verify "$verifier" \
    --repo "$tmp_root/work" --tag v1.2.3 --expect-commit "$corrected_commit" \
    >"$tmp_root/$publication_case.out" 2>&1; then
    echo "FAIL: corrected publication accepted $publication_case" >&2
    exit 1
  fi
  grep -q 'corrected source lacks durable exact-source publication proof' "$tmp_root/$publication_case.out"
  grep -q "^api repos/fixture/harn/git/commits/$corrected_commit$" "$tmp_root/publication-calls"
  case "$publication_case" in
    attestation-failure|empty-attestation|wrong-workflow|wrong-ref|wrong-source|wrong-run|wrong-attempt|wrong-subject|wrong-predicate)
      grep -q '^attestation verify$' "$tmp_root/publication-calls" ;;
  esac
  echo "published correction refused $publication_case"
done
# A refusal cannot poison the same path's next valid read.
publication_verify "$verifier" --repo "$tmp_root/work" --tag v1.2.3 --expect-commit "$corrected_commit" >/dev/null

# A matching stable version with no earlier version transition is not a cut.
git init -q -b main "$tmp_root/no-transition"
git -C "$tmp_root/no-transition" config user.name Test
git -C "$tmp_root/no-transition" config user.email test@example.com
git -C "$tmp_root/no-transition" config commit.gpgSign false
git -C "$tmp_root/no-transition" config tag.gpgSign false
git init -q -b main --bare "$tmp_root/no-transition-origin.git"
git -C "$tmp_root/no-transition" remote add origin "$tmp_root/no-transition-origin.git"
printf '[workspace.package]\nversion = "1.2.7"\n' >"$tmp_root/no-transition/Cargo.toml"
git -C "$tmp_root/no-transition" add Cargo.toml
git -C "$tmp_root/no-transition" commit -q -m bootstrap
git -C "$tmp_root/no-transition" commit --allow-empty -q -m 'Release v1.2.7'
git -C "$tmp_root/no-transition" tag -a v1.2.7 -m 'Release v1.2.7'
git -C "$tmp_root/no-transition" push -q -u origin main refs/tags/v1.2.7
if "$verifier" --repo "$tmp_root/no-transition" --tag v1.2.7 \
  >"$tmp_root/no-transition.out" 2>&1; then
  echo "FAIL: matching version without a proved transition was accepted" >&2
  exit 1
fi
grep -q 'no proved stable version transition' "$tmp_root/no-transition.out"

echo "release_tag_main_ancestry_test: ok"
