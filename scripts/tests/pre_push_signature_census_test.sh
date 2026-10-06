#!/usr/bin/env bash
set -euo pipefail

root=$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)
hook_root=${HOOK_TEST_SOURCE_ROOT:-$root}
tmp=$(mktemp -d)
trap 'rm -rf "$tmp"' EXIT
work="$tmp/work"
remote="$tmp/remote.git"
real_git=$(command -v git)
zero=0000000000000000000000000000000000000000
git init -b main --bare --quiet "$remote"
git init -b main --quiet "$work"
git -C "$work" config user.name 'Hook test'
git -C "$work" config user.email 'hook@example.com'
git -C "$work" config commit.gpgsign false
git -C "$work" remote add origin "$remote"
printf 'published history\n' > "$work/note.txt"
git -C "$work" add note.txt
git -C "$work" commit --quiet -m 'published unsigned history'
base=$(git -C "$work" rev-parse HEAD)
git -C "$work" push --quiet -u origin main

# Temporary SSH signing proves the real Git status path without host keys.
ssh-keygen -q -t ed25519 -N '' -f "$tmp/key"
git -C "$work" config gpg.format ssh
git -C "$work" config user.signingkey "$tmp/key"
git -C "$work" config commit.gpgsign true
printf 'hook@example.com ' > "$tmp/allowed_signers"
cat "$tmp/key.pub" >> "$tmp/allowed_signers"
git -C "$work" config gpg.ssh.allowedSignersFile "$tmp/allowed_signers"
git -C "$work" checkout --quiet -b good
printf 'signed change\n' >> "$work/note.txt"
git -C "$work" commit --quiet -am 'signed candidate'
good=$(git -C "$work" rev-parse HEAD)
[[ $(git -C "$work" log -1 --format='%G?' "$good") == G ]]
git -C "$work" checkout --quiet -b bad "$base"
printf 'unsigned change\n' >> "$work/note.txt"
git -C "$work" -c commit.gpgsign=false commit --quiet -am 'unsigned candidate'
bad=$(git -C "$work" rev-parse HEAD)
[[ $(git -C "$work" log -1 --format='%G?' "$bad") == N ]]

mkdir -p "$work/.githooks" "$tmp/bin"
cp "$hook_root/.githooks/lib.sh" "$hook_root/.githooks/pre-push" "$work/.githooks/"
mv "$work/.githooks/pre-push" "$work/.githooks/pre-push.production"
cat > "$work/.githooks/pre-push" <<'SH'
#!/bin/sh
# Git prepends its exec directory to PATH when invoking hooks.
export PATH="$HOOK_TEST_BIN:$PATH"
exec "$(dirname "$0")/pre-push.production" "$@"
SH
chmod +x "$work/.githooks/pre-push"
git -C "$work" config core.hooksPath "$work/.githooks"
# Other guards are outside this regression; Git graph, stdin, signatures and
# remote publication remain real. Status overrides only cover rare G? values.
cat > "$tmp/bin/gh" <<'SH'
#!/bin/sh
exit 0
SH
cat > "$tmp/bin/git" <<'SH'
#!/bin/sh
if [ "$1" = log ] && [ "$2" = -1 ] && [ "$3" = '--format=%G?' ]; then
  printf '%s\n' "$4" >> "$SIGNATURE_READS"
  if [ -n "${SIGNATURE_OVERRIDE:-}" ]; then
    printf '%s\n' "$SIGNATURE_OVERRIDE"
    exit 0
  fi
fi
if [ "$1" = rev-list ] && [ "${BREAK_ENUMERATION:-0}" = 1 ]; then exit 73; fi
exec "$REAL_GIT" "$@"
SH
chmod +x "$tmp/bin/gh" "$tmp/bin/git"
export REAL_GIT="$real_git" SIGNATURE_READS="$tmp/reads" HOOK_TEST_BIN="$tmp/bin" PATH="$tmp/bin:$PATH"

refuse_push() {
  : > "$SIGNATURE_READS"
  if git -C "$work" push --quiet origin "$@" > "$tmp/out" 2>&1; then
    echo 'expected production pre-push to refuse' >&2
    cat "$tmp/out" >&2
    exit 1
  fi
}
accept_push() {
  : > "$SIGNATURE_READS"
  git -C "$work" push --quiet origin "$@" > "$tmp/out" 2>&1 || {
    cat "$tmp/out" >&2; exit 1;
  }
}
git -C "$work" checkout --quiet good
refuse_push bad:refs/heads/rejected
grep -Fq "$bad (%G?=N)" "$tmp/out"
[[ -z $(git --git-dir="$remote" rev-parse --verify --quiet refs/heads/rejected || true) ]]
[[ $(cat "$SIGNATURE_READS") == "$bad" ]]
git -C "$work" checkout --quiet bad
accept_push good:refs/heads/accepted
[[ $(git --git-dir="$remote" rev-parse refs/heads/accepted) == "$good" ]]
[[ $(cat "$SIGNATURE_READS") == "$good" ]]

# Overlapping refs are unioned once, including mixed deletions; one unsigned
# candidate refuses the entire publication, irrespective of checked-out HEAD.
git -C "$work" checkout --quiet good
git -C "$work" checkout --quiet -b overlap
printf 'shared unpublished candidate\n' >> "$work/note.txt"
git -C "$work" commit --quiet -am 'shared unpublished candidate'
refuse_push overlap:refs/heads/overlap-one overlap:refs/heads/overlap-two bad:refs/heads/rejected :refs/heads/accepted
[[ $(sort "$SIGNATURE_READS" | uniq -d | wc -l | tr -d ' ') == 0 ]]
[[ $(wc -l < "$SIGNATURE_READS" | tr -d ' ') == 2 ]]
grep -Fq '2 distinct pushed commits' "$tmp/out"
accept_push :refs/heads/accepted
[[ ! -s "$SIGNATURE_READS" ]]
grep -Fq 'deletion-only ref update' "$tmp/out"

