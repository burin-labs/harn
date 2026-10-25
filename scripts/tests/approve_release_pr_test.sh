#!/usr/bin/env bash
# Tests for scripts/ci/approve_release_pr.sh against fixture pull requests.
#
# `gh` is a stub that serves the fixture JSON and records any approval. The
# decision logic under test is the script's own. Every refusal case changes one
# thing in an otherwise approvable release pull request, so a pass of the
# positive case proves the refusals are not vacuous.
set -euo pipefail

repo_root=$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)
script="$repo_root/scripts/ci/approve_release_pr.sh"
tmp=$(mktemp -d)
trap 'rm -rf "$tmp"' EXIT
mkdir "$tmp/bin"

fail() { echo "FAIL: $*" >&2; exit 1; }

cat > "$tmp/bin/gh" <<'SH'
#!/usr/bin/env bash
set -euo pipefail
args="$*"
jq_program=""
prev=""
for a in "$@"; do
  [[ "$prev" == --jq ]] && jq_program=$a
  prev=$a
done
emit() { if [[ -n "$jq_program" ]]; then jq -r "$jq_program" "$1"; else cat "$1"; fi; }
case "$args" in
  *"-X POST"*"/reviews"*) echo "$args" >> "$FIXTURE/approvals"; echo '{}' ;;
  *graphql*"reviewDecision"*"reviewThreads"*) emit "$FIXTURE/review_state.json" ;;
  *graphql*) emit "$FIXTURE/decision.json" ;;
  *"/releases/latest"*) emit "$FIXTURE/latest.json" ;;
  *"/commits"*) cat "$FIXTURE/commits.json" ;;
  *"/files"*) cat "$FIXTURE/files.json" ;;
  *"/pulls/"*) cat "$FIXTURE/pr.json" ;;
  *) echo "unexpected gh call: $args" >&2; exit 1 ;;
esac
SH
chmod +x "$tmp/bin/gh"

sha=$(printf 'a%.0s' {1..40})

# An approvable release pull request: version stamps, a changelog fold and a
# deleted fragment.
fixture() {
  local dir=$1
  mkdir -p "$dir"
  jq -n --arg sha "$sha" '{state:"open",draft:false,title:"Release v0.10.174",
    base:{ref:"main"},user:{login:"harn-release-bot[bot]",type:"Bot"},
    head:{ref:"release/v0.10.174",sha:$sha,repo:{full_name:"burin-labs/harn"}}}' > "$dir/pr.json"
  echo '[[{"commit":{"verification":{"verified":true}}}]]' > "$dir/commits.json"
  jq -n '[[
    {filename:"Cargo.toml",status:"modified",patch:"@@ -1,2 +1,2 @@\n-version = \"0.10.174-dev\"\n+version = \"0.10.174\"\n"},
    {filename:"Cargo.lock",status:"modified",patch:"@@ -1,3 +1,3 @@\n-version = \"0.10.174-dev\"\n+version = \"0.10.174\"\n"},
    {filename:"docs/src/embedding-rust.md",status:"modified",patch:"@@ -1 +1 @@\n-tag = \"v0.10.173\"\n+tag = \"v0.10.174\"\n"},
    {filename:"CHANGELOG.md",status:"modified",patch:"@@ -1,1 +1,3 @@\n+## v0.10.174\n+- A change.\n "},
    {filename:"changelog.d/1.added.md",status:"removed",patch:"@@ -1 +0,0 @@\n-A change.\n"}
  ]]' > "$dir/files.json"
  echo '{"tag_name":"v0.10.173"}' > "$dir/latest.json"
  echo '{"reviewThreads":{"nodes":[]},"reviews":{"totalCount":0}}' > "$dir/review_state.json"
  echo '{"data":{"repository":{"pullRequest":{"reviewDecision":"APPROVED"}}}}' > "$dir/decision.json"
  # The stub applies the script's own --jq to these.
  jq -n '{data:{repository:{pullRequest:{reviewThreads:{pageInfo:{hasNextPage:false},nodes:[]},reviews:{totalCount:0}}}}}' > "$dir/review_state.json"
}

run() {
  local dir=$1
  rm -f "$dir/approvals"
  FIXTURE=$dir PATH="$tmp/bin:$PATH" GH_TOKEN=x GITHUB_REPOSITORY=burin-labs/harn \
    bash "$script" 7 > "$dir/out" 2>&1
}

# Rewrite one fixture file with a jq program.
edit() {
  local file=$1 program=$2
  jq "$program" "$dir/$file" > "$dir/edit.tmp" && mv "$dir/edit.tmp" "$dir/$file"
}

# Each case is a function that mutates the fixture in $dir.
expect_approved() {
  local name=$1
  dir=$tmp/$name
  fixture "$dir"
  "$name"
  run "$dir" || { cat "$dir/out" >&2; fail "$name: expected approval"; }
  grep -q "commit_id=$sha" "$dir/approvals" || fail "$name: approval not bound to the head SHA"
}

