#!/usr/bin/env bash
# Drive release_publication_overdue.sh against a fixture main and a fake
# GitHub. A stable version left unpublished past its allowance must fail and
# open exactly one tracking issue; anything else must stay quiet, and an
# unreadable release list must fail rather than read as published.
set -euo pipefail

root="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
tmp="$(mktemp -d)"
trap 'rm -rf "$tmp"' EXIT
fail() { echo "FAIL: $*" >&2; exit 1; }

repo="$tmp/repo"
git init -q -b main "$repo"
git -C "$repo" config user.name "Overdue Test"
git -C "$repo" config user.email "overdue-test@example.com"
git -C "$repo" config commit.gpgsign false
commit_version() {
  printf '[workspace.package]\nversion = "%s"\n' "$1" > "$repo/Cargo.toml"
  git -C "$repo" add Cargo.toml
  GIT_COMMITTER_DATE="@$2 +0000" GIT_AUTHOR_DATE="@$2 +0000" \
    git -C "$repo" commit -q -m "$1"
  git -C "$repo" rev-parse HEAD
}
commit_version 1.2.2 1000000 >/dev/null
release_commit="$(commit_version 1.2.3 2000000)"
printf 'later\n' > "$repo/other"
git -C "$repo" add other
GIT_COMMITTER_DATE="@2000100 +0000" git -C "$repo" commit -q -m later
head_sha="$(git -C "$repo" rev-parse HEAD)"

mkdir -p "$tmp/bin"
# FAKE_TAGS: published tag names (JSON array); FAKE_RELEASES_FAIL=1 makes the
# release list unreadable; FAKE_OPEN_ISSUE: an open issue title.
cat > "$tmp/bin/gh" <<'EOF'
#!/usr/bin/env bash
printf '%s\n' "$*" >> "$FAKE_GH_LOG"
case "$1 $2" in
  "api repos/o/r/releases?per_page=50")
    [[ "${FAKE_RELEASES_FAIL:-0}" == 1 ]] && exit 1
    jq -n --argjson tags "$FAKE_TAGS" '[$tags[] | {tag_name: ., draft: false}] + [{tag_name: "v9.9.9", draft: true}]' \
      | jq "${4}" ;;
  "issue list")
    jq -n --arg t "${FAKE_OPEN_ISSUE:-}" 'if $t == "" then [] else [{number: 7, title: $t}] end' \
      | jq -r "${@: -1}" ;;
  "issue create") ;;
  *) echo "unexpected gh $*" >&2; exit 2 ;;
esac
EOF
chmod +x "$tmp/bin/gh"

run() {
  : > "$tmp/gh.log"
  set +e
  env PATH="$tmp/bin:$PATH" FAKE_GH_LOG="$tmp/gh.log" GITHUB_REPOSITORY=o/r \
    HEAD_SHA="$head_sha" SOURCE_DIRECTORY="$repo" "$@" \
    bash "$root/scripts/ci/release_publication_overdue.sh" > "$tmp/out" 2>&1
  status=$?
  set -e
}
old=$((2000000 + 5 * 3600))
young=$((2000000 + 3600))

run VERSION=1.2.3 FAKE_TAGS='["v1.2.3"]' NOW_EPOCH="$old"
[[ $status == 0 ]] || fail "a published version alarmed: $(cat "$tmp/out")"

run VERSION=1.2.3 FAKE_TAGS='["v1.2.2"]' NOW_EPOCH="$young"
[[ $status == 0 ]] || fail "a version inside its allowance alarmed"
grep -q "landed 1h ago" "$tmp/out" || fail "the young unpublished version was not reported"

run VERSION=1.2.3 FAKE_TAGS='["v1.2.2"]' NOW_EPOCH="$old"
[[ $status == 1 ]] || fail "an overdue version passed"
grep -q "$release_commit (5h ago)" "$tmp/out" || fail "the alarm did not name the version commit: $(cat "$tmp/out")"
grep -q '^issue create' "$tmp/gh.log" || fail "the overdue version opened no issue"

run VERSION=1.2.3 FAKE_TAGS='["v1.2.2"]' NOW_EPOCH="$old" FAKE_OPEN_ISSUE="[Release] v1.2.3 is unpublished on main"
[[ $status == 1 ]] || fail "an overdue version with a tracking issue passed"
! grep -q '^issue create' "$tmp/gh.log" || fail "a second tracking issue was opened"

run VERSION=1.2.3 FAKE_TAGS='[]' FAKE_RELEASES_FAIL=1 NOW_EPOCH="$old"
[[ $status == 1 ]] || fail "an unreadable release list read as published"

run VERSION=1.2.4-dev FAKE_TAGS='[]' NOW_EPOCH="$old"
[[ $status == 0 && ! -s "$tmp/gh.log" ]] || fail "a development version was judged"

echo "release_publication_overdue_test: ok"
