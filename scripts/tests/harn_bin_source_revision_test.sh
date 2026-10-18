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

git -C "$tmp_root" init --quiet
printf 'original\n' >"$tmp_root/source"
git -C "$tmp_root" add source
git -C "$tmp_root" -c user.name=Test -c user.email=test@example.invalid \
  -c commit.gpgsign=false commit --quiet -m initial
revision="$(git -C "$tmp_root" rev-parse HEAD)"
[[ "$(harn_inferred_source_revision "$tmp_root")" = "$revision" ]] || fail "clean checkout lost its revision"

printf 'changed\n' >"$tmp_root/source"
[[ -z "$(harn_inferred_source_revision "$tmp_root")" ]] || fail "dirty checkout claimed a clean revision"
git -C "$tmp_root" add source
[[ -z "$(harn_inferred_source_revision "$tmp_root")" ]] || fail "staged checkout claimed a clean revision"
git -C "$tmp_root" restore --staged --worktree source
printf 'new\n' >"$tmp_root/untracked"
[[ -z "$(harn_inferred_source_revision "$tmp_root")" ]] || fail "untracked source claimed a clean revision"
[[ -z "$(harn_inferred_source_revision "$archive_root")" ]] || fail "source archive claimed a revision"
echo "harn_bin_source_revision_test: ok"