# expect_refused <case> <phrase the refusal must contain> [posted]
# A review that posts but does not flip reviewDecision is the one refusal that
# follows an approval call.
expect_refused() {
  local name=$1 want=$2
  dir=$tmp/$name
  fixture "$dir"
  "$name"
  if run "$dir"; then fail "$name: expected refusal"; fi
  grep -q "$want" "$dir/out" || { cat "$dir/out" >&2; fail "$name: refused for the wrong reason (wanted '$want')"; }
  [[ "${3:-}" == posted || ! -e "$dir/approvals" ]] || fail "$name: approved despite refusing"
}

release() { :; }
development() {
  edit pr.json '.title = "[Release] Start 0.10.174-dev development" | .head.ref = "automation/development-0.10.174-dev"'
  jq -n '[[{filename: "Cargo.toml", status: "modified", patch: "@@ -1 +1 @@\n-version = \"0.10.173\"\n+version = \"0.10.174-dev\"\n"}]]' > "$dir/files.json"
}
# After a retired, never-published version the stable files name the published
# predecessor (0.10.177), not the previous patch, and the release folds a parked
# `## Unreleased` heading.
retired_predecessor() {
  edit pr.json '.title = "Release v0.10.179" | .head.ref = "release/v0.10.179"'
  echo '{"tag_name":"v0.10.177"}' > "$dir/latest.json"
  jq -n '[[
    {filename:"Cargo.toml",status:"modified",patch:"@@ -1,2 +1,2 @@\n-version = \"0.10.179-dev\"\n+version = \"0.10.179\"\n"},
    {filename:"docs/src/embedding-rust.md",status:"modified",patch:"@@ -1 +1 @@\n-tag = \"v0.10.177\"\n+tag = \"v0.10.179\"\n"},
    {filename:"CHANGELOG.md",status:"modified",patch:"@@ -1,3 +1,3 @@\n-## Unreleased\n+## v0.10.179\n+- A change.\n "}
  ]]' > "$dir/files.json"
}
retired_predecessor_unpublished() { retired_predecessor; echo '{"tag_name":"v0.10.173"}' > "$dir/latest.json"; }
fold_without_heading() { retired_predecessor; edit files.json '.[0][2].patch = "@@ -1,2 +1,1 @@\n-## Unreleased\n+- A change.\n "'; }
heading_elsewhere() { retired_predecessor; edit files.json '.[0][2].patch = "@@ -1,4 +1,4 @@\n-## Unreleased\n+- A change.\n ## v0.10.177\n+## v0.10.179\n "'; }
removes_other_heading() { retired_predecessor; edit files.json '.[0][2].patch += "\n-## v0.10.177"'; }
human_author() { edit pr.json '.user.login = "someone" | .user.type = "User"'; }
wrong_branch() { edit pr.json '.head.ref = "feature/x"'; }
title_mismatch() { edit pr.json '.title = "Release v0.10.175"'; }
unsigned_commit() { edit commits.json '.[0][0].commit.verification.verified = false'; }
extra_dependency() { edit files.json '.[0][0].patch += "\n+serde_evil = \"1\""'; }
wrong_version() { edit files.json '.[0][2].patch = "@@ -1 +1 @@\n-tag = \"v0.10.173\"\n+tag = \"v0.10.999\"\n"'; }
binary_file() { edit files.json '.[0][1] |= del(.patch)'; }
changelog_rewrite() { edit files.json '.[0][3].patch += "\n-An old entry."'; }
fragment_kept() { edit files.json '.[0][4].status = "modified"'; }
changelog_no_patch() { edit files.json '.[0][3] |= del(.patch)'; }
reordered_lines() { edit files.json '.[0][0].patch = "@@ -1,2 +1,2 @@\n-step_a\n-version = \"0.10.174-dev\"\n+version = \"0.10.174\"\n+step_a\n"'; }
renamed_file() { edit files.json '.[0][0].status = "renamed" | .[0][0].previous_filename = "Cargo.old.toml"'; }
thread_overflow() { edit review_state.json '.data.repository.pullRequest.reviewThreads.pageInfo.hasNextPage = true'; }
open_thread() { edit review_state.json '.data.repository.pullRequest.reviewThreads.nodes = [{isResolved: false}]'; }
not_counted() { edit decision.json '.data.repository.pullRequest.reviewDecision = "REVIEW_REQUIRED"'; }

expect_approved release
expect_approved development
expect_approved retired_predecessor
expect_refused retired_predecessor_unpublished 'changes more than the version'
expect_refused fold_without_heading 'without renaming it in place to v0.10.179'
expect_refused heading_elsewhere 'without renaming it in place to v0.10.179'
expect_refused removes_other_heading 'removes lines'
expect_refused human_author 'not harn-release-bot'
expect_refused wrong_branch 'not a release or development-bump branch'
expect_refused title_mismatch 'does not match branch'
expect_refused unsigned_commit 'GitHub-verified'
expect_refused extra_dependency 'changes more than the version'
expect_refused wrong_version 'changes more than the version'
expect_refused binary_file 'no textual patch'
expect_refused changelog_rewrite 'removes lines'
expect_refused fragment_kept 'not a deletion'
expect_refused changelog_no_patch 'no textual patch'
expect_refused reordered_lines 'changes more than the version'
expect_refused renamed_file 'is not an in-place edit'
expect_refused thread_overflow 'more threads than one page'
expect_refused open_thread 'open review thread'
expect_refused not_counted 'does not count' posted

echo "approve_release_pr_test: ok"
