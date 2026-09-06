#!/usr/bin/env bash
set -euo pipefail

root="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
verifier="$root/scripts/verify_release_tag_main_ancestry.sh"
tmp_root="$(mktemp -d "${TMPDIR:-/tmp}/harn-release-main-tag-test.XXXXXX")"
trap 'rm -rf "$tmp_root"' EXIT

git init --bare -q "$tmp_root/origin.git"
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

if "$verifier" --repo "$tmp_root/work" --tag v9.9.9 >"$tmp_root/missing.out" 2>&1; then
  echo "FAIL: missing release tag was accepted" >&2
  exit 1
fi
grep -q 'missing or is not an annotated tag' "$tmp_root/missing.out" || {
  echo "FAIL: missing-tag rejection did not name the remote tag invariant" >&2
  exit 1
}

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

echo "release_tag_main_ancestry_test: ok"
