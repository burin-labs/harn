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
git -C "$tmp_root/work" remote add origin "$tmp_root/origin.git"
printf '[workspace.package]\nversion = "1.2.2"\n' >"$tmp_root/work/Cargo.toml"
git -C "$tmp_root/work" add Cargo.toml
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
