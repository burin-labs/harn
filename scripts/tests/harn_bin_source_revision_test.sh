#!/usr/bin/env bash
set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
# shellcheck source=scripts/lib/harn_bin.sh
source "$repo_root/scripts/lib/harn_bin.sh"
tmp_root="$(mktemp -d)"
archive_root="$(mktemp -d)"
trap 'rm -rf "$tmp_root" "$archive_root"' EXIT

fail() {
  echo "harn_bin_source_revision_test: $*" >&2
  exit 1
}

git -C "$tmp_root" init --quiet -b main
printf 'original\n' >"$tmp_root/source"
git -C "$tmp_root" add source
git -C "$tmp_root" -c user.name=Test -c user.email=test@example.invalid \
  -c commit.gpgsign=false commit --quiet -m initial
revision="$(git -C "$tmp_root" rev-parse HEAD)"
[[ "$(harn_inferred_source_revision "$tmp_root")" = "$revision" ]] || fail "clean checkout lost its revision"
harn_require_unchanged_source_revision "$tmp_root" "$revision" || fail "clean source guard refused"

printf 'changed\n' >"$tmp_root/source"
[[ -z "$(harn_inferred_source_revision "$tmp_root")" ]] || fail "dirty checkout claimed a clean revision"
if harn_require_unchanged_source_revision "$tmp_root" "$revision" >/dev/null 2>&1; then
  fail "changed source retained its clean build revision"
fi
git -C "$tmp_root" add source
[[ -z "$(harn_inferred_source_revision "$tmp_root")" ]] || fail "staged checkout claimed a clean revision"
git -C "$tmp_root" restore --staged --worktree source
printf 'new\n' >"$tmp_root/untracked"
[[ -z "$(harn_inferred_source_revision "$tmp_root")" ]] || fail "untracked source claimed a clean revision"
[[ -z "$(harn_inferred_source_revision "$archive_root")" ]] || fail "source archive claimed a revision"
rm -f "$tmp_root/untracked"
git -C "$tmp_root" worktree add --quiet --detach "$archive_root/linked" HEAD
[[ "$(harn_inferred_source_revision "$archive_root/linked")" = "$revision" ]] || fail "linked worktree lost its revision"
mkdir -p "$tmp_root/archive/scripts/lib"
printf '/archive/\n' >>"$tmp_root/.git/info/exclude"
printf '[workspace]\n' >"$tmp_root/archive/Cargo.toml"
cp "$repo_root/scripts/lib/harn_bin.sh" "$tmp_root/archive/scripts/lib/"
cp "$repo_root/scripts/lib/harn_bin_freshness.sh" "$tmp_root/archive/scripts/lib/"
cp "$repo_root/scripts/lib/sha256.sh" "$tmp_root/archive/scripts/lib/"
[[ "$(harn_inferred_source_revision "$tmp_root")" = "$revision" ]] || fail "archive control dirtied the outer checkout"
archive_revision="$(
  cd "$tmp_root/archive"
  source scripts/lib/harn_bin.sh
  harn_inferred_source_revision "$(harn_repo_root)"
)"
[[ -z "$archive_revision" ]] || fail "nested source archive inherited the outer revision"
echo "harn_bin_source_revision_test: ok"