# Fresh main history removes old recovery ancestry despite stale upstream.
accept_push good:refs/heads/main
git -C "$work" update-ref refs/remotes/origin/main "$base"
git -C "$work" checkout --quiet -b rebased good
printf 'rebased candidate\n' >> "$work/note.txt"
git -C "$work" commit --quiet -am 'rebased candidate'
rebased=$(git -C "$work" rev-parse HEAD)
accept_push rebased:refs/heads/rebased
[[ $(cat "$SIGNATURE_READS") == "$rebased" ]]
# An unpublished tracking ref must never hide an unsigned candidate.
git -C "$work" update-ref refs/remotes/origin/main "$bad"
refuse_push bad:refs/heads/stale-tracking
grep -Fq "$bad (%G?=N)" "$tmp/out"
git -C "$work" update-ref refs/remotes/origin/main "$good"
refuse_push +bad:refs/heads/rebased
grep -Fq "$bad (%G?=N)" "$tmp/out"
[[ $(git --git-dir="$remote" rev-parse refs/heads/rebased) == "$rebased" ]]

# Commit-bearing annotated tags are peeled; noncommit tags contain no commits.
git -C "$work" -c tag.gpgsign=false tag -a unsigned-tag "$bad" -m 'tag'
refuse_push refs/tags/unsigned-tag
grep -Fq "$bad (%G?=N)" "$tmp/out"
tree=$(git -C "$work" rev-parse "$good^{tree}")
git -C "$work" -c tag.gpgsign=false tag tree-tag "$tree"
accept_push refs/tags/tree-tag
[[ ! -s "$SIGNATURE_READS" ]]

# Deterministically cover every policy status on a real candidate graph.
for status in G U E X Y R; do
  export SIGNATURE_OVERRIDE="$status"
  accept_push bad:refs/heads/status-"$status"
  [[ $(cat "$SIGNATURE_READS") == "$bad" ]]
done
for status in N B invalid; do
  export SIGNATURE_OVERRIDE="$status"
  refuse_push bad:refs/heads/status-"$status"
done
unset SIGNATURE_OVERRIDE
export BREAK_ENUMERATION=1
refuse_push rebased:refs/heads/enumeration-failure
grep -Fq 'revision enumeration failed' "$tmp/out"
unset BREAK_ENUMERATION

# Direct hook input checks preserve malformed/unresolvable failures instead
# of silently interpreting a failed Git read as a measured zero.
hook_input() {
  (cd "$work"; printf '%s\n' "$1" | ./.githooks/pre-push origin "$remote") > "$tmp/out" 2>&1
}
for input in '' 'malformed' \
  "refs/heads/bad $bad refs/heads/bad $zero extra" \
  "refs/heads/bad $bad refs/heads/bad..ref $zero" \
  "(delete) $bad refs/heads/bad $base" \
  "(delete) $zero refs/heads/bad $zero" \
  "refs/heads/missing deadbeefdeadbeefdeadbeefdeadbeefdeadbeef refs/heads/missing $zero"; do
  if hook_input "$input"; then echo 'invalid hook input accepted' >&2; exit 1; fi
  grep -Fq 'cannot census pushed commits' "$tmp/out"
done

# An empty destination cannot exclude the unsigned root from a new ref.
git init -b main --bare --quiet "$tmp/empty.git"
# This call deliberately uses a different destination, with no advertised root.
if (cd "$work"; printf 'refs/heads/good %s refs/heads/good %s\n' "$good" "$zero" |
    ./.githooks/pre-push empty "$tmp/empty.git") > "$tmp/out" 2>&1; then
  echo 'unsigned root was omitted from empty-destination census' >&2; exit 1
fi
grep -Fq "$base (%G?=N)" "$tmp/out"
# Missing server old objects cannot silently remove the pushed candidate.
if hook_input "refs/heads/bad $bad refs/heads/bad deadbeefdeadbeefdeadbeefdeadbeefdeadbeef"; then
  echo 'missing remote history hid an unsigned candidate' >&2; exit 1
fi
grep -Fq 'checking ancestry conservatively' "$tmp/out"
grep -Fq "$bad (%G?=N)" "$tmp/out"
# A freshly advertised default absent locally must remain conservative too.
empty_tree=$(git --git-dir="$tmp/empty.git" mktree </dev/null)
foreign=$(printf 'foreign default\n' | git --git-dir="$tmp/empty.git" \
  -c user.name=Fixture -c user.email=fixture@example.com commit-tree "$empty_tree")
git --git-dir="$tmp/empty.git" update-ref refs/heads/main "$foreign"
if (cd "$work"; printf 'refs/heads/good %s refs/heads/good %s\n' "$good" "$zero" |
    ./.githooks/pre-push empty "$tmp/empty.git") > "$tmp/out" 2>&1; then
  echo 'unavailable advertised history hid an unsigned root' >&2; exit 1
fi
grep -Fq "Advertised history $foreign is unavailable locally" "$tmp/out"
grep -Fq "$base (%G?=N)" "$tmp/out"
if (cd "$work"; printf 'refs/heads/good %s refs/heads/good %s\n' "$good" "$zero" |
    ./.githooks/pre-push absent "$tmp/nonexistent.git") > "$tmp/out" 2>&1; then
  echo 'failed remote discovery accepted' >&2; exit 1
fi
grep -Fq 'destination history unavailable' "$tmp/out"
echo 'pre_push_signature_census_test: ok'
